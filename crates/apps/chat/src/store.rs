//! glassd's side of chat (ADR 0030, decision 2): each person's member key
//! and the sealed records the console sent for them, each checked before
//! it is kept. All of it is root's, in glassd's state directory
//! (`<state>/chat/<login>/`), never in a home, so a person's own programs
//! and agents reach chat only through actions.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use glass_crypto::{EpochKey, PublicKey};
use glass_managed::chat::{
    ChatLink, ChatSyncRequest, ChatSyncResponse, Conversation, Epoch, LinkError, MemberKey,
    MemberSecret, Message, RegisterKey, is_conversation_id,
};
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
    /// New conversations or messages arrived for these people.
    Changed { logins: Vec<String> },
}

/// Why chat couldn't do something, in words for a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatFailure {
    /// Not managed, or IT hasn't set chat up: trying again won't help.
    NotSetUp(String),
    /// The console couldn't be reached, or refused.
    Link(LinkError),
    /// Something on this computer.
    Local(String),
}

impl std::fmt::Display for ChatFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSetUp(m) | Self::Local(m) | Self::Link(LinkError::Refused(m)) => {
                f.write_str(m)
            }
            Self::Link(LinkError::Unreachable(m)) => write!(
                f,
                "your organization's server can't be reached ({m}). Nothing was queued; try again when it's back"
            ),
        }
    }
}

impl From<glass_managed::chat::ChatError> for ChatFailure {
    fn from(e: glass_managed::chat::ChatError) -> Self {
        Self::Local(e.to_string())
    }
}

impl From<glass_crypto::CryptoError> for ChatFailure {
    fn from(e: glass_crypto::CryptoError) -> Self {
        Self::Local(e.to_string())
    }
}

pub(crate) const NOT_MANAGED: &str =
    "This computer isn't managed by an organization, so there's no chat.";
pub(crate) const NOT_SET_UP: &str = "Your organization hasn't set chat up yet: IT makes its compliance key in the admin console first.";

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

struct Inner {
    dir: PathBuf,
    connection: RwLock<Option<Connection>>,
    compliance: RwLock<Option<Compliance>>,
    /// Writers to the stores, key files and cursor, one at a time.
    write: Mutex<()>,
    listeners: Mutex<Vec<Listener>>,
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
#[derive(Debug, Default, Serialize, Deserialize)]
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
    /// Records already refused, so each is audited once.
    #[serde(default)]
    pub refused: BTreeSet<String>,
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

impl ChatHandle {
    /// Chat kept in `dir`, which glassd owns. Until [`Self::connect`],
    /// every action says this computer has no chat.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            dir: dir.into(),
            connection: RwLock::new(None),
            compliance: RwLock::new(None),
            write: Mutex::new(()),
            listeners: Mutex::new(Vec::new()),
        }))
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
    pub(crate) fn changed(&self, login: &str) {
        self.notify(&Notice::Changed {
            logins: vec![login.to_owned()],
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

    /// `login`'s store; empty when there is none or it doesn't parse.
    pub(crate) fn load(&self, login: &str) -> Store {
        fs::read(self.person_dir(login).join("store.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Saves `login`'s store. Call with [`Self::lock`] held.
    pub(crate) fn save(&self, login: &str, store: &Store) -> Result<(), ChatFailure> {
        let bytes = serde_json::to_vec(store).map_err(|e| ChatFailure::Local(e.to_string()))?;
        self.write_private(&self.person_dir(login).join("store.json"), &bytes)
            .map_err(|e| ChatFailure::Local(format!("keeping chat on this computer: {e}")))
    }

    fn cursor(&self) -> u64 {
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
            let mut store = self.load(login);
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

    /// One round with the console: waits up to `wait_secs` for news for
    /// this computer's people and keeps what checks out. Whether anything
    /// new arrived.
    pub fn sync_once(&self, wait_secs: u64) -> Result<bool, LinkError> {
        let connection = self
            .connection()
            .map_err(|e| LinkError::Unreachable(e.to_string()))?;
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
        {
            let _write = self.lock();
            for login in &logins {
                let mut store = self.load(login);
                let mut refused = Vec::new();
                let news = store.absorb(&connection.org, login, &response, &mut refused);
                if news || !refused.is_empty() {
                    if let Err(e) = self.save(login, &store) {
                        refused.push((None, e.to_string()));
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
            // A person who started chat while this waited reset the place
            // to the beginning; keep that.
            if self.cursor() == since {
                self.set_cursor(response.cursor);
            }
        }
        if !changed.is_empty() {
            notices.push(Notice::Changed {
                logins: changed.clone(),
            });
        }
        for notice in &notices {
            self.notify(notice);
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
        self.epoch(conversation, number)?
            .open(&member.key.id, &member.secret.agree)
            .ok()
    }

    /// A channel's name, opened.
    pub(crate) fn name(&self, member: &Member, conversation: &Conversation) -> Option<String> {
        let sealed = conversation.name.as_deref()?;
        let key = self.epoch_key(member, &conversation.id, 1)?;
        glass_managed::chat::open_name(&key, &conversation.id, 1, sealed).ok()
    }

    /// Keeps `message`, in sequence order, once.
    pub(crate) fn keep(&mut self, conversation: &str, message: Message) {
        let list = self.messages.entry(conversation.to_owned()).or_default();
        if list.iter().any(|m| m.header == message.header) {
            return;
        }
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
    /// doesn't to `refused`. Whether anything new was kept.
    pub(crate) fn absorb(
        &mut self,
        org: &str,
        login: &str,
        response: &ChatSyncResponse,
        refused: &mut Vec<(Option<String>, String)>,
    ) -> bool {
        let mut news = false;
        for key in &response.keys {
            // An id is its keys' hash, so the first record for one stands.
            if key.keys().is_ok() && !self.keys.contains_key(&key.id) {
                self.keys.insert(key.id.clone(), key.clone());
            }
        }
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
                // People are fixed when a conversation is made (slice C1);
                // the console changing them alone is refused (ADR 0030).
                Some(have)
                    if have.members != conversation.members
                        || have.kind != conversation.kind
                        || have.created_by != conversation.created_by
                        || have.name != conversation.name =>
                {
                    self.refuse(
                        refused,
                        format!("c:{}:{}", conversation.id, conversation.members.join(",")),
                        Some(&conversation.id),
                        "the console changed who is in it without a member's signature".into(),
                    );
                }
                _ => {
                    self.conversations
                        .insert(conversation.id.clone(), conversation.clone());
                    news = true;
                }
            }
        }
        for epoch in &response.epochs {
            let id = epoch.body.conversation.as_str();
            let Some(conversation) = self.conversations.get(id) else {
                continue;
            };
            if self.epoch(id, epoch.body.number).is_some() {
                continue;
            }
            let signed = self.keys.get(&epoch.body.made_by).is_some_and(|maker| {
                conversation.members.contains(&maker.login)
                    && (epoch.body.number != 1 || maker.login == conversation.created_by)
                    && epoch.verify(maker).is_ok()
            });
            if signed {
                self.epochs
                    .entry(id.to_owned())
                    .or_default()
                    .push(epoch.clone());
                news = true;
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
            if self
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
                self.keep(&header.conversation, message.clone());
                news = true;
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
