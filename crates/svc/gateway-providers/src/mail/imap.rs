//! The IMAP client (RFC 9051, and 3501 servers): what `ImapLink` needs and
//! nothing more. Sign in, list folders, open one (with CONDSTORE and
//! QRESYNC where the server has them, RFC 7162), search and fetch by UID,
//! append (UIDPLUS, RFC 4315), move (RFC 6851) and IDLE (RFC 2177).
//!
//! Responses are read whole, literals included, and parsed into
//! [`Value`]s. Everything the server says is untrusted: sizes are capped,
//! and its text reaches people only through [`tidy`].

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use glass_managed::mail::{Account, LinkError, RemoteFolder, Role};

use super::wire::{Conn, Dialer, IO_TIMEOUT, io_error, tidy};

/// The largest literal we accept: a whole message at the sync's cap, with
/// room to spare.
const MAX_LITERAL: usize = 64 * 1024 * 1024;
/// The most bytes one response may carry, literals included.
const MAX_RESPONSE: usize = 96 * 1024 * 1024;

/// A parsed piece of a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Atom(String),
    /// A quoted string or a literal.
    Str(Vec<u8>),
    Nil,
    List(Vec<Value>),
}

impl Value {
    pub(crate) fn atom(&self) -> Option<&str> {
        match self {
            Value::Atom(a) => Some(a),
            _ => None,
        }
    }

    fn is(&self, word: &str) -> bool {
        self.atom().is_some_and(|a| a.eq_ignore_ascii_case(word))
    }

    pub(crate) fn number<T: std::str::FromStr>(&self) -> Option<T> {
        self.atom()?.parse().ok()
    }

    /// A string's text, from an atom, a quoted string or a literal.
    fn text(&self) -> Option<String> {
        match self {
            Value::Atom(a) => Some(a.clone()),
            Value::Str(s) => Some(String::from_utf8_lossy(s).into_owned()),
            _ => None,
        }
    }
}

/// One FETCH response's items, as (name, value) pairs.
pub(crate) type Fetched = Vec<(String, Value)>;

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Done {
    pub(crate) ok: bool,
    /// The response code, such as `APPENDUID 38505 3955`.
    pub(crate) code: Vec<Value>,
    pub(crate) text: String,
}

/// One untagged response: `* <values>`, with a status response's code and
/// text kept apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Untagged {
    pub(crate) values: Vec<Value>,
    pub(crate) code: Vec<Value>,
}

/// What opening a folder said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Opened {
    pub(crate) uid_validity: u32,
    pub(crate) uid_next: u32,
    pub(crate) exists: u32,
    pub(crate) highest_modseq: Option<u64>,
    /// UIDs gone since the state QRESYNC was given.
    pub(crate) vanished: Vec<u32>,
    /// Whether QRESYNC answered for the state it was given.
    pub(crate) resynced: bool,
}

pub(crate) struct Session {
    conn: Conn,
    tag: u32,
    capabilities: BTreeSet<String>,
    qresync: bool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("conn", &self.conn)
            .finish_non_exhaustive()
    }
}

fn refused(host: &str, doing: &str, done: &Done) -> LinkError {
    LinkError::Refused(format!("{host} refused {doing}: {}", done.text))
}

impl Session {
    /// Connects and signs in as `account`.
    pub(crate) fn open(dialer: &Dialer, account: &Account) -> Result<Session, LinkError> {
        let conn = dialer.dial(&account.incoming)?;
        let mut session = Session {
            conn,
            tag: 0,
            capabilities: BTreeSet::new(),
            qresync: false,
        };
        let greeting = session.read_response()?;
        let greeting = match greeting {
            Response::Untagged(u) => u,
            _ => return Err(session.garbled("its greeting")),
        };
        if greeting.values.first().is_some_and(|v| v.is("BYE")) {
            return Err(LinkError::Unreachable(format!(
                "{} isn't taking connections now",
                session.conn.host
            )));
        }
        session.note_capabilities(&greeting.code);
        if !session.conn.is_tls() {
            if session.capabilities.is_empty() {
                session.capability()?;
            }
            if !session.has("STARTTLS") {
                return Err(LinkError::Refused(format!(
                    "{} doesn't offer STARTTLS, so mail can't be read from it securely",
                    session.conn.host
                )));
            }
            let done = session.command("STARTTLS", |_| Ok(()))?;
            if !done.ok {
                return Err(refused(&session.conn.host.clone(), "STARTTLS", &done));
            }
            session.conn.start_tls(dialer)?;
            session.capabilities.clear();
        }
        if session.capabilities.is_empty() {
            session.capability()?;
        }
        // LOGIN takes quoted ASCII; anything else (a password with ü, a
        // server that disables LOGIN) signs in with AUTHENTICATE PLAIN,
        // whose base64 carries any bytes.
        let done = match (quoted(&account.user), quoted(account.secret.expose())) {
            (Ok(user), Ok(secret)) if !session.has("LOGINDISABLED") => {
                session.command(&format!("LOGIN {user} {secret}"), |_| Ok(()))?
            }
            _ if session.has("AUTH=PLAIN") => session.authenticate_plain(account)?,
            _ if session.has("LOGINDISABLED") => {
                return Err(LinkError::Refused(format!(
                    "{} doesn't allow signing in with a password",
                    session.conn.host
                )));
            }
            _ => {
                return Err(LinkError::SignIn(
                    "the user name or password has characters this mail server can't be sent \
                     in a sign-in"
                        .into(),
                ));
            }
        };
        if !done.ok {
            return Err(LinkError::SignIn(format!(
                "{} didn't accept the password for {}: {}",
                session.conn.host, account.user, done.text
            )));
        }
        // Capabilities can change once signed in.
        if !done.code.first().is_some_and(|v| v.is("CAPABILITY")) {
            session.capabilities.clear();
            session.capability()?;
        } else {
            session.capabilities.clear();
            session.note_capabilities(&done.code);
        }
        if session.has("QRESYNC") {
            let done = session.command("ENABLE QRESYNC", |_| Ok(()))?;
            session.qresync = done.ok;
        }
        Ok(session)
    }

    /// SASL PLAIN (RFC 4616): in the command itself where the server takes
    /// that (SASL-IR, RFC 4959), else after its go-ahead.
    fn authenticate_plain(&mut self, account: &Account) -> Result<Done, LinkError> {
        let mut plain = Vec::new();
        plain.push(0);
        plain.extend_from_slice(account.user.as_bytes());
        plain.push(0);
        plain.extend_from_slice(account.secret.expose().as_bytes());
        let encoded = glass_managed::base64::encode(&plain);
        if self.has("SASL-IR") {
            return self.command(&format!("AUTHENTICATE PLAIN {encoded}"), |_| Ok(()));
        }
        let tag = self.next_tag();
        self.conn
            .write_all(format!("{tag} AUTHENTICATE PLAIN\r\n").as_bytes())?;
        match self.read_response()? {
            Response::Continue(_) => {}
            Response::Tagged { tag: t, done } if t == tag => return Ok(done),
            _ => return Err(self.garbled("an answer to AUTHENTICATE")),
        }
        self.conn.write_all(format!("{encoded}\r\n").as_bytes())?;
        self.finish(&tag, |_| Ok(()))
    }

    pub(crate) fn has(&self, capability: &str) -> bool {
        self.capabilities.contains(&capability.to_ascii_uppercase())
    }

    fn note_capabilities(&mut self, values: &[Value]) {
        let mut values = values.iter();
        if values.next().is_some_and(|v| v.is("CAPABILITY")) {
            for v in values {
                if let Some(a) = v.atom() {
                    self.capabilities.insert(a.to_ascii_uppercase());
                }
            }
        }
    }

    fn capability(&mut self) -> Result<(), LinkError> {
        let mut found = Vec::new();
        self.command("CAPABILITY", |u| {
            if u.values.first().is_some_and(|v| v.is("CAPABILITY")) {
                found.push(u.values.clone());
            }
            Ok(())
        })?;
        for values in found {
            self.note_capabilities(&values);
        }
        Ok(())
    }

    fn garbled(&self, what: &str) -> LinkError {
        LinkError::Refused(format!("{} sent {what} we couldn't read", self.conn.host))
    }

    fn next_tag(&mut self) -> String {
        self.tag += 1;
        format!("g{}", self.tag)
    }

    /// Sends `command` and reads to its end, handing each untagged
    /// response to `untagged`.
    pub(crate) fn command(
        &mut self,
        command: &str,
        untagged: impl FnMut(&Untagged) -> Result<(), LinkError>,
    ) -> Result<Done, LinkError> {
        let tag = self.next_tag();
        self.conn
            .write_all(format!("{tag} {command}\r\n").as_bytes())?;
        self.finish(&tag, untagged)
    }

    fn finish(
        &mut self,
        tag: &str,
        mut untagged: impl FnMut(&Untagged) -> Result<(), LinkError>,
    ) -> Result<Done, LinkError> {
        loop {
            match self.read_response()? {
                Response::Untagged(u) => {
                    if u.values.first().is_some_and(|v| v.is("BYE")) {
                        // The tagged reply may still follow (LOGOUT).
                        continue;
                    }
                    untagged(&u)?;
                }
                Response::Tagged { tag: t, done } if t == tag => return Ok(done),
                Response::Tagged { .. } => return Err(self.garbled("a reply to another command")),
                Response::Continue(_) => return Err(self.garbled("an unexpected go-ahead")),
            }
        }
    }

    /// Reads one response, its literals included.
    fn read_response(&mut self) -> Result<Response, LinkError> {
        let mut whole = Vec::new();
        loop {
            let line = self.conn.read_line()?;
            whole.extend_from_slice(&line);
            match literal_length(&line) {
                Some(n) if n <= MAX_LITERAL && whole.len() + n <= MAX_RESPONSE => {
                    whole.extend_from_slice(b"\r\n");
                    let bytes = self.conn.read_exact(n)?;
                    whole.extend_from_slice(&bytes);
                }
                Some(_) => {
                    return Err(LinkError::Refused(format!(
                        "{} sent more than we take in one response",
                        self.conn.host
                    )));
                }
                None => break,
            }
        }
        parse_response(&whole).ok_or_else(|| self.garbled("a response"))
    }

    /// Every folder, with the role each plays.
    pub(crate) fn folders(&mut self) -> Result<Vec<RemoteFolder>, LinkError> {
        let mut folders = Vec::new();
        let done = self.command("LIST \"\" \"*\"", |u| {
            if let [kind, Value::List(attributes), _delimiter, name, ..] = u.values.as_slice()
                && kind.is("LIST")
                && let Some(name) = name.text()
            {
                let attributes: Vec<String> = attributes.iter().filter_map(Value::text).collect();
                if !attributes
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case("\\Noselect"))
                {
                    let role = Role::of(&name, &attributes);
                    folders.push(RemoteFolder { name, role });
                }
            }
            Ok(())
        })?;
        if !done.ok {
            return Err(refused(&self.conn.host, "listing folders", &done));
        }
        Ok(folders)
    }

    /// Opens `folder`, read only (EXAMINE) or to change it (SELECT). With
    /// a known state and QRESYNC, the server says what vanished since.
    pub(crate) fn open_folder(
        &mut self,
        folder: &str,
        write: bool,
        since: Option<(u32, u64)>,
    ) -> Result<Opened, LinkError> {
        let verb = if write { "SELECT" } else { "EXAMINE" };
        let params = match since {
            Some((validity, modseq)) if self.qresync && modseq > 0 => {
                format!(" (QRESYNC ({validity} {modseq}))")
            }
            _ if self.has("CONDSTORE") => " (CONDSTORE)".into(),
            _ => String::new(),
        };
        let resyncing = params.contains("QRESYNC");
        let mut opened = Opened::default();
        let done = self.command(&format!("{verb} {}{params}", quoted(folder)?), |u| {
            match (u.values.first(), u.values.get(1)) {
                (Some(n), Some(kind)) if kind.is("EXISTS") => {
                    opened.exists = n.number().unwrap_or_default();
                }
                (Some(kind), _) if kind.is("VANISHED") => {
                    let set = u.values.iter().skip(1).find_map(Value::atom);
                    if let Some(set) = set {
                        opened.vanished.extend(uid_set(set));
                    }
                }
                (Some(kind), _) if kind.is("OK") => note_code(&u.code, &mut opened),
                _ => {}
            }
            Ok(())
        })?;
        if !done.ok {
            return Err(refused(
                &self.conn.host,
                &format!("opening {folder}"),
                &done,
            ));
        }
        note_code(&done.code, &mut opened);
        opened.resynced = resyncing && since.is_some_and(|(v, _)| v == opened.uid_validity);
        Ok(opened)
    }

    /// UIDs in the open folder: all of them, or those from `from` on.
    pub(crate) fn search(&mut self, from: Option<u32>) -> Result<Vec<u32>, LinkError> {
        let criteria = match from {
            Some(from) => format!("UID {from}:*"),
            None => "ALL".into(),
        };
        let mut uids = self.search_for(&criteria)?;
        // `n:*` always includes the newest message, even below `n`.
        if let Some(from) = from {
            uids.retain(|u| *u >= from);
        }
        Ok(uids)
    }

    /// UIDs in the open folder the server has as unread: usually far fewer
    /// than those read (ADR 0050).
    pub(crate) fn unseen(&mut self) -> Result<Vec<u32>, LinkError> {
        self.search_for("UNSEEN")
    }

    fn search_for(&mut self, criteria: &str) -> Result<Vec<u32>, LinkError> {
        let mut uids = Vec::new();
        let done = self.command(&format!("UID SEARCH {criteria}"), |u| {
            if u.values.first().is_some_and(|v| v.is("SEARCH")) {
                uids.extend(u.values.iter().skip(1).filter_map(Value::number::<u32>));
            }
            Ok(())
        })?;
        if !done.ok {
            return Err(refused(&self.conn.host, "searching", &done));
        }
        uids.sort_unstable();
        uids.dedup();
        Ok(uids)
    }

    /// Marks `uids` in the folder open to change (SELECT) read or unread.
    pub(crate) fn store_seen(&mut self, uids: &[u32], seen: bool) -> Result<(), LinkError> {
        if uids.is_empty() {
            return Ok(());
        }
        let sign = if seen { '+' } else { '-' };
        let done = self.command(
            &format!("UID STORE {} {sign}FLAGS.SILENT (\\Seen)", set_of(uids)),
            |_| Ok(()),
        )?;
        if !done.ok {
            let doing = if seen {
                "marking read"
            } else {
                "marking unread"
            };
            return Err(refused(&self.conn.host, doing, &done));
        }
        Ok(())
    }

    /// `items` for each of `uids`: each FETCH response's pairs, by UID.
    pub(crate) fn fetch(
        &mut self,
        uids: &[u32],
        items: &str,
    ) -> Result<Vec<(u32, Fetched)>, LinkError> {
        let mut found = Vec::new();
        if uids.is_empty() {
            return Ok(found);
        }
        let done = self.command(&format!("UID FETCH {} ({items})", set_of(uids)), |u| {
            if let [_, kind, Value::List(pairs)] = u.values.as_slice()
                && kind.is("FETCH")
            {
                let pairs: Vec<(String, Value)> = pairs
                    .chunks(2)
                    .filter_map(|pair| match pair {
                        [Value::Atom(k), v] => Some((k.to_ascii_uppercase(), v.clone())),
                        _ => None,
                    })
                    .collect();
                let uid = pairs
                    .iter()
                    .find(|(k, _)| k == "UID")
                    .and_then(|(_, v)| v.number::<u32>());
                if let Some(uid) = uid {
                    found.push((uid, pairs));
                }
            }
            Ok(())
        })?;
        if !done.ok {
            return Err(refused(&self.conn.host, "fetching messages", &done));
        }
        Ok(found)
    }

    /// Files `message` in `folder` as read; its UID there, if the server
    /// says.
    pub(crate) fn append(
        &mut self,
        folder: &str,
        message: &[u8],
    ) -> Result<Option<u32>, LinkError> {
        let tag = self.next_tag();
        self.conn.write_all(
            format!(
                "{tag} APPEND {} (\\Seen) {{{}}}\r\n",
                quoted(folder)?,
                message.len()
            )
            .as_bytes(),
        )?;
        // Wait for the go-ahead (or a refusal) before the message.
        match self.read_response()? {
            Response::Continue(_) => {}
            Response::Tagged { tag: t, done } if t == tag => {
                return Err(refused(
                    &self.conn.host,
                    &format!("filing in {folder}"),
                    &done,
                ));
            }
            _ => return Err(self.garbled("an answer to APPEND")),
        }
        self.conn.write_all(message)?;
        self.conn.write_all(b"\r\n")?;
        let done = self.finish(&tag, |_| Ok(()))?;
        if !done.ok {
            return Err(refused(
                &self.conn.host,
                &format!("filing in {folder}"),
                &done,
            ));
        }
        Ok(uid_from_code(&done.code, "APPENDUID", 2))
    }

    /// Moves `uid` from the open folder to `to`; its UID there, if the
    /// server says. Without MOVE: copy, mark deleted, expunge that one.
    pub(crate) fn move_to(&mut self, uid: u32, to: &str) -> Result<Option<u32>, LinkError> {
        let mut new = None;
        if self.has("MOVE") {
            let done = self.command(&format!("UID MOVE {uid} {}", quoted(to)?), |u| {
                if new.is_none() {
                    new = uid_from_code(&u.code, "COPYUID", 3);
                }
                Ok(())
            })?;
            if !done.ok {
                return Err(refused(&self.conn.host, &format!("moving to {to}"), &done));
            }
            return Ok(new.or_else(|| uid_from_code(&done.code, "COPYUID", 3)));
        }
        let done = self.command(&format!("UID COPY {uid} {}", quoted(to)?), |_| Ok(()))?;
        if !done.ok {
            return Err(refused(&self.conn.host, &format!("copying to {to}"), &done));
        }
        new = uid_from_code(&done.code, "COPYUID", 3);
        let flagged = self.command(
            &format!("UID STORE {uid} +FLAGS.SILENT (\\Deleted)"),
            |_| Ok(()),
        )?;
        if !flagged.ok {
            return Err(refused(&self.conn.host, "removing the original", &flagged));
        }
        let expunge = if self.has("UIDPLUS") {
            format!("UID EXPUNGE {uid}")
        } else {
            "EXPUNGE".into()
        };
        let done = self.command(&expunge, |_| Ok(()))?;
        if !done.ok {
            return Err(refused(&self.conn.host, "removing the original", &done));
        }
        Ok(new)
    }

    /// Waits in IDLE on the open folder until it changes or `timeout`
    /// passes; whether it changed.
    pub(crate) fn idle(&mut self, timeout: Duration) -> Result<bool, LinkError> {
        if !self.has("IDLE") {
            std::thread::sleep(timeout);
            return Ok(false);
        }
        let tag = self.next_tag();
        self.conn.write_all(format!("{tag} IDLE\r\n").as_bytes())?;
        match self.read_response()? {
            Response::Continue(_) => {}
            Response::Tagged { done, .. } => {
                return Err(refused(&self.conn.host, "waiting for mail", &done));
            }
            Response::Untagged(_) => return Err(self.garbled("an answer to IDLE")),
        }
        let deadline = Instant::now() + timeout;
        let mut changed = false;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            self.conn.set_read_timeout(left)?;
            match self.conn.read_line_io() {
                Ok(line) => {
                    let text = String::from_utf8_lossy(&line).to_ascii_uppercase();
                    if text.starts_with("* ")
                        && ["EXISTS", "EXPUNGE", "VANISHED", "RECENT"]
                            .iter()
                            .any(|w| text.contains(w))
                    {
                        changed = true;
                        break;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(io_error(&self.conn.host, &e)),
            }
        }
        self.conn.set_read_timeout(IO_TIMEOUT)?;
        self.conn.write_all(b"DONE\r\n")?;
        let done = self.finish(&tag, |_| Ok(()))?;
        if !done.ok {
            return Err(refused(&self.conn.host, "waiting for mail", &done));
        }
        Ok(changed)
    }

    /// Signs out; best effort.
    pub(crate) fn logout(mut self) {
        let _ = self.command("LOGOUT", |_| Ok(()));
    }
}

fn note_code(code: &[Value], opened: &mut Opened) {
    match code {
        [kind, n, ..] if kind.is("UIDVALIDITY") => opened.uid_validity = n.number().unwrap_or(0),
        [kind, n, ..] if kind.is("UIDNEXT") => opened.uid_next = n.number().unwrap_or(0),
        [kind, n, ..] if kind.is("HIGHESTMODSEQ") => opened.highest_modseq = n.number(),
        _ => {}
    }
}

/// `APPENDUID <validity> <uid>` (index 2) or `COPYUID <validity> <from>
/// <to>` (index 3), for one message.
fn uid_from_code(code: &[Value], kind: &str, index: usize) -> Option<u32> {
    if !code.first().is_some_and(|v| v.is(kind)) {
        return None;
    }
    let set = code.get(index)?.atom()?;
    let uids = uid_set(set);
    (uids.len() == 1).then(|| uids[0])
}

/// `1:3,7,9:10` → 1, 2, 3, 7, 9, 10. At most 100,000.
pub(crate) fn uid_set(set: &str) -> Vec<u32> {
    let mut uids = Vec::new();
    for part in set.split(',') {
        let (a, b) = part.split_once(':').unwrap_or((part, part));
        let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) else {
            continue;
        };
        let (lo, hi) = (a.min(b), a.max(b));
        if (hi - lo) as usize + uids.len() > 100_000 {
            break;
        }
        uids.extend(lo..=hi);
    }
    uids
}

/// 1, 2, 3, 7 → `1:3,7`.
fn set_of(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}:{end}")
        });
        i += 1;
    }
    parts.join(",")
}

/// A quoted string; IMAP's quoted strings can't carry line breaks or NUL.
pub(crate) fn quoted(text: &str) -> Result<String, LinkError> {
    if text.contains(['\r', '\n', '\0']) || !text.is_ascii() {
        return Err(LinkError::Refused(format!(
            "{text:?} can't be named to the server"
        )));
    }
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// A sign-in name or password: quoted when it can be, else a synchronizing
/// literal isn't worth it here, so it is refused with a reason.
/// `{123}` or `{123+}` at a line's end: the literal that follows.
fn literal_length(line: &[u8]) -> Option<usize> {
    let line = line.strip_suffix(b"}")?;
    let open = line.iter().rposition(|b| *b == b'{')?;
    let digits = &line[open + 1..];
    let digits = digits.strip_suffix(b"+").unwrap_or(digits);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) || digits.len() > 10 {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Response {
    Untagged(Untagged),
    Tagged { tag: String, done: Done },
    Continue(String),
}

fn parse_response(bytes: &[u8]) -> Option<Response> {
    if let Some(rest) = bytes.strip_prefix(b"+") {
        return Some(Response::Continue(tidy(rest)));
    }
    let mut p = Parser { bytes, at: 0 };
    let tag = p.atom_text()?;
    p.space();
    if tag == "*" {
        // A status response keeps its code and text apart from its words.
        let start = p.at;
        let word = p.atom_text().unwrap_or_default().to_ascii_uppercase();
        if matches!(word.as_str(), "OK" | "NO" | "BAD" | "BYE" | "PREAUTH") {
            p.space();
            let code = p.code()?;
            return Some(Response::Untagged(Untagged {
                values: vec![Value::Atom(word)],
                code,
            }));
        }
        p.at = start;
        let values = p.values(None)?;
        // The greeting's CAPABILITY can come as a plain response too.
        return Some(Response::Untagged(Untagged {
            values,
            code: Vec::new(),
        }));
    }
    let status = p.atom_text()?.to_ascii_uppercase();
    p.space();
    let code = p.code()?;
    let text = tidy(&bytes[p.at.min(bytes.len())..]);
    Some(Response::Tagged {
        tag,
        done: Done {
            ok: status == "OK",
            code,
            text,
        },
    })
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn space(&mut self) {
        while self.peek() == Some(b' ') {
            self.at += 1;
        }
    }

    /// `[...]` at a status response's start, as values; none if absent.
    fn code(&mut self) -> Option<Vec<Value>> {
        if self.peek() != Some(b'[') {
            return Some(Vec::new());
        }
        self.at += 1;
        let values = self.values(Some(b']'))?;
        self.space();
        Some(values)
    }

    fn atom_text(&mut self) -> Option<String> {
        let start = self.at;
        let mut depth = 0usize;
        while let Some(b) = self.peek() {
            match b {
                b'[' => depth += 1,
                b']' if depth > 0 => depth -= 1,
                b']' => break,
                b'<' if depth == 0 && self.at > start && self.bytes[self.at - 1] == b']' => {
                    // `BODY[]<0>`: a partial fetch's origin.
                    while let Some(c) = self.peek() {
                        self.at += 1;
                        if c == b'>' {
                            break;
                        }
                    }
                    continue;
                }
                b' ' | b'(' | b')' | b'{' | b'"' | b'\r' | b'\n' if depth == 0 => break,
                _ if b < 0x20 => return None,
                _ => {}
            }
            self.at += 1;
        }
        (self.at > start).then(|| String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned())
    }

    /// Values up to `close` (or the end).
    fn values(&mut self, close: Option<u8>) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        loop {
            self.space();
            match self.peek() {
                None => return close.is_none().then_some(out),
                Some(b) if Some(b) == close => {
                    self.at += 1;
                    return Some(out);
                }
                Some(b'(') => {
                    self.at += 1;
                    out.push(Value::List(self.values(Some(b')'))?));
                }
                Some(b'"') => out.push(Value::Str(self.quoted()?)),
                Some(b'{') => out.push(Value::Str(self.literal()?)),
                Some(b'~') if self.bytes.get(self.at + 1) == Some(&b'{') => {
                    self.at += 1;
                    out.push(Value::Str(self.literal()?));
                }
                Some(b')' | b']') => return None,
                Some(_) => {
                    let atom = self.atom_text()?;
                    out.push(if atom.eq_ignore_ascii_case("NIL") {
                        Value::Nil
                    } else {
                        Value::Atom(atom)
                    });
                }
            }
        }
    }

    fn quoted(&mut self) -> Option<Vec<u8>> {
        self.at += 1;
        let mut out = Vec::new();
        loop {
            let b = self.peek()?;
            self.at += 1;
            match b {
                b'"' => return Some(out),
                b'\\' => {
                    out.push(self.peek()?);
                    self.at += 1;
                }
                _ => out.push(b),
            }
        }
    }

    fn literal(&mut self) -> Option<Vec<u8>> {
        let close = self.bytes[self.at..].iter().position(|b| *b == b'}')? + self.at;
        let digits = std::str::from_utf8(&self.bytes[self.at + 1..close]).ok()?;
        let n: usize = digits.trim_end_matches('+').parse().ok()?;
        // `}` then CRLF, then the bytes.
        let start = close + 3;
        let end = start.checked_add(n)?;
        if self.bytes.get(close + 1..start)? != b"\r\n" || end > self.bytes.len() {
            return None;
        }
        self.at = end;
        Some(self.bytes[start..end].to_vec())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_fetch_with_a_literal_body_parses() {
        let raw = b"* 3 FETCH (UID 42 RFC822.SIZE 11 BODY[] {11}\r\nHello\r\nyou!)";
        let Some(Response::Untagged(u)) = parse_response(raw) else {
            panic!("untagged");
        };
        let Value::List(pairs) = &u.values[2] else {
            panic!("list");
        };
        assert_eq!(pairs[0], Value::Atom("UID".into()));
        assert_eq!(pairs[4], Value::Atom("BODY[]".into()));
        assert_eq!(pairs[5], Value::Str(b"Hello\r\nyou!".to_vec()));
    }

    #[test]
    fn status_responses_keep_their_codes() {
        let Some(Response::Untagged(u)) =
            parse_response(b"* OK [UIDVALIDITY 3857529045] UIDs valid")
        else {
            panic!("untagged");
        };
        assert_eq!(
            u.code,
            [
                Value::Atom("UIDVALIDITY".into()),
                Value::Atom("3857529045".into())
            ]
        );
        let Some(Response::Tagged { tag, done }) =
            parse_response(b"g4 OK [APPENDUID 38505 3955] APPEND completed")
        else {
            panic!("tagged");
        };
        assert_eq!(tag, "g4");
        assert!(done.ok);
        assert_eq!(uid_from_code(&done.code, "APPENDUID", 2), Some(3955));
        let Some(Response::Tagged { done, .. }) =
            parse_response(b"g5 NO [AUTHENTICATIONFAILED] it's \"wrong\" (sorry")
        else {
            panic!("tagged");
        };
        assert!(!done.ok);
        assert_eq!(done.text, "it's \"wrong\" (sorry");
    }

    #[test]
    fn list_responses_name_folders_with_their_attributes() {
        let Some(Response::Untagged(u)) =
            parse_response(b"* LIST (\\HasNoChildren \\Junk) \"/\" \"Junk Mail\"")
        else {
            panic!("untagged");
        };
        assert_eq!(u.values[3], Value::Str(b"Junk Mail".to_vec()));
        assert_eq!(literal_length(b"* LIST () \"/\" {5}"), Some(5));
        assert_eq!(literal_length(b"g1 APPEND x {12+}"), Some(12));
        assert_eq!(literal_length(b"* OK fine"), None);
    }

    #[test]
    fn uid_sets_go_both_ways() {
        assert_eq!(uid_set("1:3,7,10:9"), [1, 2, 3, 7, 9, 10]);
        assert_eq!(set_of(&[7, 1, 2, 3, 9, 10]), "1:3,7,9:10");
        assert!(quoted("a\r\nb").is_err());
        assert_eq!(quoted(r#"say "hi"\"#).unwrap(), r#""say \"hi\"\\""#);
    }
}
