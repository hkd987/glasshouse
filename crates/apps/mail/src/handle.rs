//! glassd's side of mail (ADR 0037 §6): the link to the org's mail
//! service, each person's sign-in, and where each folder's sync left off.
//! The sign-ins and sync state are root's, in glassd's state directory
//! (`<state>/mail/<login>/`), never in a home, so no agent, page or
//! program of the person's reads a mail credential; only the mail itself
//! goes to `~/Mail`, the cache.
//!
//! One handle lives as long as glassd: the providers policy rebuilds share
//! it, and its watcher thread keeps the Inbox fresh (IMAP IDLE) and wakes
//! the Mail app when mail arrives.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use glass_managed::mail::{
    Account, FolderState, LinkError, MailLink, Role, Secret, Server, SyncLimits,
};
use glass_platform::fs::Owner;
use serde::{Deserialize, Serialize};

use crate::cache::{self, Cache};
use crate::message;

/// How long a folder synced counts as fresh for a listing.
const FRESH: Duration = Duration::from_secs(5);
/// A first sync's newest messages per folder.
const FIRST_SYNC: usize = 500;

/// The org's mail service, as glassd connects it from `[mail]` policy.
#[derive(Clone)]
pub struct Remote {
    pub link: Arc<dyn MailLink>,
    pub incoming: Server,
    pub outgoing: Server,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("incoming", &self.incoming)
            .field("outgoing", &self.outgoing)
            .finish_non_exhaustive()
    }
}

/// What glassd hears from mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// New mail, or mail gone, in a person's cache: the Mail app reloads.
    Changed { login: String },
}

type Listener = Box<dyn Fn(&Notice) + Send + Sync>;

/// A person's sign-in, as kept.
#[derive(Serialize, Deserialize)]
struct SignIn {
    user: String,
    secret: Secret,
}

/// Where a person's folders are on the server and where each sync left
/// off.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SyncState {
    #[serde(default)]
    folders: BTreeMap<Role, FolderEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FolderEntry {
    /// The folder's name on the server.
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<FolderState>,
}

/// A person's home, as the actions last saw it, for the watcher.
#[derive(Debug, Clone)]
struct Home {
    home: PathBuf,
    owner: Owner,
}

/// Why a folder couldn't be brought up to date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncError {
    /// No service, or no sign-in for this person.
    NotConnected,
    Link(LinkError),
    Local(String),
}

/// What a sync changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Synced {
    pub new: usize,
    pub gone: usize,
}

struct Inner {
    dir: PathBuf,
    remote: RwLock<Option<Remote>>,
    listeners: Mutex<Vec<Listener>>,
    /// One sync at a time per person.
    syncing: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// When each person's folders last synced.
    fresh: Mutex<HashMap<(String, Role), Instant>>,
    homes: Mutex<BTreeMap<String, Home>>,
}

/// Mail on this computer, shared by glassd's watcher and the actions.
#[derive(Clone)]
pub struct MailHandle(Arc<Inner>);

impl std::fmt::Debug for MailHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailHandle")
            .field("dir", &self.0.dir)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A login that is safe as a folder name.
pub(crate) fn plain_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 64
        && !login.starts_with(['.', '-'])
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Default for MailHandle {
    fn default() -> Self {
        Self::new("/nonexistent")
    }
}

impl MailHandle {
    /// Mail kept in `dir` (glassd's `<state>/mail`), with no service yet:
    /// `~/Mail` is then the whole store.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            dir: dir.into(),
            remote: RwLock::new(None),
            listeners: Mutex::new(Vec::new()),
            syncing: Mutex::new(HashMap::new()),
            fresh: Mutex::new(HashMap::new()),
            homes: Mutex::new(BTreeMap::new()),
        }))
    }

    /// Mail goes through `remote` from now on (`[mail] service = "imap"`).
    /// A different service than before starts every folder afresh.
    pub fn connect(&self, remote: Remote) {
        let mut current = self
            .0
            .remote
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let same = current
            .as_ref()
            .is_some_and(|r| r.incoming == remote.incoming && r.outgoing == remote.outgoing);
        *current = Some(remote);
        drop(current);
        if !same {
            lock(&self.0.fresh).clear();
        }
    }

    /// No service: `~/Mail` is the whole store again.
    pub fn disconnect(&self) {
        *self
            .0
            .remote
            .write()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(crate) fn remote(&self) -> Option<Remote> {
        self.0
            .remote
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Calls `f` for what glassd should hear: mail that changed.
    pub fn on_notice(&self, f: impl Fn(&Notice) + Send + Sync + 'static) {
        lock(&self.0.listeners).push(Box::new(f));
    }

    fn notify(&self, notice: &Notice) {
        for listener in lock(&self.0.listeners).iter() {
            listener(notice);
        }
    }

    fn person_dir(&self, login: &str) -> Option<PathBuf> {
        plain_login(login).then(|| self.0.dir.join(login))
    }

    /// Keeps `login`'s sign-in, root only (mode 0600 in a 0700 folder).
    pub(crate) fn save_sign_in(
        &self,
        login: &str,
        user: &str,
        secret: &Secret,
    ) -> std::io::Result<()> {
        let dir = self
            .person_dir(login)
            .ok_or_else(|| std::io::Error::other("not a plain login"))?;
        let bytes = serde_json::to_vec(&SignIn {
            user: user.to_owned(),
            secret: secret.clone(),
        })?;
        write_private(&dir.join("account.json"), &bytes)?;
        // A new sign-in may be another account: start its folders afresh.
        let _ = fs::remove_file(dir.join("sync.json"));
        lock(&self.0.fresh).retain(|(l, _), _| l != login);
        Ok(())
    }

    fn sign_in(&self, login: &str) -> Option<SignIn> {
        let path = self.person_dir(login)?.join("account.json");
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    /// Whether `login` has signed in to the org's service on this computer.
    pub fn connected(&self, login: &str) -> bool {
        self.remote().is_some() && self.sign_in(login).is_some()
    }

    /// The name `login` signs in with, if they have.
    pub(crate) fn user(&self, login: &str) -> Option<String> {
        self.sign_in(login).map(|s| s.user)
    }

    /// `login`'s account on the org's service, when both are there.
    pub(crate) fn account(&self, login: &str) -> Option<(Remote, Account)> {
        let remote = self.remote()?;
        let sign_in = self.sign_in(login)?;
        let account = Account {
            login: login.to_owned(),
            incoming: remote.incoming.clone(),
            outgoing: remote.outgoing.clone(),
            user: sign_in.user,
            secret: sign_in.secret,
        };
        Some((remote, account))
    }

    /// An account for `login` with a sign-in not kept yet, to check it.
    pub(crate) fn trial(
        &self,
        login: &str,
        user: &str,
        secret: Secret,
    ) -> Option<(Remote, Account)> {
        let remote = self.remote()?;
        let account = Account {
            login: login.to_owned(),
            incoming: remote.incoming.clone(),
            outgoing: remote.outgoing.clone(),
            user: user.to_owned(),
            secret,
        };
        Some((remote, account))
    }

    fn load_state(&self, login: &str) -> SyncState {
        self.person_dir(login)
            .and_then(|d| fs::read(d.join("sync.json")).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, login: &str, state: &SyncState) -> Result<(), SyncError> {
        let dir = self
            .person_dir(login)
            .ok_or_else(|| SyncError::Local("not a plain login".into()))?;
        let bytes = serde_json::to_vec(state).map_err(|e| SyncError::Local(e.to_string()))?;
        write_private(&dir.join("sync.json"), &bytes)
            .map_err(|e| SyncError::Local(format!("keeping mail's place: {e}")))
    }

    /// Where `login`'s mail is kept, for the watcher: noted on each action.
    pub(crate) fn remember(&self, login: &str, home: &Path, owner: Owner) {
        lock(&self.0.homes).insert(
            login.to_owned(),
            Home {
                home: home.to_path_buf(),
                owner,
            },
        );
    }

    fn person_lock(&self, login: &str) -> Arc<Mutex<()>> {
        lock(&self.0.syncing)
            .entry(login.to_owned())
            .or_default()
            .clone()
    }

    /// Whether `role` synced for `login` in the last few seconds.
    pub(crate) fn is_fresh(&self, login: &str, role: Role) -> bool {
        lock(&self.0.fresh)
            .get(&(login.to_owned(), role))
            .is_some_and(|t| t.elapsed() < FRESH)
    }

    /// The server's name for `role`, and where its sync left off.
    pub(crate) fn folder(
        &self,
        login: &str,
        remote: &Remote,
        account: &Account,
        role: Role,
    ) -> Result<(String, Option<FolderState>), SyncError> {
        let mut state = self.load_state(login);
        if !state.folders.contains_key(&role) {
            let folders = remote.link.folders(account).map_err(SyncError::Link)?;
            for folder in folders {
                if let Some(r) = folder.role {
                    state.folders.entry(r).or_insert(FolderEntry {
                        name: folder.name,
                        state: None,
                    });
                }
            }
            self.save_state(login, &state)?;
        }
        let entry = state.folders.get(&role).ok_or_else(|| {
            SyncError::Link(LinkError::Refused(format!(
                "your mail service has no {} folder",
                cache::dir(role)
            )))
        })?;
        Ok((entry.name.clone(), entry.state))
    }

    /// Brings `role` in `login`'s cache up to date with the server.
    pub(crate) fn sync(
        &self,
        login: &str,
        home: &Path,
        owner: Owner,
        role: Role,
    ) -> Result<Synced, SyncError> {
        let person = self.person_lock(login);
        let _one = lock(&person);
        let (remote, account) = self.account(login).ok_or(SyncError::NotConnected)?;
        let (name, since) = self.folder(login, &remote, &account, role)?;
        let cache = Cache::open(home, owner)
            .map_err(|e| SyncError::Local(format!("opening the mail folder: {e}")))?;
        cache
            .prepare()
            .map_err(|e| SyncError::Local(format!("making the mail folders: {e}")))?;
        let reading = |e: std::io::Error| SyncError::Local(format!("reading the mail folder: {e}"));
        // With no record of where this folder's sync left off (a new
        // sign-in, a lost state folder), start from what the cache holds,
        // so mail that went from the server meanwhile goes from here too;
        // a different UIDVALIDITY on the server starts it afresh.
        let since = match since {
            Some(s) => Some(s),
            None => cache.kept_validity(role).map_err(reading)?.map(|validity| {
                let uids = cache.uids(role, validity).unwrap_or_default();
                FolderState {
                    uid_validity: validity,
                    uid_next: uids.last().map_or(1, |u| u.saturating_add(1)),
                    highest_modseq: None,
                    exists: uids.len() as u32,
                }
            }),
        };
        let known = match since {
            Some(s) => cache.uids(role, s.uid_validity).map_err(reading)?,
            None => Vec::new(),
        };
        let limits = SyncLimits {
            newest: FIRST_SYNC,
            max_bytes: cache::MAX_MESSAGE_BYTES,
        };
        let synced = remote
            .link
            .sync(&account, &name, since.as_ref(), &known, limits)
            .map_err(SyncError::Link)?;
        let validity = synced.state.uid_validity;
        let mut report = Synced::default();
        let local = |e: std::io::Error| SyncError::Local(format!("keeping mail: {e}"));
        // Kept under another UIDVALIDITY (or none before): void.
        for kept in cache.names(role).map_err(local)? {
            if let Some((v, _)) = cache::parse_synced(&kept)
                && v != validity
            {
                cache.remove(role, &kept).map_err(local)?;
                report.gone += 1;
            }
        }
        for m in &synced.new {
            let raw = if m.truncated {
                message::header_only(&m.raw)
            } else {
                m.raw.clone()
            };
            cache
                .keep(role, &cache::synced_name(validity, m.uid), &raw)
                .map_err(local)?;
            report.new += 1;
        }
        for uid in &synced.vanished {
            cache
                .remove(role, &cache::synced_name(validity, *uid))
                .map_err(local)?;
            report.gone += 1;
        }
        let mut state = self.load_state(login);
        state.folders.insert(
            role,
            FolderEntry {
                name,
                state: Some(synced.state),
            },
        );
        self.save_state(login, &state)?;
        lock(&self.0.fresh).insert((login.to_owned(), role), Instant::now());
        if report.new + report.gone > 0 {
            self.notify(&Notice::Changed {
                login: login.to_owned(),
            });
        }
        Ok(report)
    }

    /// The people whose mail glassd watches: signed in, with a home seen.
    pub fn watched(&self) -> Vec<String> {
        if self.remote().is_none() {
            return Vec::new();
        }
        lock(&self.0.homes)
            .keys()
            .filter(|login| self.sign_in(login).is_some())
            .cloned()
            .collect()
    }

    /// Waits up to `timeout` for `login`'s Inbox to change (IMAP IDLE),
    /// then syncs it, which wakes the Mail app when mail arrived. For
    /// glassd's watcher; an error says why it couldn't wait.
    pub fn watch(&self, login: &str, timeout: Duration) -> Result<(), String> {
        let home = lock(&self.0.homes)
            .get(login)
            .cloned()
            .ok_or_else(|| "no mail seen yet".to_owned())?;
        let (remote, account) = self
            .account(login)
            .ok_or_else(|| "not connected".to_owned())?;
        let (name, _) = self
            .folder(login, &remote, &account, Role::Inbox)
            .map_err(|e| format!("{e:?}"))?;
        // Anything that arrived while nobody waited comes first.
        self.sync(login, &home.home, home.owner, Role::Inbox)
            .map_err(|e| format!("{e:?}"))?;
        let changed = remote
            .link
            .wait(&account, &name, timeout)
            .map_err(|e| e.to_string())?;
        if changed {
            self.sync(login, &home.home, home.owner, Role::Inbox)
                .map_err(|e| format!("{e:?}"))?;
        }
        Ok(())
    }
}

/// Writes a file only root (glassd's user) reads, atomically.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let tmp = dir.join(format!(
        ".{}.new",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}
