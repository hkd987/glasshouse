//! Mail on a computer (ADR 0037 §6, IN-2): one set of mail actions, and a
//! link per provider.
//!
//! [`MailLink`] is how `glass-app-mail` reaches a person's mailbox: list
//! its folders, sync a folder since what the computer already has, fetch a
//! message, submit one, append one to a folder, and move one. The links
//! themselves live where the network is, in `glass-gateway-providers`
//! (`ImapLink`: IMAP and SMTP submission over the base system's TLS), so
//! the app crate stays free of it. [`testing::MemoryLink`] is a mailbox in
//! memory, for tests of everything above the wire.
//!
//! Links hold no credentials. Each request carries the [`Account`] it acts
//! for: the servers from IT's signed `[mail]` policy, and the person's
//! sign-in, which glassd keeps root only and never puts in a record or a
//! result. An `Account`'s `Debug` never shows the secret.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A mail server, as policy names it (NV-3). There is no plain option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    pub host: String,
    pub port: u16,
    /// `true`: TLS from the first byte (IMAPS 993, submissions 465);
    /// `false`: plain, then STARTTLS before anything else (submission 587).
    pub implicit_tls: bool,
}

/// A secret that never prints.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The secret itself, for the wire only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// Who a request acts for, and where their mail is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The person's login on this computer.
    pub login: String,
    /// Where mail is read (IMAP).
    pub incoming: Server,
    /// Where mail is sent through (SMTP submission).
    pub outgoing: Server,
    /// The name they sign in with, usually their address.
    pub user: String,
    pub secret: Secret,
}

/// What a folder is for, from the server's special-use attributes (RFC
/// 6154) or, without them, its usual names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
}

impl Role {
    pub const ALL: [Role; 6] = [
        Role::Inbox,
        Role::Drafts,
        Role::Sent,
        Role::Archive,
        Role::Junk,
        Role::Trash,
    ];

    /// The role a folder plays, from its LIST attributes and name.
    pub fn of(name: &str, attributes: &[String]) -> Option<Role> {
        if name.eq_ignore_ascii_case("INBOX") {
            return Some(Role::Inbox);
        }
        let has = |a: &str| attributes.iter().any(|x| x.eq_ignore_ascii_case(a));
        for (attribute, role) in [
            ("\\Drafts", Role::Drafts),
            ("\\Sent", Role::Sent),
            ("\\Archive", Role::Archive),
            ("\\Junk", Role::Junk),
            ("\\Trash", Role::Trash),
        ] {
            if has(attribute) {
                return Some(role);
            }
        }
        let leaf = name.rsplit(['/', '.']).next().unwrap_or(name);
        let named = |names: &[&str]| names.iter().any(|n| leaf.eq_ignore_ascii_case(n));
        if named(&["Drafts", "Draft"]) {
            Some(Role::Drafts)
        } else if named(&["Sent", "Sent Messages", "Sent Items", "Sent Mail"]) {
            Some(Role::Sent)
        } else if named(&["Archive", "Archives"]) {
            Some(Role::Archive)
        } else if named(&["Junk", "Spam", "Junk E-mail", "Junk Email", "Bulk Mail"]) {
            Some(Role::Junk)
        } else if named(&["Trash", "Deleted Items", "Deleted Messages", "Bin"]) {
            Some(Role::Trash)
        } else {
            None
        }
    }
}

/// A folder on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    /// Its name on the server, as the server spells it.
    pub name: String,
    pub role: Option<Role>,
}

/// Where a folder's sync left off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FolderState {
    pub uid_validity: u32,
    pub uid_next: u32,
    /// CONDSTORE's HIGHESTMODSEQ (RFC 7162), when the server has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highest_modseq: Option<u64>,
    /// How many messages the folder had.
    pub exists: u32,
}

/// A message as the server has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMessage {
    pub uid: u32,
    /// The whole message, or only its header when it is larger than the
    /// sync's cap (`truncated`).
    pub raw: Vec<u8>,
    pub truncated: bool,
}

/// What changed in a folder since a [`FolderState`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderSync {
    /// Where the folder is now.
    pub state: FolderState,
    /// The folder's UIDVALIDITY changed: every UID kept before is void.
    pub reset: bool,
    /// Messages new since, oldest first.
    pub new: Vec<RemoteMessage>,
    /// UIDs the computer had that the server no longer does.
    pub vanished: Vec<u32>,
    /// Every UID in the folder the server has as unread (no `\Seen`),
    /// ascending: read state changed anywhere shows here (ADR 0050).
    pub unseen: Vec<u32>,
}

/// What a sync asks of a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncLimits {
    /// The newest messages a first sync fetches.
    pub newest: usize,
    /// Larger messages come as their header only.
    pub max_bytes: u64,
}

/// One message to send: who it goes from and to (the SMTP envelope), and
/// the message as it will leave, byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing<'a> {
    pub from: &'a str,
    pub to: &'a [String],
    pub message: &'a [u8],
}

/// The server's answer to a submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    /// Its final reply, such as `250 2.0.0 queued as 1a2b`.
    pub reply: String,
}

/// Why a request to the mail server didn't go through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The server couldn't be reached (offline, or TLS failed).
    #[error("{0}")]
    Unreachable(String),
    /// The server didn't accept the person's sign-in.
    #[error("{0}")]
    SignIn(String),
    /// The server refused the request, and says why.
    #[error("{0}")]
    Refused(String),
}

impl LinkError {
    /// Whether trying again later could work without anyone doing
    /// anything: the server was out of reach.
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Unreachable(_))
    }
}

pub type Result<T> = std::result::Result<T, LinkError>;

/// A person's mailbox, reached for them (ADR 0037 §6).
pub trait MailLink: Send + Sync + fmt::Debug {
    /// Signs in to both servers and back out: whether `account` works.
    fn check(&self, account: &Account) -> Result<()>;
    /// The account's folders.
    fn folders(&self, account: &Account) -> Result<Vec<RemoteFolder>>;
    /// What changed in `folder` since `since` (everything, newest first up
    /// to `limits.newest`, when there is no `since`). `known` are the UIDs
    /// this computer has, so a server without QRESYNC can say which went.
    fn sync(
        &self,
        account: &Account,
        folder: &str,
        since: Option<&FolderState>,
        known: &[u32],
        limits: SyncLimits,
    ) -> Result<FolderSync>;
    /// One message, whole; `None` when it's gone.
    fn fetch(&self, account: &Account, folder: &str, uid: u32) -> Result<Option<Vec<u8>>>;
    /// Hands a message to the outgoing server, which delivers it.
    fn submit(&self, account: &Account, outgoing: &Outgoing<'_>) -> Result<Submitted>;
    /// Files `message` in `folder`, marked `\Seen`. The UID it got there,
    /// when the server says (UIDPLUS).
    fn append(&self, account: &Account, folder: &str, message: &[u8]) -> Result<Option<u32>>;
    /// Moves a message to another folder. Its UID there, when the server
    /// says (UIDPLUS).
    fn move_to(&self, account: &Account, from: &str, uid: u32, to: &str) -> Result<Option<u32>>;
    /// Marks messages in `folder` read (`\Seen`) or unread (ADR 0050).
    fn mark(&self, account: &Account, folder: &str, uids: &[u32], seen: bool) -> Result<()>;
    /// Waits up to `timeout` for `folder` to change (IMAP IDLE); whether it
    /// did. A link that can't wait returns `Ok(false)` at once.
    fn wait(&self, _account: &Account, _folder: &str, _timeout: Duration) -> Result<bool> {
        Ok(false)
    }
}

/// A mailbox in memory for tests: folders of messages with UIDs, a record
/// of what was submitted, and a switch to take it out of reach.
pub mod testing {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[derive(Debug, Default)]
    struct Folder {
        uid_validity: u32,
        uid_next: u32,
        messages: BTreeMap<u32, Vec<u8>>,
        /// The UIDs marked `\Seen`.
        seen: std::collections::BTreeSet<u32>,
    }

    #[derive(Debug)]
    struct Kept {
        folders: BTreeMap<String, Folder>,
        submitted: Vec<(String, Vec<String>, Vec<u8>)>,
        password: String,
    }

    #[derive(Debug)]
    pub struct MemoryLink {
        kept: Mutex<Kept>,
        offline: AtomicBool,
    }

    const FOLDERS: [&str; 6] = ["INBOX", "Drafts", "Sent", "Archive", "Junk", "Trash"];

    impl MemoryLink {
        /// A mailbox with the usual folders, whose password is `password`.
        pub fn new(password: &str) -> Self {
            let folders = FOLDERS
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let folder = Folder {
                        uid_validity: 1000 + i as u32,
                        uid_next: 1,
                        ..Folder::default()
                    };
                    ((*name).to_owned(), folder)
                })
                .collect();
            Self {
                kept: Mutex::new(Kept {
                    folders,
                    submitted: Vec::new(),
                    password: password.to_owned(),
                }),
                offline: AtomicBool::new(false),
            }
        }

        /// Out of reach, or back.
        pub fn offline(&self, offline: bool) {
            self.offline.store(offline, Ordering::SeqCst);
        }

        /// Mail arriving in `folder`; its UID.
        pub fn deliver(&self, folder: &str, message: &[u8]) -> u32 {
            let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
            let folder = kept.folders.entry(folder.to_owned()).or_default();
            folder.uid_next = folder.uid_next.max(1);
            let uid = folder.uid_next;
            folder.uid_next += 1;
            folder.messages.insert(uid, message.to_vec());
            uid
        }

        /// What was submitted: the envelope's sender and recipients, and
        /// the message.
        pub fn submitted(&self) -> Vec<(String, Vec<String>, Vec<u8>)> {
            self.kept
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .submitted
                .clone()
        }

        /// The UIDs in `folder` marked read.
        pub fn seen(&self, folder: &str) -> Vec<u32> {
            self.kept
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .folders
                .get(folder)
                .map(|f| f.seen.iter().copied().collect())
                .unwrap_or_default()
        }

        /// The messages in `folder`, by UID.
        pub fn messages(&self, folder: &str) -> Vec<(u32, Vec<u8>)> {
            self.kept
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .folders
                .get(folder)
                .map(|f| f.messages.iter().map(|(u, m)| (*u, m.clone())).collect())
                .unwrap_or_default()
        }

        fn reach(&self, account: &Account) -> Result<std::sync::MutexGuard<'_, Kept>> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(LinkError::Unreachable(format!(
                    "{} can't be reached",
                    account.incoming.host
                )));
            }
            let kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
            if account.secret.expose() != kept.password {
                return Err(LinkError::SignIn(
                    "the server didn't accept the password".into(),
                ));
            }
            Ok(kept)
        }
    }

    fn missing(folder: &str) -> LinkError {
        LinkError::Refused(format!("there's no folder {folder:?}"))
    }

    impl MailLink for MemoryLink {
        fn check(&self, account: &Account) -> Result<()> {
            self.reach(account).map(drop)
        }

        fn folders(&self, account: &Account) -> Result<Vec<RemoteFolder>> {
            let kept = self.reach(account)?;
            Ok(kept
                .folders
                .keys()
                .map(|name| RemoteFolder {
                    name: name.clone(),
                    role: Role::of(name, &[]),
                })
                .collect())
        }

        fn sync(
            &self,
            account: &Account,
            folder: &str,
            since: Option<&FolderState>,
            known: &[u32],
            limits: SyncLimits,
        ) -> Result<FolderSync> {
            let kept = self.reach(account)?;
            let f = kept.folders.get(folder).ok_or_else(|| missing(folder))?;
            let state = FolderState {
                uid_validity: f.uid_validity,
                uid_next: f.uid_next.max(1),
                highest_modseq: None,
                exists: f.messages.len() as u32,
            };
            let reset = since.is_some_and(|s| s.uid_validity != f.uid_validity);
            let after = match since {
                Some(s) if !reset => s.uid_next,
                _ => 0,
            };
            let mut new: Vec<RemoteMessage> = f
                .messages
                .range(after..)
                .rev()
                .take(if since.is_some() && !reset {
                    usize::MAX
                } else {
                    limits.newest
                })
                .map(|(uid, raw)| {
                    let truncated = raw.len() as u64 > limits.max_bytes;
                    RemoteMessage {
                        uid: *uid,
                        raw: if truncated {
                            header_of(raw).to_vec()
                        } else {
                            raw.clone()
                        },
                        truncated,
                    }
                })
                .collect();
            new.reverse();
            let vanished = if reset {
                Vec::new()
            } else {
                known
                    .iter()
                    .copied()
                    .filter(|u| !f.messages.contains_key(u))
                    .collect()
            };
            let unseen = f
                .messages
                .keys()
                .copied()
                .filter(|u| !f.seen.contains(u))
                .collect();
            Ok(FolderSync {
                state,
                reset,
                new,
                vanished,
                unseen,
            })
        }

        fn fetch(&self, account: &Account, folder: &str, uid: u32) -> Result<Option<Vec<u8>>> {
            let kept = self.reach(account)?;
            let f = kept.folders.get(folder).ok_or_else(|| missing(folder))?;
            Ok(f.messages.get(&uid).cloned())
        }

        fn submit(&self, account: &Account, outgoing: &Outgoing<'_>) -> Result<Submitted> {
            let mut kept = self.reach(account)?;
            kept.submitted.push((
                outgoing.from.to_owned(),
                outgoing.to.to_vec(),
                outgoing.message.to_vec(),
            ));
            Ok(Submitted {
                reply: format!("250 2.0.0 queued as m{}", kept.submitted.len()),
            })
        }

        fn append(&self, account: &Account, folder: &str, message: &[u8]) -> Result<Option<u32>> {
            let mut kept = self.reach(account)?;
            let f = kept
                .folders
                .get_mut(folder)
                .ok_or_else(|| missing(folder))?;
            f.uid_next = f.uid_next.max(1);
            let uid = f.uid_next;
            f.uid_next += 1;
            f.messages.insert(uid, message.to_vec());
            f.seen.insert(uid);
            Ok(Some(uid))
        }

        fn move_to(
            &self,
            account: &Account,
            from: &str,
            uid: u32,
            to: &str,
        ) -> Result<Option<u32>> {
            let mut kept = self.reach(account)?;
            if !kept.folders.contains_key(to) {
                return Err(missing(to));
            }
            let source = kept.folders.get_mut(from).ok_or_else(|| missing(from))?;
            let message = source
                .messages
                .remove(&uid)
                .ok_or_else(|| LinkError::Refused(format!("{from} has no message {uid}")))?;
            let seen = source.seen.remove(&uid);
            let f = kept.folders.get_mut(to).ok_or_else(|| missing(to))?;
            f.uid_next = f.uid_next.max(1);
            let new = f.uid_next;
            f.uid_next += 1;
            f.messages.insert(new, message);
            if seen {
                f.seen.insert(new);
            }
            Ok(Some(new))
        }

        fn mark(&self, account: &Account, folder: &str, uids: &[u32], seen: bool) -> Result<()> {
            let mut kept = self.reach(account)?;
            let f = kept
                .folders
                .get_mut(folder)
                .ok_or_else(|| missing(folder))?;
            for uid in uids.iter().filter(|u| f.messages.contains_key(u)) {
                if seen {
                    f.seen.insert(*uid);
                } else {
                    f.seen.remove(uid);
                }
            }
            Ok(())
        }
    }

    /// A message's header: everything up to the first empty line.
    fn header_of(raw: &[u8]) -> &[u8] {
        raw.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| &raw[..i + 4])
            .or_else(|| {
                raw.windows(2)
                    .position(|w| w == b"\n\n")
                    .map(|i| &raw[..i + 2])
            })
            .unwrap_or(raw)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn account(secret: &str) -> Account {
            let server = |port| Server {
                host: "mail.acme.example".into(),
                port,
                implicit_tls: true,
            };
            Account {
                login: "dana".into(),
                incoming: server(993),
                outgoing: server(465),
                user: "dana@acme.example".into(),
                secret: Secret::new(secret),
            }
        }

        #[test]
        fn an_accounts_debug_never_shows_its_secret() {
            let shown = format!("{:?}", account("hunter2-canary"));
            assert!(!shown.contains("hunter2-canary"), "{shown}");
        }

        #[test]
        fn a_sync_since_a_state_brings_only_whats_new_and_whats_gone() {
            let link = MemoryLink::new("pw");
            let a = account("pw");
            let limits = SyncLimits {
                newest: 10,
                max_bytes: 1024,
            };
            let one = link.deliver("INBOX", b"Subject: one\r\n\r\nx\r\n");
            let first = link.sync(&a, "INBOX", None, &[], limits).unwrap();
            assert_eq!(first.new.len(), 1);
            let two = link.deliver("INBOX", b"Subject: two\r\n\r\nx\r\n");
            link.move_to(&a, "INBOX", one, "Archive").unwrap();
            let next = link
                .sync(&a, "INBOX", Some(&first.state), &[one], limits)
                .unwrap();
            assert_eq!(next.new.iter().map(|m| m.uid).collect::<Vec<_>>(), [two]);
            assert_eq!(next.vanished, [one]);
            assert!(matches!(
                link.check(&account("wrong")),
                Err(LinkError::SignIn(_))
            ));
            link.offline(true);
            assert!(link.check(&a).unwrap_err().is_offline());
        }

        #[test]
        fn folders_play_their_roles_by_attribute_then_by_name() {
            let attrs = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
            assert_eq!(Role::of("INBOX", &[]), Some(Role::Inbox));
            assert_eq!(
                Role::of("Bulk", &attrs(&["\\HasNoChildren", "\\Junk"])),
                Some(Role::Junk)
            );
            assert_eq!(Role::of("[Gmail]/Spam", &[]), Some(Role::Junk));
            assert_eq!(Role::of("Deleted Items", &[]), Some(Role::Trash));
            assert_eq!(Role::of("Projects", &[]), None);
            // Gmail's All Mail holds everything: never synced as Archive.
            assert_eq!(Role::of("[Gmail]/All Mail", &[]), None);
        }
    }
}
