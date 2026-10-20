//! Mail servers (ADR 0037, EM0): [`ImapLink`] reads a person's mailbox
//! over IMAP and sends through SMTP submission, both in TLS verified
//! against the base system's trust store. It serves `[mail] service =
//! "imap"`, and later Glasshouse Mail's cells. glassd reaches only the
//! hosts IT's signed policy names; agents never reach them at all (their
//! jails have no network), only the mail actions.
//!
//! Each request signs in afresh with the [`Account`] it carries, and signs
//! out: the link keeps no session and no secret.

mod imap;
mod smtp;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;
mod wire;

use std::time::Duration;

use glass_managed::mail::{
    Account, FolderState, FolderSync, LinkError, MailLink, Outgoing, RemoteFolder, RemoteMessage,
    Submitted, SyncLimits,
};

use imap::{Session, Value};
use smtp::Submission;
use wire::Dialer;

/// How many messages one FETCH asks for.
const FETCH_CHUNK: usize = 50;

/// Any IMAP and SMTP submission service.
#[derive(Debug, Clone)]
pub struct ImapLink {
    dialer: Dialer,
}

impl Default for ImapLink {
    fn default() -> Self {
        Self::new()
    }
}

impl ImapLink {
    /// Verifying servers against the system trust store, by the names
    /// policy gives them.
    pub fn new() -> Self {
        Self {
            dialer: Dialer::new(&[], Vec::new()),
        }
    }

    /// Also trusting `pem` and reaching `host` at `ip`, still checking the
    /// certificate against `host`: for a test server (`mox localserve`),
    /// never for anything policy names.
    pub fn for_test_server(pem: &[u8], host: &str, ip: std::net::IpAddr) -> Self {
        Self {
            dialer: Dialer::new(&[pem.to_vec()], vec![(host.to_owned(), ip)]),
        }
    }

    fn session<T>(
        &self,
        account: &Account,
        work: impl FnOnce(&mut Session) -> Result<T, LinkError>,
    ) -> Result<T, LinkError> {
        let mut session = Session::open(&self.dialer, account)?;
        let result = work(&mut session);
        session.logout();
        result
    }
}

impl MailLink for ImapLink {
    fn check(&self, account: &Account) -> Result<(), LinkError> {
        self.session(account, |_| Ok(()))?;
        Submission::open(&self.dialer, account)?.quit();
        Ok(())
    }

    fn folders(&self, account: &Account) -> Result<Vec<RemoteFolder>, LinkError> {
        self.session(account, Session::folders)
    }

    fn sync(
        &self,
        account: &Account,
        folder: &str,
        since: Option<&FolderState>,
        known: &[u32],
        limits: SyncLimits,
    ) -> Result<FolderSync, LinkError> {
        self.session(account, |s| sync(s, folder, since, known, limits))
    }

    fn fetch(
        &self,
        account: &Account,
        folder: &str,
        uid: u32,
    ) -> Result<Option<Vec<u8>>, LinkError> {
        self.session(account, |s| {
            s.open_folder(folder, false, None)?;
            let found = s.fetch(&[uid], "UID BODY.PEEK[]")?;
            Ok(found
                .into_iter()
                .find(|(u, _)| *u == uid)
                .and_then(|(_, pairs)| body(&pairs, "BODY[]")))
        })
    }

    fn submit(&self, account: &Account, outgoing: &Outgoing<'_>) -> Result<Submitted, LinkError> {
        let mut submission = Submission::open(&self.dialer, account)?;
        let result = submission.submit(outgoing);
        submission.quit();
        result
    }

    fn append(
        &self,
        account: &Account,
        folder: &str,
        message: &[u8],
    ) -> Result<Option<u32>, LinkError> {
        self.session(account, |s| s.append(folder, message))
    }

    fn move_to(
        &self,
        account: &Account,
        from: &str,
        uid: u32,
        to: &str,
    ) -> Result<Option<u32>, LinkError> {
        self.session(account, |s| {
            s.open_folder(from, true, None)?;
            s.move_to(uid, to)
        })
    }

    fn wait(&self, account: &Account, folder: &str, timeout: Duration) -> Result<bool, LinkError> {
        self.session(account, |s| {
            s.open_folder(folder, false, None)?;
            s.idle(timeout)
        })
    }
}

fn body(pairs: &[(String, Value)], key: &str) -> Option<Vec<u8>> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| match v {
            Value::Str(bytes) => Some(bytes.clone()),
            _ => None,
        })
}

/// What changed in `folder` since `since`, by the cheapest route the
/// server offers: nothing at all when its counts haven't moved, QRESYNC's
/// list of what vanished when it has it, else every UID compared with
/// what this computer has.
fn sync(
    s: &mut Session,
    folder: &str,
    since: Option<&FolderState>,
    known: &[u32],
    limits: SyncLimits,
) -> Result<FolderSync, LinkError> {
    let resync = since.and_then(|st| st.highest_modseq.map(|m| (st.uid_validity, m)));
    let opened = s.open_folder(folder, false, resync)?;
    let state = FolderState {
        uid_validity: opened.uid_validity,
        uid_next: opened.uid_next,
        highest_modseq: opened.highest_modseq,
        exists: opened.exists,
    };
    let reset = since.is_some_and(|st| st.uid_validity != state.uid_validity);
    let mut result = FolderSync {
        state,
        reset,
        ..FolderSync::default()
    };
    // UIDNEXT and EXISTS both unmoved: nothing arrived (UIDNEXT would
    // grow) and so nothing went (EXISTS would shrink).
    if let Some(st) = since
        && !reset
        && state.uid_next > 0
        && st.uid_next == state.uid_next
        && st.exists == state.exists
    {
        return Ok(result);
    }
    let mut new = match since {
        Some(st) if !reset => {
            let from = st.uid_next.max(1);
            let new = s.search(Some(from))?;
            result.vanished = if opened.resynced {
                opened
                    .vanished
                    .iter()
                    .copied()
                    .filter(|u| known.contains(u))
                    .collect()
            } else if known.is_empty() {
                Vec::new()
            } else {
                let all = s.search(None)?;
                known
                    .iter()
                    .copied()
                    .filter(|u| all.binary_search(u).is_err())
                    .collect()
            };
            new
        }
        _ => s.search(None)?,
    };
    new.retain(|u| !known.contains(u) || reset);
    if new.len() > limits.newest {
        new.drain(..new.len() - limits.newest);
    }
    for chunk in new.chunks(FETCH_CHUNK) {
        let sizes = s.fetch(chunk, "UID RFC822.SIZE")?;
        let (small, large): (Vec<_>, Vec<_>) = sizes.iter().partition(|(_, pairs)| {
            pairs
                .iter()
                .find(|(k, _)| k == "RFC822.SIZE")
                .and_then(|(_, v)| v.number::<u64>())
                .is_some_and(|n| n <= limits.max_bytes)
        });
        let small: Vec<u32> = small.iter().map(|(u, _)| *u).collect();
        let large: Vec<u32> = large.iter().map(|(u, _)| *u).collect();
        for (uid, pairs) in s.fetch(&small, "UID BODY.PEEK[]")? {
            if let Some(raw) = body(&pairs, "BODY[]") {
                result.new.push(RemoteMessage {
                    uid,
                    raw,
                    truncated: false,
                });
            }
        }
        for (uid, pairs) in s.fetch(&large, "UID BODY.PEEK[HEADER]")? {
            if let Some(raw) = body(&pairs, "BODY[HEADER]") {
                result.new.push(RemoteMessage {
                    uid,
                    raw,
                    truncated: true,
                });
            }
        }
    }
    result.new.sort_by_key(|m| m.uid);
    Ok(result)
}
