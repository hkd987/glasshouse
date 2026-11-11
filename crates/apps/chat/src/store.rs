//! glassd's side of chat (ADR 0030, decision 2): each person's member key
//! and the sealed records the console sent for them, each checked before
//! it is kept. All of it is root's, in glassd's state directory
//! (`<state>/chat/<login>/`), never in a home, so a person's own programs
//! and agents reach chat only through actions.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use glass_crypto::{EpochKey, PublicKey};
pub use glass_managed::chat::ChatFailure;
use glass_managed::chat::{
    ChatLink, ChatSyncRequest, ChatSyncResponse, Conversation, ConversationKind, Epoch, KeptFor,
    LinkError, MemberKey, MemberSecret, Message, RegisterKey, is_conversation_id,
};
use glass_managed::shared::SealedBlob;
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

/// What glassd audits from chat (AU-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// A member key was made and registered for `login` on this computer.
    KeyMade { login: String, key: String },
    /// A record from the console failed a check and was kept out.
    Refused {
        login: String,
        conversation: Option<String>,
        reason: String,
    },
    /// New conversations or messages arrived for these people, in
    /// conversations of these kinds.
    Changed {
        logins: Vec<String>,
        kinds: Vec<ConversationKind>,
    },
    /// Chat older than the organization keeps it was removed from this
    /// computer (CF-61): how many messages, and the periods they were
    /// past.
    Trimmed { removed: usize, kept_for: KeptFor },
}

// Chat, docs, sheets and artifacts all say these (ADR 0033), so they name
// what's shared, not chat alone.
pub(crate) const NOT_MANAGED: &str = "This computer isn't managed by an organization, so nothing here is shared with colleagues: no chat, docs or shared pages.";
pub(crate) const NOT_SET_UP: &str = "Your organization hasn't set up chat and sharing yet: IT makes its compliance key in the admin console first.";

/// A person's keys on this computer.
pub(crate) struct Member {
    pub key: MemberKey,
    pub secret: MemberSecret,
}

#[derive(Clone)]
pub(crate) struct Connection {
    pub org: String,
    pub device: String,
    pub link: Arc<dyn ChatLink>,
}

/// IT's compliance key, from the signed Chat release (ADR 0030, decision 4).
#[derive(Clone)]
pub(crate) struct Compliance {
    pub key: PublicKey,
    pub hex: String,
    pub fingerprint: String,
}

type Listener = Box<dyn Fn(&Notice) + Send + Sync>;

/// A draft's place: the login, the conversation and the thread, if any.
type DraftKey = (String, String, Option<String>);

/// A conversation folded: how many messages and the newest sequence
/// number it was folded from, and the result.
type Fold = ((usize, u64), Arc<crate::fold::Folded>);

struct Inner {
    dir: PathBuf,
    connection: RwLock<Option<Connection>>,
    compliance: RwLock<Option<Compliance>>,
    /// Writers to the stores, key files and cursor, one at a time.
    write: Mutex<()>,
    /// Times the cursor went back to the beginning for someone new here: a
    /// sync that began before one leaves the cursor there (ADR 0052).
    resets: AtomicU64,
    listeners: Mutex<Vec<Listener>>,
    /// Unsent drafts by (login, conversation, thread), in memory only, so
    /// nothing unsealed is written (ADR 0030 C4a).
    drafts: Mutex<HashMap<DraftKey, String>>,
    /// Each conversation folded once per new message, by (login,
    /// conversation): how many messages and the newest sequence number
    /// folded, and the result.
    folds: Mutex<HashMap<(String, String), Fold>>,
    /// Items' logs as read so far, by (login, item).
    item_logs: Mutex<HashMap<(String, String), ItemLog>>,
    /// Each person's store as last read or saved, with what its file was
    /// then: an action reads the store several times over, and each used
    /// to parse all of it again (audit 9.2).
    stores: Mutex<HashMap<String, Kept>>,
    /// People whose store's file is there and couldn't be read: nothing
    /// is saved over it until it can be.
    unread: Mutex<HashMap<String, String>>,
    /// How long the organization keeps chat (IT's Chat release, CF-61);
    /// for good until it sets a period.
    kept_for: Mutex<KeptFor>,
    /// Sealed blobs this computer sent or fetched.
    blobs: Mutex<Blobs>,
    /// How many times a store's file was parsed (for tests).
    parses: AtomicU64,
    /// Tests only: writes fail, as on a full disk.
    full: std::sync::atomic::AtomicBool,
}

/// A store as last read or saved, and what its file was.
struct Kept {
    file: FileIs,
    store: Arc<Store>,
}

/// What tells one writing of a file from the next.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIs {
    len: u64,
    modified: Option<std::time::SystemTime>,
    ino: u64,
}

impl FileIs {
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(path).ok()?;
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            ino: meta.ino(),
        })
    }
}

/// How many people's stores are kept in memory at once.
const STORES_KEPT: usize = 64;

/// Where an item's messages are in its log, as read so far. A log only
/// grows, so each read parses what was appended since, and a reader
/// asking for what's new reads only that (ADR 0052): at 10,000 messages,
/// parsing the whole log took 85 ms for every pull, push and open.
#[derive(Default)]
struct ItemLog {
    /// Bytes of the log read.
    read: u64,
    ids: HashSet<String>,
    /// Each message's sequence number, offset and length, in sequence
    /// order.
    at: Vec<(u64, u64, usize)>,
}

/// How much of the sealed blobs a computer sent or fetched it keeps in
/// memory, as they travel (hex): room for ten of the largest.
const BLOBS_KEPT_BYTES: usize = 32 * 1024 * 1024;

/// Sealed blobs this computer sent or fetched, the one used longest ago
/// first. A blob never changes (its id names it, and whoever opens it
/// checks its SHA-256), so one that is here isn't fetched again: an
/// artifact's page was fetched from the console every time it was looked
/// at, and a doc's snapshot every time the doc was loaded (audit 9.3).
///
/// They are kept sealed, as they came. Nothing here opens without the key,
/// which travels only inside a sealed message.
#[derive(Default)]
struct Blobs {
    held: std::collections::VecDeque<SealedBlob>,
    bytes: usize,
}

impl Blobs {
    fn size(blob: &SealedBlob) -> usize {
        blob.ct.len() + blob.nonce.len() + blob.id.len() + blob.conversation.len()
    }

    fn get(&mut self, conversation: &str, id: &str) -> Option<SealedBlob> {
        let at = self
            .held
            .iter()
            .position(|b| b.id == id && b.conversation == conversation)?;
        let blob = self.held.remove(at)?;
        self.held.push_back(blob.clone());
        Some(blob)
    }

    fn keep(&mut self, blob: SealedBlob) {
        if self
            .held
            .iter()
            .any(|b| b.id == blob.id && b.conversation == blob.conversation)
        {
            return;
        }
        let size = Self::size(&blob);
        // One that wouldn't fit alone would only push out the rest.
        if size > BLOBS_KEPT_BYTES {
            return;
        }
        self.bytes += size;
        self.held.push_back(blob);
        while self.bytes > BLOBS_KEPT_BYTES {
            let Some(oldest) = self.held.pop_front() else {
                break;
            };
            self.bytes -= Self::size(&oldest);
        }
    }
}

/// Chat on this computer, shared by glassd's sync thread and the actions.
#[derive(Clone)]
pub struct ChatHandle(Arc<Inner>);

impl std::fmt::Debug for ChatHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatHandle")
            .field("dir", &self.0.dir)
            .finish_non_exhaustive()
    }
}

/// What the console sent for one person, as kept.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct Store {
    /// The member key the console has for this person here.
    #[serde(default)]
    pub registered: Option<String>,
    #[serde(default)]
    pub conversations: BTreeMap<String, Conversation>,
    /// By conversation; each verified, with this computer's envelopes.
    #[serde(default)]
    pub epochs: BTreeMap<String, Vec<Epoch>>,
    /// By conversation, verified, by sequence number.
    #[serde(default)]
    pub messages: BTreeMap<String, Vec<Message>>,
    /// The member keys that signed what's here, by id.
    #[serde(default)]
    pub keys: BTreeMap<String, MemberKey>,
    /// The newest message the person has seen in Chat, per conversation.
    #[serde(default)]
    pub seen: BTreeMap<String, u64>,
    /// What the person wants to hear of, per conversation (CF-92): `all`,
    /// `mentions` or `none`. Unset is `all` for direct messages and groups
    /// and `mentions` for channels.
    #[serde(default)]
    pub notify: BTreeMap<String, String>,
    /// Records already refused, so each is audited once.
    #[serde(default)]
    pub refused: BTreeSet<String>,
    /// The newest message ids kept per shared item, so a message echoed
    /// back by sync isn't logged twice (ADR 0035 §4).
    #[serde(default)]
    pub item_recent: BTreeMap<String, Vec<String>>,
    /// Items' messages kept since this store was loaded, appended to their
    /// logs when it is saved.
    #[serde(skip)]
    pub item_pending: Vec<(String, Message)>,
}

/// Message ids remembered per item for [`Store::item_recent`].
const ITEM_RECENT: usize = 256;

/// A login that is safe as a folder name.
pub(crate) fn plain_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 64
        && !login.starts_with(['.', '-'])
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl ChatHandle {
    /// Chat kept in `dir`, which glassd owns. Until [`Self::connect`],
    /// every action says this computer has no chat.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            dir: dir.into(),
            connection: RwLock::new(None),
            compliance: RwLock::new(None),
            write: Mutex::new(()),
            resets: AtomicU64::new(0),
            listeners: Mutex::new(Vec::new()),
            drafts: Mutex::new(HashMap::new()),
            folds: Mutex::new(HashMap::new()),
            kept_for: Mutex::new(KeptFor::default()),
            blobs: Mutex::new(Blobs::default()),
            item_logs: Mutex::new(HashMap::new()),
            stores: Mutex::new(HashMap::new()),
            unread: Mutex::new(HashMap::new()),
            parses: AtomicU64::new(0),
            full: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// `login`'s draft in `conversation` (a thread's when `thread`).
    pub(crate) fn draft(
        &self,
        login: &str,
        conversation: &str,
        thread: Option<&str>,
    ) -> Option<String> {
        self.0
            .drafts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(
                login.to_owned(),
                conversation.to_owned(),
                thread.map(str::to_owned),
            ))
            .cloned()
    }

    /// Keeps `text` as the draft, or clears it when empty.
    pub(crate) fn set_draft(
        &self,
        login: &str,
        conversation: &str,
        thread: Option<&str>,
        text: &str,
    ) {
        let key = (
            login.to_owned(),
            conversation.to_owned(),
            thread.map(str::to_owned),
        );
        let mut drafts = self.0.drafts.lock().unwrap_or_else(PoisonError::into_inner);
        if text.trim().is_empty() {
            drafts.remove(&key);
        } else {
            drafts.insert(key, text.to_owned());
        }
    }

    /// `login`'s fold of `conversation` if it was made from `stamp`'s
    /// messages (their count and newest sequence number).
    pub(crate) fn cached_fold(
        &self,
        login: &str,
        conversation: &str,
        stamp: (usize, u64),
    ) -> Option<Arc<crate::fold::Folded>> {
        self.0
            .folds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(login.to_owned(), conversation.to_owned()))
            .filter(|(at, _)| *at == stamp)
            .map(|(_, f)| Arc::clone(f))
    }

    pub(crate) fn keep_fold(
        &self,
        login: &str,
        conversation: &str,
        stamp: (usize, u64),
        folded: Arc<crate::fold::Folded>,
    ) {
        self.0
            .folds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((login.to_owned(), conversation.to_owned()), (stamp, folded));
    }

    /// This computer is `device` in `org`, and reaches its console through
    /// `link`.
    pub fn connect(&self, org: &str, device: &str, link: Arc<dyn ChatLink>) {
        *self
            .0
            .connection
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(Connection {
            org: org.into(),
            device: device.into(),
            link,
        });
    }

    /// IT's compliance key from a verified Chat release: every epoch this
    /// computer makes is sealed to it too.
    pub fn set_compliance(&self, hex: &str, fingerprint: &str) -> Result<(), String> {
        let bytes = hex::decode(hex).map_err(|_| "the compliance key isn't hex")?;
        let key = PublicKey::from_bytes(&bytes).map_err(|e| e.to_string())?;
        *self
            .0
            .compliance
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(Compliance {
            key,
            hex: hex.to_owned(),
            fingerprint: fingerprint.to_owned(),
        });
        Ok(())
    }

    /// The compliance key's fingerprint, as every computer's Chat shows it.
    pub fn fingerprint(&self) -> Option<String> {
        self.0
            .compliance
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|c| c.fingerprint.clone())
    }

    /// Calls `f` with each notice, for the audit log.
    pub fn on_notice(&self, f: impl Fn(&Notice) + Send + Sync + 'static) {
        self.0
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Box::new(f));
    }

    fn notify(&self, notice: &Notice) {
        for listener in self
            .0
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            listener(notice);
        }
    }

    /// `login`'s chat changed on this computer, such as a post from an
    /// agent: their Chat hears of it.
    pub(crate) fn changed(&self, login: &str, kind: ConversationKind) {
        self.notify(&Notice::Changed {
            logins: vec![login.to_owned()],
            kinds: vec![kind],
        });
    }

    pub(crate) fn connection(&self) -> Result<Connection, ChatFailure> {
        self.0
            .connection
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| ChatFailure::NotSetUp(NOT_MANAGED.into()))
    }

    pub(crate) fn compliance(&self) -> Result<Compliance, ChatFailure> {
        self.connection()?;
        self.0
            .compliance
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| ChatFailure::NotSetUp(NOT_SET_UP.into()))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, ()> {
        self.0.write.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn person_dir(&self, login: &str) -> PathBuf {
        self.0.dir.join(login)
    }

    /// `login`'s store, to read: empty when there is none.
    ///
    /// Parsed once and kept: asked for again while its file is as it was,
    /// it is the same store, not read again. An action asks five to eight
    /// times, and the store is all of a person's chat.
    ///
    /// A file that isn't a store (cut short, damaged) is set aside beside
    /// itself and the store starts again; the console sends what it has
    /// at the next sync. One that is there and can't be read at all leaves
    /// the store empty for now, and nothing is saved over the file
    /// ([`Self::save`] refuses) until it reads again.
    pub(crate) fn load(&self, login: &str) -> Arc<Store> {
        let path = self.person_dir(login).join("store.json");
        let file = FileIs::of(&path);
        if let Some(file) = file
            && let Some(kept) = self
                .0
                .stores
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(login)
            && kept.file == file
        {
            return Arc::clone(&kept.store);
        }
        if file.is_some() {
            self.0.parses.fetch_add(1, Ordering::SeqCst);
        }
        let mut unread = self.0.unread.lock().unwrap_or_else(PoisonError::into_inner);
        let store = match glass_state::read_json_or_set_aside::<Store>(&path) {
            Ok(store) => {
                unread.remove(login);
                Arc::new(store.unwrap_or_default())
            }
            Err(e) => {
                unread.insert(login.to_owned(), e.to_string());
                return Arc::new(Store::default());
            }
        };
        drop(unread);
        // Kept as the file it was read from: one written since is read
        // again.
        if let Some(file) = file
            && FileIs::of(&path) == Some(file)
        {
            self.keep(login, file, Arc::clone(&store));
        }
        store
    }

    /// `login`'s store, to change and [`Self::save`]: a copy of its own.
    pub(crate) fn load_to_change(&self, login: &str) -> Store {
        Store::clone(&self.load(login))
    }

    fn keep(&self, login: &str, file: FileIs, store: Arc<Store>) {
        let mut stores = self.0.stores.lock().unwrap_or_else(PoisonError::into_inner);
        if stores.len() >= STORES_KEPT && !stores.contains_key(login) {
            stores.clear();
        }
        stores.insert(login.to_owned(), Kept { file, store });
    }

    /// How many times a store's file has been parsed.
    #[cfg(test)]
    pub(crate) fn parses(&self) -> u64 {
        self.0.parses.load(Ordering::SeqCst)
    }

    /// Tests only: from now, nothing can be written, as on a full disk.
    #[cfg(test)]
    pub(crate) fn fill_disk(&self, full: bool) {
        self.0.full.store(full, Ordering::SeqCst);
    }

    /// Saves `login`'s store, and appends what it kept for shared items to
    /// their logs first. Call with [`Self::lock`] held.
    pub(crate) fn save(&self, login: &str, store: &Store) -> Result<(), ChatFailure> {
        if let Some(why) = self
            .0
            .unread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(login)
        {
            return Err(ChatFailure::Local(format!(
                "chat on this computer couldn't be read, so nothing is written over it: {why}"
            )));
        }
        let mut logs: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
        for (item, message) in &store.item_pending {
            let line = logs.entry(item.as_str()).or_default();
            serde_json::to_writer(&mut *line, message)
                .map_err(|e| ChatFailure::Local(e.to_string()))?;
            line.push(b'\n');
        }
        for (item, lines) in logs {
            if !glass_managed::chat::is_conversation_id(item) {
                continue;
            }
            self.append_private(&self.item_log(login, item), &lines)
                .map_err(|e| {
                    ChatFailure::Local(format!("keeping a shared item on this computer: {e}"))
                })?;
        }
        let bytes = serde_json::to_vec(store).map_err(|e| ChatFailure::Local(e.to_string()))?;
        let path = self.person_dir(login).join("store.json");
        // Not the kept store while the file is being replaced.
        self.0
            .stores
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(login);
        self.write_private(&path, &bytes)
            .map_err(|e| ChatFailure::Local(format!("keeping chat on this computer: {e}")))?;
        // What was saved is what the next read finds: kept as that, with
        // what was pending now in the items' logs.
        if let Some(file) = FileIs::of(&path) {
            let mut saved = store.clone();
            saved.item_pending.clear();
            self.keep(login, file, Arc::new(saved));
        }
        Ok(())
    }

    fn item_log(&self, login: &str, item: &str) -> PathBuf {
        self.person_dir(login)
            .join("items")
            .join(format!("{item}.log"))
    }

    /// Item `item`'s messages on this computer for `login` after sequence
    /// number `after`, each once, in order, with any from before items had
    /// logs.
    pub(crate) fn item_messages(
        &self,
        login: &str,
        item: &str,
        store: &Store,
        after: u64,
    ) -> Vec<Message> {
        let mut seen = BTreeSet::new();
        let mut out: Vec<Message> = store.messages.get(item).cloned().unwrap_or_default();
        for m in &out {
            if let Ok(h) = m.header() {
                seen.insert(h.id);
            }
        }
        out.retain(|m| m.seq > after);
        if is_conversation_id(item) {
            let mut logs = self
                .0
                .item_logs
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let log = logs.entry((login.to_owned(), item.to_owned())).or_default();
            let path = self.item_log(login, item);
            let new = Self::read_item_log(&path, log, after).unwrap_or_default();
            out.extend(
                new.into_iter()
                    .filter(|m| seen.is_empty() || m.header().is_ok_and(|h| !seen.contains(&h.id))),
            );
        }
        out.sort_by_key(|m| m.seq);
        out
    }

    /// Indexes what was appended to the log at `path` since `log` last read
    /// it, then reads the messages after sequence number `after`.
    fn read_item_log(path: &Path, log: &mut ItemLog, after: u64) -> std::io::Result<Vec<Message>> {
        let mut file = match fs::File::open(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            other => other?,
        };
        let len = file.metadata()?.len();
        if len < log.read {
            // Not the log it was: read it all again.
            *log = ItemLog::default();
        }
        if len > log.read {
            let mut bytes = Vec::new();
            file.seek(SeekFrom::Start(log.read))?;
            file.read_to_end(&mut bytes)?;
            // Whole lines only: an append under way is read next time.
            let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |n| n + 1);
            let mut offset = 0;
            for line in bytes[..whole].split_inclusive(|b| *b == b'\n') {
                let at = log.read + offset as u64;
                offset += line.len();
                let Ok(message) = serde_json::from_slice::<Message>(line) else {
                    continue;
                };
                if message.header().is_ok_and(|h| log.ids.insert(h.id)) {
                    let place = log.at.partition_point(|(seq, ..)| *seq <= message.seq);
                    log.at.insert(place, (message.seq, at, line.len()));
                }
            }
            log.read += whole as u64;
        }
        let from = log.at.partition_point(|(seq, ..)| *seq <= after);
        let wanted = &log.at[from..];
        let Some(start) = wanted.iter().map(|(_, at, _)| *at).min() else {
            return Ok(Vec::new());
        };
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(start))?;
        file.take(log.read - start).read_to_end(&mut bytes)?;
        Ok(wanted
            .iter()
            .filter_map(|(_, at, n)| {
                let from = usize::try_from(at - start).ok()?;
                serde_json::from_slice(bytes.get(from..from + n)?).ok()
            })
            .collect())
    }

    /// Appends `bytes` to `path`, root only, in folders only root can open.
    fn append_private(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_data()
    }

    /// Where this computer's syncs have reached: every message up to it is
    /// kept for each person here (ADR 0052).
    pub(crate) fn cursor(&self) -> u64 {
        fs::read_to_string(self.0.dir.join("cursor"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn set_cursor(&self, cursor: u64) {
        if let Err(e) =
            self.write_private(&self.0.dir.join("cursor"), cursor.to_string().as_bytes())
        {
            self.notify(&Notice::Refused {
                login: String::new(),
                conversation: None,
                reason: format!("couldn't keep chat's place: {e}"),
            });
        }
    }

    /// `login`'s keys on this computer, made and registered with the
    /// console the first time they use chat (ADR 0030, decision 1).
    pub(crate) fn member(&self, login: &str) -> Result<Member, ChatFailure> {
        if !plain_login(login) {
            return Err(ChatFailure::NotSetUp(format!(
                "{login:?} can't use chat: chat needs an account name of letters, digits, '.', '_' and '-'."
            )));
        }
        let connection = self.connection()?;
        let path = self.person_dir(login).join("member.key");
        let notice;
        let member = {
            let _write = self.lock();
            let secret = match fs::read(&path) {
                Ok(bytes) => MemberSecret::from_bytes(&bytes).map_err(|e| {
                    ChatFailure::Local(format!("this computer's chat key for {login}: {e}"))
                })?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let secret = MemberSecret::generate()?;
                    self.write_private(&path, &secret.to_bytes()?)
                        .map_err(|e| ChatFailure::Local(format!("keeping a chat key: {e}")))?;
                    secret
                }
                Err(e) => {
                    return Err(ChatFailure::Local(format!(
                        "reading this computer's chat key for {login}: {e}"
                    )));
                }
            };
            let key = secret.public(login, &connection.device)?;
            let mut store = self.load_to_change(login);
            notice = if store.registered.as_deref() == Some(key.id.as_str()) {
                None
            } else {
                connection
                    .link
                    .register(&RegisterKey { key: key.clone() })
                    .map_err(ChatFailure::Link)?;
                store.registered = Some(key.id.clone());
                store.keys.insert(key.id.clone(), key.clone());
                self.save(login, &store)?;
                // Fetch this person's conversations from the beginning.
                self.set_cursor(0);
                self.0.resets.fetch_add(1, Ordering::SeqCst);
                Some(Notice::KeyMade {
                    login: login.to_owned(),
                    key: key.id.clone(),
                })
            };
            Member { key, secret }
        };
        if let Some(notice) = notice {
            self.notify(&notice);
        }
        Ok(member)
    }

    /// `login`'s keys if Chat made and registered them, without reaching
    /// the console.
    pub(crate) fn existing(&self, login: &str) -> Option<Member> {
        let connection = self.connection().ok()?;
        if !plain_login(login) {
            return None;
        }
        let bytes = fs::read(self.person_dir(login).join("member.key")).ok()?;
        let secret = MemberSecret::from_bytes(&bytes).ok()?;
        let key = secret.public(login, &connection.device).ok()?;
        (self.load(login).registered.as_deref() == Some(key.id.as_str()))
            .then_some(Member { key, secret })
    }

    /// The people with a registered member key on this computer.
    pub fn logins(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.0.dir) else {
            return Vec::new();
        };
        let mut logins: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|login| plain_login(login) && self.load(login).registered.is_some())
            .collect();
        logins.sort();
        logins
    }

    /// How long the organization keeps chat (CF-61), from IT's Chat
    /// release. From here on nothing older is kept from a sync, and
    /// [`ChatHandle::trim`] removes what is here already.
    pub fn set_kept_for(&self, kept_for: KeptFor) {
        *self
            .0
            .kept_for
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = kept_for;
    }

    /// Blob `id` of `conversation`, if this computer sent or fetched it
    /// and `login` is in the conversation as far as this computer knows:
    /// the console's own check for a blob, made here for one it isn't
    /// asked for again.
    pub(crate) fn kept_blob(
        &self,
        login: &str,
        conversation: &str,
        id: &str,
    ) -> Option<SealedBlob> {
        let member = self
            .load(login)
            .conversations
            .get(conversation)
            .is_some_and(|c| c.members.iter().any(|m| m == login));
        if !member {
            return None;
        }
        self.0
            .blobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(conversation, id)
    }

    /// Keeps a sealed blob this computer sent or fetched.
    pub(crate) fn keep_blob(&self, blob: &SealedBlob) {
        self.0
            .blobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keep(blob.clone());
    }

    /// The periods in force here.
    pub fn kept_for(&self) -> KeptFor {
        *self
            .0
            .kept_for
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Removes the chat older than the organization keeps it from every
    /// person's store on this computer, and says so (a [`Notice::Trimmed`],
    /// which glassd puts on the audit record). How many messages went.
    /// glassd calls it when IT's periods arrive and every hour; with none
    /// set, nothing is ever removed.
    ///
    /// Chat is channels, direct messages and groups. A doc's, an
    /// artifact's or a skill's messages are its content, and a meeting's
    /// its record: none of them is touched.
    pub fn trim(&self) -> usize {
        self.trim_as_of(Timestamp::now())
    }

    /// [`ChatHandle::trim`] as it would be at `now`.
    #[doc(hidden)]
    pub fn trim_as_of(&self, now: Timestamp) -> usize {
        let kept_for = self.kept_for();
        if !kept_for.is_set() {
            return 0;
        }
        let mut removed = 0;
        let mut changed = Vec::new();
        {
            let _write = self.lock();
            for login in self.logins() {
                let mut store = self.load_to_change(&login);
                let gone = store.trim(kept_for, now.unix_seconds());
                if gone == 0 {
                    continue;
                }
                // One that can't be saved now (a full disk) is as it was,
                // and is tried again at the next trim.
                if self.save(&login, &store).is_ok() {
                    removed += gone;
                    changed.push(login);
                }
            }
        }
        if removed > 0 {
            self.notify(&Notice::Trimmed { removed, kept_for });
            // Open Chat windows show what is here now.
            self.notify(&Notice::Changed {
                logins: changed,
                kinds: vec![
                    ConversationKind::Direct,
                    ConversationKind::Group,
                    ConversationKind::Channel,
                ],
            });
        }
        removed
    }

    /// One round with the console: waits up to `wait_secs` for news for
    /// this computer's people and keeps what checks out. Whether anything
    /// new arrived. A store that couldn't be kept is an error, so glassd
    /// backs off, and the next round asks from the same place.
    pub fn sync_once(&self, wait_secs: u64) -> Result<bool, LinkError> {
        let connection = self
            .connection()
            .map_err(|e| LinkError::Unreachable(e.to_string()))?;
        // Before the logins: someone starting chat after this is read
        // isn't asked about, so their reset of the cursor must stand.
        let resets = self.0.resets.load(Ordering::SeqCst);
        let logins = self.logins();
        if logins.is_empty() {
            return Ok(false);
        }
        let since = self.cursor();
        let response = connection.link.sync(&ChatSyncRequest {
            device: connection.device.clone(),
            logins: logins.clone(),
            since,
            wait_secs,
        })?;
        let mut notices = Vec::new();
        let mut changed = Vec::new();
        let mut kinds = Vec::new();
        let mut unkept = None;
        {
            let _write = self.lock();
            for login in &logins {
                let mut store = self.load_to_change(login);
                let mut refused = Vec::new();
                let news = store.absorb(&connection.org, login, &response, &mut refused);
                // Nothing past the organization's periods is kept,
                // whatever the console still had of it: its own pass is
                // once a day, and an answer may have been on its way
                // since before a message aged (CF-61, ChatSync.tla).
                let kept_for = self.kept_for();
                if kept_for.is_set() {
                    store.trim(kept_for, Timestamp::now().unix_seconds());
                }
                for kind in &news {
                    if !kinds.contains(kind) {
                        kinds.push(*kind);
                    }
                }
                let news = !news.is_empty();
                if news || !refused.is_empty() {
                    if let Err(e) = self.save(login, &store) {
                        refused.push((None, e.to_string()));
                        unkept = Some(e.to_string());
                    } else if news {
                        changed.push(login.clone());
                    }
                }
                notices.extend(
                    refused
                        .into_iter()
                        .map(|(conversation, reason)| Notice::Refused {
                            login: login.clone(),
                            conversation,
                            reason,
                        }),
                );
            }
            // The cursor moves only past what every person here kept, and
            // stays at the beginning for someone who started chat while this
            // waited. Another round may have moved it meanwhile; the newer
            // place stands (ChatSync.tla).
            let now = self.cursor();
            if unkept.is_none()
                && self.0.resets.load(Ordering::SeqCst) == resets
                && (now == since || response.cursor > now)
            {
                self.set_cursor(response.cursor);
            }
        }
        if !changed.is_empty() {
            notices.push(Notice::Changed {
                logins: changed.clone(),
                kinds,
            });
        }
        for notice in &notices {
            self.notify(notice);
        }
        if let Some(e) = unkept {
            return Err(LinkError::Refused(e));
        }
        Ok(!changed.is_empty())
    }

    /// Writes `bytes` to `path` whole, root only, in folders only root
    /// can open.
    pub(crate) fn write_private(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        if self.0.full.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "no space left on device (a test's)",
            ));
        }
        // Beside it, synced, renamed over it, the folder synced: a power
        // cut leaves the old file or the new one (glass-state).
        glass_state::write(path, bytes, glass_state::PRIVATE)
    }
}

fn hashed(header: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    header.hash(&mut hasher);
    hasher.finish()
}

fn short(s: &str) -> &str {
    s.get(..16).unwrap_or(s)
}

impl Store {
    pub(crate) fn epoch(&self, conversation: &str, number: u32) -> Option<&Epoch> {
        self.epochs
            .get(conversation)?
            .iter()
            .find(|e| e.body.number == number)
    }

    /// The key of `conversation`'s epoch `number`, opened with `member`'s
    /// secret.
    pub(crate) fn epoch_key(
        &self,
        member: &Member,
        conversation: &str,
        number: u32,
    ) -> Option<EpochKey> {
        if let Ok(key) = self
            .epoch(conversation, number)?
            .open(&member.key.id, &member.secret.agree)
        {
            return Some(key);
        }
        // Someone added later holds a later epoch's key, which opens every
        // earlier one through the backward chain (ADR 0030 §1).
        let mut later: Vec<&Epoch> = self
            .epochs
            .get(conversation)?
            .iter()
            .filter(|e| e.body.number > number)
            .collect();
        later.sort_by_key(|e| e.body.number);
        let start = later.iter().find_map(|e| {
            e.open(&member.key.id, &member.secret.agree)
                .ok()
                .map(|k| (e.body.number, k))
        })?;
        let (mut at, mut key) = start;
        while at > number {
            key = self.epoch(conversation, at)?.open_previous(&key).ok()?;
            at -= 1;
        }
        Some(key)
    }

    /// Checks the signed membership chain that takes `target` from what
    /// this computer has (or from its first epoch, for someone added) to
    /// the members and epoch the console says, through `offered` and the
    /// epochs kept (ADR 0030 C2). The epochs up to `target`'s, whole.
    fn replay(&self, target: &Conversation, offered: &[Epoch]) -> Result<Vec<Epoch>, String> {
        let mut all: BTreeMap<u32, Epoch> = BTreeMap::new();
        for e in self
            .epochs
            .get(&target.id)
            .into_iter()
            .flatten()
            .chain(offered.iter().filter(|e| e.body.conversation == target.id))
        {
            match all.get(&e.body.number) {
                Some(have) if have != e => {
                    return Err(format!(
                        "two different epochs {} were offered",
                        e.body.number
                    ));
                }
                _ => {
                    all.insert(e.body.number, e.clone());
                }
            }
        }
        let (mut members, mut number) = match self.conversations.get(&target.id) {
            Some(have) => (have.members.clone(), have.epoch),
            None => {
                let first = all.get(&1).ok_or("its first epoch is missing")?;
                let maker = self
                    .keys
                    .get(&first.body.made_by)
                    .ok_or("its first epoch's maker is unknown")?;
                if maker.login != target.created_by || first.verify(maker).is_err() {
                    return Err("its first epoch isn't its maker's".into());
                }
                let mut m = first
                    .body
                    .envelopes
                    .iter()
                    .filter(|e| e.key != glass_managed::chat::COMPLIANCE)
                    .map(|e| self.keys.get(&e.key).map(|k| k.login.clone()))
                    .collect::<Option<Vec<String>>>()
                    .ok_or("this computer can't tell who was in it first")?;
                m.sort();
                m.dedup();
                (m, 1)
            }
        };
        while number < target.epoch {
            let next = all
                .get(&(number + 1))
                .ok_or("an epoch in its chain is missing")?;
            let maker = self
                .keys
                .get(&next.body.made_by)
                .ok_or("an epoch's maker is unknown")?;
            next.verify(maker)
                .map_err(|_| "an epoch's signature doesn't check".to_owned())?;
            members = glass_managed::chat::check_change(
                target.kind,
                &members,
                number,
                next,
                &maker.login,
            )
            .map_err(|e| e.to_string())?;
            number += 1;
        }
        let mut want = target.members.clone();
        want.sort();
        if members != want || number != target.epoch {
            return Err("the console changed who is in it without a member's signature".into());
        }
        Ok(all
            .into_values()
            .filter(|e| e.body.number <= number)
            .collect())
    }

    /// A channel's name, opened.
    pub(crate) fn name(&self, member: &Member, conversation: &Conversation) -> Option<String> {
        let sealed = conversation.name.as_deref()?;
        let key = self.epoch_key(member, &conversation.id, 1)?;
        glass_managed::chat::open_name(&key, &conversation.id, 1, sealed).ok()
    }

    /// A meeting's title and time, opened (CF-21).
    pub(crate) fn meeting(
        &self,
        member: &Member,
        conversation: &Conversation,
    ) -> Option<glass_managed::meet::MeetingDetails> {
        let sealed = conversation.name.as_deref()?;
        let key = self.epoch_key(member, &conversation.id, 1)?;
        glass_managed::meet::MeetingDetails::open(&key, &conversation.id, sealed).ok()
    }

    /// Keeps `message`, in sequence order, once.
    pub(crate) fn keep(&mut self, conversation: &str, message: Message) {
        if self
            .messages
            .get(conversation)
            .is_some_and(|list| list.iter().any(|m| m.header == message.header))
        {
            return;
        }
        self.keep_new(conversation, message);
    }

    /// Removes the chat that is past `kept_for` at `now` (seconds since
    /// 1970): the messages of channels, direct messages and groups,
    /// whatever they carry (an edit, a reaction, a pin and a topic are
    /// messages too). What refers to a message that's gone is shown as it
    /// is for one that never arrived: not at all, and a reply on its own
    /// (fold.rs). How many went.
    pub(crate) fn trim(&mut self, kept_for: KeptFor, now: i64) -> usize {
        let mut removed = 0;
        for (id, list) in &mut self.messages {
            let Some(kind) = self.conversations.get(id).map(|c| c.kind) else {
                continue;
            };
            if kept_for.days(kind).is_none() {
                continue;
            }
            let had = list.len();
            list.retain(|m| !kept_for.is_past(kind, m, now));
            removed += had - list.len();
        }
        removed
    }

    /// Keeps `message`, which isn't in the conversation yet, in sequence
    /// order.
    fn keep_new(&mut self, conversation: &str, message: Message) {
        // A shared item's messages go to its own log (ADR 0035 §4).
        if self
            .conversations
            .get(conversation)
            .is_some_and(|c| c.kind.is_item())
        {
            let Ok(header) = message.header() else {
                return;
            };
            let recent = self.item_recent.entry(conversation.to_owned()).or_default();
            if recent.contains(&header.id) {
                return;
            }
            recent.push(header.id);
            if recent.len() > ITEM_RECENT {
                recent.remove(0);
            }
            self.item_pending.push((conversation.to_owned(), message));
            return;
        }
        let list = self.messages.entry(conversation.to_owned()).or_default();
        let at = list.partition_point(|m| m.seq <= message.seq);
        list.insert(at, message);
    }

    fn refuse(
        &mut self,
        out: &mut Vec<(Option<String>, String)>,
        mark: String,
        conversation: Option<&str>,
        reason: String,
    ) {
        if self.refused.insert(mark) {
            out.push((conversation.map(str::to_owned), reason));
        }
    }

    /// Keeps what in `response` is `login`'s and checks out; adds what
    /// doesn't to `refused`. The kinds of conversation something new was
    /// kept in.
    pub(crate) fn absorb(
        &mut self,
        org: &str,
        login: &str,
        response: &ChatSyncResponse,
        refused: &mut Vec<(Option<String>, String)>,
    ) -> Vec<ConversationKind> {
        let mut news = Vec::new();
        let mut note = |kind: ConversationKind| {
            if !news.contains(&kind) {
                news.push(kind);
            }
        };
        for key in &response.keys {
            // An id is its keys' hash, so the first record for one stands.
            if key.keys().is_ok() && !self.keys.contains_key(&key.id) {
                self.keys.insert(key.id.clone(), key.clone());
            }
        }
        let mut changed: Vec<&Conversation> = Vec::new();
        for conversation in &response.conversations {
            if !conversation.members.iter().any(|m| m == login) {
                continue;
            }
            if !is_conversation_id(&conversation.id) {
                self.refuse(
                    refused,
                    format!("c:{}", short(&conversation.id)),
                    None,
                    "a conversation's id is malformed".into(),
                );
                continue;
            }
            match self.conversations.get(&conversation.id) {
                Some(have) if have == conversation => {}
                // What a conversation is never changes; who is in it changes
                // only by a member's signed epoch (ADR 0030 C2).
                Some(have)
                    if have.kind != conversation.kind
                        || have.created_by != conversation.created_by
                        || have.name != conversation.name
                        || have.label != conversation.label =>
                {
                    self.refuse(
                        refused,
                        format!("c:{}:{}", conversation.id, conversation.members.join(",")),
                        Some(&conversation.id),
                        "the console changed what a conversation is".into(),
                    );
                }
                Some(_) => changed.push(conversation),
                None if conversation.epoch == 1 => {
                    self.conversations
                        .insert(conversation.id.clone(), conversation.clone());
                    note(conversation.kind);
                }
                // Someone added later checks the chain from the start.
                None => changed.push(conversation),
            }
        }
        for epoch in &response.epochs {
            let id = epoch.body.conversation.as_str();
            let Some(conversation) = self.conversations.get(id) else {
                continue;
            };
            // A change is checked with its chain, below.
            if self.epoch(id, epoch.body.number).is_some() || epoch.body.number > conversation.epoch
            {
                continue;
            }
            let signed = self.keys.get(&epoch.body.made_by).is_some_and(|maker| {
                conversation.members.contains(&maker.login)
                    && (epoch.body.number != 1 || maker.login == conversation.created_by)
                    && epoch.verify(maker).is_ok()
            });
            if signed {
                note(conversation.kind);
                self.epochs
                    .entry(id.to_owned())
                    .or_default()
                    .push(epoch.clone());
            } else {
                let id = id.to_owned();
                self.refuse(
                    refused,
                    format!("e:{id}:{}:{}", epoch.body.number, short(&epoch.signature)),
                    Some(&id),
                    format!(
                        "epoch {} isn't signed by a member of the conversation",
                        epoch.body.number
                    ),
                );
            }
        }
        for conversation in changed {
            match self.replay(conversation, &response.epochs) {
                Ok(epochs) => {
                    self.epochs.insert(conversation.id.clone(), epochs);
                    self.conversations
                        .insert(conversation.id.clone(), conversation.clone());
                    note(conversation.kind);
                }
                Err(reason) => self.refuse(
                    refused,
                    format!(
                        "c:{}:{}:{}",
                        conversation.id,
                        conversation.epoch,
                        conversation.members.join(",")
                    ),
                    Some(&conversation.id),
                    reason,
                ),
            }
        }
        // Each conversation's headers, hashed once per sync: a whole
        // history arriving (someone new here, or added) is checked against
        // what's kept without comparing every pair.
        let mut kept: HashMap<String, HashSet<u64>> = HashMap::new();
        for message in &response.messages {
            let Ok(header) = message.header() else {
                self.refuse(
                    refused,
                    format!("m:{}", short(&message.signature)),
                    None,
                    "a message's header is malformed".into(),
                );
                continue;
            };
            let Some(conversation) = self.conversations.get(&header.conversation) else {
                continue;
            };
            let hashes = kept.entry(header.conversation.clone()).or_insert_with(|| {
                self.messages
                    .get(&header.conversation)
                    .into_iter()
                    .flatten()
                    .map(|m| hashed(&m.header))
                    .collect()
            });
            let hash = hashed(&message.header);
            if hashes.contains(&hash)
                && self
                    .messages
                    .get(&header.conversation)
                    .is_some_and(|list| list.iter().any(|m| m.header == message.header))
            {
                continue;
            }
            let checks = header.org == org
                && conversation.members.contains(&header.sender)
                && self.epoch(&header.conversation, header.epoch).is_some()
                && self
                    .keys
                    .get(&header.key)
                    .is_some_and(|k| message.verify(k).is_ok());
            if checks {
                note(conversation.kind);
                hashes.insert(hash);
                self.keep_new(&header.conversation, message.clone());
            } else {
                self.refuse(
                    refused,
                    format!("m:{}", short(&message.signature)),
                    Some(&header.conversation),
                    format!(
                        "a message said to be from {} doesn't check against their key",
                        header.sender
                    ),
                );
            }
        }
        news
    }
}

#[cfg(test)]
mod blobs_kept {
    use super::*;

    fn blob(n: usize, bytes: usize) -> SealedBlob {
        SealedBlob {
            id: format!("b-{n}"),
            conversation: "cv-1".into(),
            nonce: "0".repeat(24),
            ct: "a".repeat(bytes),
        }
    }

    /// The blobs a computer keeps have a size: past it the one used
    /// longest ago goes, and one just read is the last to go.
    #[test]
    fn ar2_kept_blobs_have_a_size_and_the_one_used_longest_ago_goes_first() {
        let mut blobs = Blobs::default();
        let each = BLOBS_KEPT_BYTES / 4;
        for n in 0..4 {
            blobs.keep(blob(n, each - 100));
        }
        assert_eq!(blobs.held.len(), 4);
        // The same blob again is the same blob.
        blobs.keep(blob(0, each - 100));
        assert_eq!(blobs.held.len(), 4);
        // The first is read, so the second is now the one used longest ago.
        assert!(blobs.get("cv-1", "b-0").is_some());
        assert!(blobs.get("cv-2", "b-0").is_none(), "another item's");
        blobs.keep(blob(4, each - 100));
        assert!(blobs.bytes <= BLOBS_KEPT_BYTES);
        assert!(blobs.get("cv-1", "b-1").is_none(), "it went");
        for n in [0, 2, 3, 4] {
            assert!(blobs.get("cv-1", &format!("b-{n}")).is_some(), "b-{n}");
        }
        // One larger than all the room there is isn't kept, nor anything
        // because of it.
        blobs.keep(blob(9, BLOBS_KEPT_BYTES + 1));
        assert!(blobs.bytes <= BLOBS_KEPT_BYTES);
        assert!(blobs.get("cv-1", "b-9").is_none());
        assert_eq!(blobs.held.len(), 4, "the others are still here");
    }
}
