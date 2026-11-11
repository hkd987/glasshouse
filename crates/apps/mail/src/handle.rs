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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
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
    /// Marks made on this computer that the server hasn't heard yet, by
    /// UID under `state`'s UIDVALIDITY: whether each is read (ADR 0050).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    marks: BTreeMap<u32, bool>,
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
    /// Messages read or unread now that weren't before.
    pub marked: usize,
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
    /// Drafts a call is sending or replacing now, by path.
    taken: Mutex<BTreeSet<PathBuf>>,
}

/// Drafts one call has taken, until it ends.
pub(crate) struct Taken<'a> {
    handle: &'a MailHandle,
    drafts: Vec<PathBuf>,
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        let mut taken = lock(&self.handle.0.taken);
        for draft in &self.drafts {
            taken.remove(draft);
        }
    }
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

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
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
            taken: Mutex::new(BTreeSet::new()),
        }))
    }

    /// Takes `drafts` (paths) for one call while it sends or replaces
    /// them, so each leaves once (DP-30, ADR 0051): refused, with the
    /// index of the first, when another call has one or this call names
    /// one twice.
    pub(crate) fn take_drafts(&self, drafts: Vec<PathBuf>) -> Result<Taken<'_>, usize> {
        let mut taken = lock(&self.0.taken);
        let mut named = BTreeSet::new();
        if let Some(i) = drafts
            .iter()
            .position(|d| taken.contains(d) || !named.insert(d))
        {
            return Err(i);
        }
        taken.extend(drafts.iter().cloned());
        Ok(Taken {
            handle: self,
            drafts,
        })
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

    /// Tells glassd `login`'s mail changed on this computer: an action
    /// drafted, sent, filed or marked it.
    pub(crate) fn changed(&self, login: &str) {
        self.notify(&Notice::Changed {
            login: login.to_owned(),
        });
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

    /// `login`'s sign-in, if they have one here. A file that isn't a
    /// sign-in is set aside beside itself (still root's alone), and they
    /// sign in again.
    fn sign_in(&self, login: &str) -> Option<SignIn> {
        let path = self.person_dir(login)?.join("account.json");
        glass_state::read_json_or_set_aside(&path).ok().flatten()
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

    /// Where `login`'s folders had got to. Lost or damaged, they sync from
    /// the start: the service has the mail, and what was there is kept
    /// aside.
    fn load_state(&self, login: &str) -> SyncState {
        self.person_dir(login)
            .and_then(|d| {
                glass_state::read_json_or_set_aside(&d.join("sync.json"))
                    .ok()
                    .flatten()
            })
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

    /// One person's mail, changed by one sync or mark at a time.
    pub(crate) fn person_lock(&self, login: &str) -> Arc<Mutex<()>> {
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
                        marks: BTreeMap::new(),
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
        // Read state (ADR 0050): marks made here while the server was out
        // of reach go to it first; then the server's says the rest.
        let mut state = self.load_state(login);
        let mut marks = match state.folders.get(&role) {
            Some(entry) if !synced.reset => entry.marks.clone(),
            _ => BTreeMap::new(),
        };
        let mut unseen: BTreeSet<u32> = synced.unseen.iter().copied().collect();
        for seen in [true, false] {
            let uids: Vec<u32> = marks
                .iter()
                .filter(|(_, read)| **read == seen)
                .map(|(uid, _)| *uid)
                .collect();
            if uids.is_empty() {
                continue;
            }
            match remote.link.mark(&account, &name, validity, &uids, seen) {
                Ok(_) => {
                    for uid in &uids {
                        marks.remove(uid);
                        if seen {
                            unseen.remove(uid);
                        } else {
                            unseen.insert(*uid);
                        }
                    }
                }
                Err(e) if e.is_offline() => {}
                // Gone, or refused: the server's state stands.
                Err(_) => {
                    for uid in &uids {
                        marks.remove(uid);
                    }
                }
            }
        }
        // One listing of the markers, and a write only where one differs.
        let was_read = cache.read_names(role).map_err(local)?;
        for kept in cache.names(role).map_err(local)? {
            let Some((v, uid)) = cache::parse_synced(&kept) else {
                continue;
            };
            if v != validity {
                continue;
            }
            let read = marks.get(&uid).copied().unwrap_or(!unseen.contains(&uid));
            if read != was_read.contains(&kept)
                && cache.set_read(role, &kept, read).map_err(local)?
            {
                report.marked += 1;
            }
        }
        state.folders.insert(
            role,
            FolderEntry {
                name,
                state: Some(synced.state),
                marks,
            },
        );
        self.save_state(login, &state)?;
        lock(&self.0.fresh).insert((login.to_owned(), role), Instant::now());
        if report.new + report.gone + report.marked > 0 {
            self.notify(&Notice::Changed {
                login: login.to_owned(),
            });
        }
        Ok(report)
    }

    /// Tells the server that synced messages `uids` (named under
    /// `validity`) in `role` are read or unread now. The caller holds
    /// [`Self::person_lock`], so a sync can't undo the mark halfway. Out of
    /// reach or not connected, the marks wait for the next sync (ADR 0050).
    /// Whether the server has them now.
    pub(crate) fn tell_server(
        &self,
        login: &str,
        role: Role,
        validity: u32,
        uids: &[u32],
        seen: bool,
    ) -> Result<bool, SyncError> {
        if uids.is_empty() {
            return Ok(true);
        }
        if let Some((remote, account)) = self.account(login) {
            let told = self
                .folder(login, &remote, &account, role)
                .and_then(|(name, folder)| match folder {
                    Some(f) if f.uid_validity != validity => Err(SyncError::Local(format!(
                        "{} changed on the server since it was listed; list it again",
                        cache::dir(role)
                    ))),
                    _ => remote
                        .link
                        .mark(&account, &name, validity, uids, seen)
                        .map_err(|e| match e {
                            LinkError::Refused(_) => SyncError::Local(format!(
                                "{} changed on the server since it was listed; list it again",
                                cache::dir(role)
                            )),
                            e => SyncError::Link(e),
                        }),
                });
            match told {
                Ok(found) => {
                    // A mark of these still waiting is older: the server
                    // has the newer one now (ADR 0051).
                    self.forget_waiting(login, role, uids)?;
                    if found.len() < uids.len() {
                        return Err(SyncError::Local(format!(
                            "it isn't in {} on the mail server any more (filed elsewhere, \
                             perhaps on another device); list the folder again",
                            cache::dir(role)
                        )));
                    }
                    return Ok(true);
                }
                Err(SyncError::Link(e)) if e.is_offline() => {}
                Err(e) => return Err(e),
            }
        }
        let mut state = self.load_state(login);
        let Some(entry) = state.folders.get_mut(&role) else {
            // Never synced: no message of the server's is here to mark.
            return Ok(false);
        };
        if entry.state.is_some_and(|f| f.uid_validity != validity) {
            return Ok(false);
        }
        for uid in uids {
            entry.marks.insert(*uid, seen);
        }
        self.save_state(login, &state)?;
        Ok(false)
    }

    /// The UIDVALIDITY `role` was last synced under, if it ever was.
    pub(crate) fn synced_validity(&self, login: &str, role: Role) -> Option<u32> {
        self.load_state(login)
            .folders
            .get(&role)
            .and_then(|e| e.state)
            .map(|s| s.uid_validity)
    }

    /// Marks of `uids` in `role` waiting for the server, gone: the server
    /// has a newer one, or the message no longer has that UID here.
    pub(crate) fn forget_waiting(
        &self,
        login: &str,
        role: Role,
        uids: &[u32],
    ) -> Result<(), SyncError> {
        let mut state = self.load_state(login);
        let Some(entry) = state.folders.get_mut(&role) else {
            return Ok(());
        };
        let before = entry.marks.len();
        entry.marks.retain(|uid, _| !uids.contains(uid));
        if entry.marks.len() == before {
            return Ok(());
        }
        self.save_state(login, &state)
    }

    /// Tells the server of a mark of `uid` (under `validity`) in `role`
    /// still waiting, before the message moves: the MOVE then takes the
    /// flag with it, where a mark waiting under the old UID would be lost
    /// (ADR 0051). The caller holds [`Self::person_lock`].
    pub(crate) fn tell_waiting(
        &self,
        login: &str,
        role: Role,
        validity: u32,
        uid: u32,
    ) -> Result<(), SyncError> {
        let waiting = self
            .load_state(login)
            .folders
            .get(&role)
            .filter(|e| e.state.is_some_and(|s| s.uid_validity == validity))
            .and_then(|e| e.marks.get(&uid).copied());
        let Some(seen) = waiting else {
            return Ok(());
        };
        let (remote, account) = self.account(login).ok_or(SyncError::NotConnected)?;
        let (name, _) = self.folder(login, &remote, &account, role)?;
        match remote.link.mark(&account, &name, validity, &[uid], seen) {
            Ok(_) => {}
            Err(e) if e.is_offline() => return Err(SyncError::Link(e)),
            // Refused: the server's state stands.
            Err(_) => {}
        }
        self.forget_waiting(login, role, &[uid])
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
    // Beside it, synced, renamed over it, the folder synced (glass-state).
    glass_state::write(path, bytes, glass_state::PRIVATE)
}
