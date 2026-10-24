//! Chat on our servers (ADR 0030, slice C1): what the console keeps and
//! relays for the org's computers without being able to read it.
//!
//! It holds member keys, who is in which conversation, each epoch with the
//! keys sealed to members' computers and to IT, and sealed messages in
//! order. It checks what it can without reading: that a key is registered
//! to its sender on the computer posting, that the sender is a member,
//! that the epoch is current, that the signature holds, that an epoch's
//! envelopes are exactly its members' keys and IT's. It never holds a
//! name, a topic, a message or a key that opens one (GT-3).
//!
//! Slice C1 runs on the enrollment token (signed device requests come with
//! ADR 0028's device keys) and allows one computer per person.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::Path;

use anyhow::Context;

use glass_managed::chat::{
    COMPLIANCE, ChatSyncRequest, ChatSyncResponse, Conversation, ConversationKind,
    CreateConversation, Epoch, Keys, MAX_MEMBERS, MemberKey, Message, is_conversation_id,
};
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

use crate::directory::Directory;
use crate::state::Refused;

/// A conversation with its epochs, as the console keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub conversation: Conversation,
    pub epochs: Vec<Epoch>,
    /// The sequence number its creation took, so a waiting computer hears
    /// of it.
    pub created_seq: u64,
    /// The sequence number of its last change of members, likewise.
    #[serde(default)]
    pub changed_seq: u64,
    /// People added after it was made, with the sequence number they were
    /// added at: their computers get the whole history (ADR 0030 C2).
    #[serde(default)]
    pub joined: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatState {
    #[serde(default)]
    pub keys: Vec<MemberKey>,
    #[serde(default)]
    pub conversations: BTreeMap<String, Stored>,
    /// Sealed messages, by sequence number. They're kept in their own log
    /// (`chat-messages.jsonl`), a line each as it's posted, so a post
    /// writes one line, not the org's history; consoles before ADR 0052
    /// kept them here, and [`open_log`] moves them.
    #[serde(default, skip_serializing)]
    pub messages: Vec<Message>,
    /// Each posted message's conversation and id, made once from the
    /// history, so a post isn't checked against every message.
    #[serde(skip)]
    posted: Posted,
    /// The last sequence number given out, across the org.
    #[serde(default)]
    pub seq: u64,
    /// IT's compliance key (hex), and the signed release that carries it to
    /// every computer (ADR 0030, decision 4).
    #[serde(default)]
    pub compliance: String,
    #[serde(default)]
    pub release: Option<glass_managed::Signed>,
}

fn rule(m: impl Into<String>) -> Refused {
    Refused::Rule(m.into())
}

fn invalid(m: impl Into<String>) -> Refused {
    Refused::Invalid(vec![m.into()])
}

/// An index of [`ChatState::messages`], not state of its own: two states
/// with the same messages have the same one.
#[derive(Debug, Clone, Default)]
struct Posted(Option<HashSet<(String, String)>>);

impl PartialEq for Posted {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// Reads the chat log at `path` into `chat` after what `chat.json` held.
/// Messages an older console kept in `chat.json` are written to the log
/// first (the caller then saves `chat.json` without them), and a line a
/// crash cut short is dropped.
pub fn open_log(chat: &mut ChatState, path: &Path) -> anyhow::Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let torn = !text.is_empty() && !text.ends_with('\n');
    let mut logged = Vec::new();
    for line in text.split_inclusive('\n') {
        match serde_json::from_str::<Message>(line.trim_end()) {
            Ok(message) => logged.push(message),
            Err(_) if torn && !line.ends_with('\n') => {}
            Err(e) => return Err(e).with_context(|| format!("parsing {}", path.display())),
        }
    }
    let moving = !chat.messages.is_empty();
    let last = chat.messages.last().map_or(0, |m| m.seq);
    chat.messages
        .extend(logged.into_iter().filter(|m| m.seq > last));
    chat.seq = chat.seq.max(chat.messages.last().map_or(0, |m| m.seq));
    if moving || torn {
        let mut whole = Vec::new();
        for message in &chat.messages {
            serde_json::to_writer(&mut whole, message)?;
            whole.push(b'\n');
        }
        let tmp = path.with_extension("jsonl.new");
        std::fs::write(&tmp, whole).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(moving)
}

/// Appends `message` to the chat log at `path`.
pub fn append_log(path: &Path, message: &Message) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(&line))
        .with_context(|| format!("writing {}", path.display()))
}

impl ChatState {
    pub(crate) fn key(&self, id: &str) -> Option<&MemberKey> {
        self.keys.iter().find(|k| k.id == id)
    }

    /// Registers `key` for its login on `device`.
    pub fn register(
        &mut self,
        device: &str,
        key: MemberKey,
        directory: &dyn Directory,
    ) -> Result<(), Refused> {
        if key.device != device {
            return Err(invalid("a computer registers its own keys"));
        }
        if !directory.users().iter().any(|u| u.name == key.login) {
            return Err(rule(format!("{} isn't in the org's directory.", key.login)));
        }
        key.keys().map_err(|e| invalid(e.to_string()))?;
        if self.keys.iter().any(|k| k.id == key.id) {
            return Ok(());
        }
        if let Some(other) = self
            .keys
            .iter()
            .find(|k| k.login == key.login && k.device != device)
        {
            return Err(rule(format!(
                "{} already has chat on {}; a second computer comes later.",
                key.login, other.device
            )));
        }
        // A computer that made its key again replaces the one before.
        self.keys
            .retain(|k| !(k.login == key.login && k.device == device));
        self.keys.push(key);
        Ok(())
    }

    /// Whether `login` has a key registered on `device` and is in
    /// `conversation`: who may keep and fetch its blobs (ADR 0033).
    pub fn member_here(
        &self,
        device: &str,
        login: &str,
        conversation: &str,
    ) -> Result<(), Refused> {
        if !self
            .keys
            .iter()
            .any(|k| k.login == login && k.device == device)
        {
            return Err(rule("That person has no key on this computer."));
        }
        match self.conversations.get(conversation) {
            Some(stored) if stored.conversation.members.iter().any(|m| m == login) => Ok(()),
            _ => Err(rule("You aren't in it.")),
        }
    }

    pub fn lookup(&self, logins: &[String], compliance: &str) -> Keys {
        Keys {
            keys: self
                .keys
                .iter()
                .filter(|k| logins.contains(&k.login))
                .cloned()
                .collect(),
            compliance: compliance.into(),
        }
    }

    /// A new conversation from `device`, with its first epoch.
    pub fn create(
        &mut self,
        request: CreateConversation,
        directory: &dyn Directory,
    ) -> Result<u64, Refused> {
        let CreateConversation {
            device,
            conversation,
            epoch,
        } = request;
        let device = device.as_str();
        if !is_conversation_id(&conversation.id) {
            return Err(invalid("a conversation's id is cv- and 32 hex digits"));
        }
        if self.conversations.contains_key(&conversation.id) {
            return Err(rule("That conversation exists already."));
        }
        let members: BTreeSet<&str> = conversation.members.iter().map(String::as_str).collect();
        // An artifact or a doc may be its maker's alone until they share
        // it (ADR 0033, 0035); a conversation is two people at least.
        let fewest = if matches!(
            conversation.kind,
            ConversationKind::Artifact | ConversationKind::Doc
        ) {
            1
        } else {
            2
        };
        if members.len() != conversation.members.len()
            || members.len() < fewest
            || members.len() > MAX_MEMBERS
        {
            return Err(invalid(format!(
                "{fewest} to {MAX_MEMBERS} people, each once"
            )));
        }
        if conversation.kind == ConversationKind::Direct && members.len() != 2 {
            return Err(invalid("a direct message is between two people"));
        }
        for login in &members {
            if !directory.users().iter().any(|u| u.name == *login) {
                return Err(rule(format!("{login} isn't in the org's directory.")));
            }
        }
        if !members.contains(conversation.created_by.as_str()) {
            return Err(invalid("whoever makes a conversation is in it"));
        }
        if conversation.epoch != 1
            || epoch.body.number != 1
            || epoch.body.conversation != conversation.id
        {
            return Err(invalid("a conversation starts at epoch 1"));
        }
        let maker = self
            .key(&epoch.body.made_by)
            .filter(|k| k.login == conversation.created_by && k.device == device)
            .ok_or_else(|| rule("The epoch must be made on the creator's own computer."))?;
        epoch
            .verify(maker)
            .map_err(|_| rule("The epoch's signature doesn't check."))?;
        self.check_envelopes(&epoch, &members)?;
        self.seq += 1;
        let seq = self.seq;
        self.conversations.insert(
            conversation.id.clone(),
            Stored {
                conversation,
                epochs: vec![epoch],
                created_seq: seq,
                changed_seq: seq,
                joined: BTreeMap::new(),
            },
        );
        Ok(seq)
    }

    /// A conversation's next epoch from `device`, changing who is in it
    /// (ADR 0030 C2): the first one based on the current epoch wins, made
    /// on a member's own computer, checked as every computer checks it, and
    /// sealed to exactly the members after it and IT. Its sequence number.
    pub fn rotate(
        &mut self,
        request: glass_managed::chat::RotateEpoch,
        directory: &dyn Directory,
    ) -> Result<u64, Refused> {
        let glass_managed::chat::RotateEpoch { device, epoch } = request;
        let stored = self
            .conversations
            .get(&epoch.body.conversation)
            .ok_or_else(|| Refused::NotFound("No such conversation.".into()))?;
        let maker = self
            .key(&epoch.body.made_by)
            .filter(|k| k.device == device)
            .ok_or_else(|| rule("The epoch must be made on a member's own computer."))?;
        epoch
            .verify(maker)
            .map_err(|_| rule("The epoch's signature doesn't check."))?;
        if epoch.body.based_on != Some(stored.conversation.epoch) {
            return Err(rule("Someone changed it first; fetch the newest epoch."));
        }
        let after = glass_managed::chat::check_change(
            stored.conversation.kind,
            &stored.conversation.members,
            stored.conversation.epoch,
            &epoch,
            &maker.login,
        )
        .map_err(|e| rule(e.to_string()))?;
        for login in &after {
            if !directory.users().iter().any(|u| u.name == *login) {
                return Err(rule(format!("{login} isn't in the org's directory.")));
            }
        }
        let members: BTreeSet<&str> = after.iter().map(String::as_str).collect();
        self.check_envelopes(&epoch, &members)?;
        let added: Vec<String> = after
            .iter()
            .filter(|l| !stored.conversation.members.contains(l))
            .cloned()
            .collect();
        self.seq += 1;
        let seq = self.seq;
        let stored = self
            .conversations
            .get_mut(&epoch.body.conversation)
            .ok_or_else(|| Refused::NotFound("No such conversation.".into()))?;
        stored.conversation.members = after;
        stored.conversation.epoch = epoch.body.number;
        stored.changed_seq = seq;
        for login in added {
            stored.joined.insert(login, seq);
        }
        stored.epochs.push(epoch);
        Ok(seq)
    }

    /// An epoch's envelopes must be exactly its members' keys and IT's: a
    /// computer left out couldn't read, and IT's key is how retention,
    /// holds and eDiscovery work (ADR 0030, decision 4).
    fn check_envelopes(&self, epoch: &Epoch, members: &BTreeSet<&str>) -> Result<(), Refused> {
        let want: BTreeSet<&str> = self
            .keys
            .iter()
            .filter(|k| members.contains(k.login.as_str()))
            .map(|k| k.id.as_str())
            .chain([COMPLIANCE])
            .collect();
        let have: Vec<&str> = epoch
            .body
            .envelopes
            .iter()
            .map(|e| e.key.as_str())
            .collect();
        let have_set: BTreeSet<&str> = have.iter().copied().collect();
        if !have_set.contains(COMPLIANCE) {
            return Err(rule("Every epoch is sealed to IT's compliance key too."));
        }
        if have_set.len() != have.len() || have_set != want {
            return Err(rule(
                "The epoch must be sealed to exactly its members' computers and IT.",
            ));
        }
        Ok(())
    }

    /// A message from `device`. Its sequence number.
    pub fn post(&mut self, device: &str, org: &str, mut message: Message) -> Result<u64, Refused> {
        let header = message.header().map_err(|e| invalid(e.to_string()))?;
        if header.org != org {
            return Err(invalid("a message for another org"));
        }
        let stored = self
            .conversations
            .get(&header.conversation)
            .ok_or_else(|| Refused::NotFound("No such conversation.".into()))?;
        let sender = self
            .key(&header.key)
            .filter(|k| k.login == header.sender && k.device == device)
            .ok_or_else(|| rule("The message's key isn't its sender's on this computer."))?;
        if !stored.conversation.members.contains(&header.sender) {
            return Err(rule(format!(
                "{} isn't in this conversation.",
                header.sender
            )));
        }
        if header.epoch != stored.conversation.epoch {
            return Err(rule(
                "The message is sealed under an old epoch; seal it again.",
            ));
        }
        message
            .verify(sender)
            .map_err(|_| rule("The message's signature doesn't check."))?;
        let messages = &self.messages;
        let posted = self.posted.0.get_or_insert_with(|| {
            messages
                .iter()
                .filter_map(|m| m.header().ok())
                .map(|h| (h.conversation, h.id))
                .collect()
        });
        if !posted.insert((header.conversation, header.id)) {
            return Err(rule("That message was posted already."));
        }
        self.seq += 1;
        message.seq = self.seq;
        message.at = Some(Timestamp::now().to_string());
        self.messages.push(message);
        Ok(self.seq)
    }

    /// What changed for the people on `request.device` since its cursor.
    pub fn sync(&self, request: &ChatSyncRequest) -> ChatSyncResponse {
        // Only logins with a key on this computer: a computer hears of its
        // own people's conversations, nobody else's.
        let mine: Vec<&MemberKey> = self
            .keys
            .iter()
            .filter(|k| k.device == request.device && request.logins.contains(&k.login))
            .collect();
        let logins: BTreeSet<&str> = mine.iter().map(|k| k.login.as_str()).collect();
        let theirs: Vec<&Stored> = self
            .conversations
            .values()
            .filter(|s| {
                s.conversation
                    .members
                    .iter()
                    .any(|m| logins.contains(m.as_str()))
            })
            .collect();
        let ids: BTreeSet<&str> = theirs.iter().map(|s| s.conversation.id.as_str()).collect();
        // Whole: the maker's signature covers every envelope.
        let epochs = theirs.iter().flat_map(|s| s.epochs.clone()).collect();
        // Someone added since the cursor gets the conversation's history.
        let joined: BTreeSet<&str> = theirs
            .iter()
            .filter(|s| {
                s.joined
                    .iter()
                    .any(|(l, at)| logins.contains(l.as_str()) && *at > request.since)
            })
            .map(|s| s.conversation.id.as_str())
            .collect();
        // In sequence order: what's new starts past the cursor, unless
        // someone here was added since, whose conversation's history is
        // theirs too.
        let from = if joined.is_empty() {
            self.messages.partition_point(|m| m.seq <= request.since)
        } else {
            0
        };
        let messages: Vec<Message> = self.messages[from..]
            .iter()
            .filter(|m| {
                m.header().is_ok_and(|h| {
                    ids.contains(h.conversation.as_str())
                        && (m.seq > request.since || joined.contains(h.conversation.as_str()))
                })
            })
            .cloned()
            .collect();
        let people: BTreeSet<&str> = theirs
            .iter()
            .flat_map(|s| s.conversation.members.iter().map(String::as_str))
            .collect();
        ChatSyncResponse {
            conversations: theirs.iter().map(|s| s.conversation.clone()).collect(),
            epochs,
            messages,
            keys: self
                .keys
                .iter()
                .filter(|k| people.contains(k.login.as_str()))
                .cloned()
                .collect(),
            cursor: self.seq,
        }
    }

    /// Whether anything for `request` is newer than its cursor.
    pub fn news(&self, request: &ChatSyncRequest) -> bool {
        let answer = self.sync(request);
        !answer.messages.is_empty()
            || self.conversations.values().any(|s| {
                s.created_seq.max(s.changed_seq) > request.since
                    && answer
                        .conversations
                        .iter()
                        .any(|c| c.id == s.conversation.id)
            })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use glass_crypto::{EpochKey, SecretKey};
    use glass_managed::chat::{
        Body, ChatSyncResponse, Conversation, ConversationKind, Epoch, Header, Keys, MemberKey,
        MemberSecret, Message, new_conversation_id, new_message_id, open_name, seal_name,
    };
    use serde_json::{Value, json};

    use glass_managed::meet::{
        Joined, MediaKey, MediaKeyRecord, MediaKeys, MeetBody, MeetOp, MeetRequest,
    };

    use super::{ChatState, append_log, open_log};
    use crate::directory::LocalDirectory;
    use crate::relay::Relay;
    use crate::server::Console;
    use crate::state::{Refused, State};

    struct Org {
        dir: tempfile::TempDir,
        console: Arc<Console>,
        token: String,
        it: SecretKey,
        /// The keys of the computers tests sign as.
        keys: Mutex<std::collections::HashMap<String, glass_managed::device::DeviceKey>>,
    }

    /// A computer with one person's chat, scripted.
    struct Computer {
        name: String,
        key: MemberKey,
        secret: MemberSecret,
    }

    impl Org {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let directory = LocalDirectory::starter("root");
            let state = State::open(dir.path(), "acme", "Acme Corp", &directory).unwrap();
            let token = state.org.enrollment_token.clone();
            let it = SecretKey::from_bytes(
                &hex::decode(
                    std::fs::read_to_string(dir.path().join("compliance-dev.key"))
                        .unwrap()
                        .trim(),
                )
                .unwrap(),
            )
            .unwrap();
            let console = Console::new(
                state,
                Box::new(directory),
                dir.path().join("web"),
                "http://127.0.0.1:7400".into(),
            );
            Self {
                dir,
                console,
                token,
                it,
                keys: Mutex::new(std::collections::HashMap::new()),
            }
        }

        /// A computer's request, signed by the computer it names.
        async fn post(&self, path: &str, body: &impl serde::Serialize) -> (u16, Value) {
            let request = crate::server::signed_for_tests(
                &self.console,
                &self.keys,
                &self.token,
                &format!("/device/v1/{path}"),
                serde_json::to_vec(body).unwrap(),
            )
            .await;
            let r = self.console.handle(&request).await;
            (
                r.status,
                serde_json::from_slice(&r.body).unwrap_or(Value::Null),
            )
        }

        async fn computer(&self, name: &str, login: &str) -> Computer {
            let secret = MemberSecret::generate().unwrap();
            let key = secret.public(login, name).unwrap();
            let (status, body) = self.post("chat/keys", &json!({ "key": key })).await;
            assert_eq!(status, 200, "{body}");
            Computer {
                name: name.into(),
                key,
                secret,
            }
        }

        async fn keys(&self, logins: &[&str]) -> Keys {
            let (_, body) = self
                .post("chat/keys/lookup", &json!({ "logins": logins }))
                .await;
            serde_json::from_value(body).unwrap()
        }

        /// `by` opens a channel with `members`: its id and epoch key.
        async fn channel(
            &self,
            by: &Computer,
            members: &[&str],
            name: &str,
        ) -> (u16, String, EpochKey) {
            let keys = self.keys(members).await;
            let compliance =
                glass_crypto::PublicKey::from_bytes(&hex::decode(&keys.compliance).unwrap())
                    .unwrap();
            let id = new_conversation_id().unwrap();
            let key = EpochKey::generate().unwrap();
            let epoch = Epoch::make(
                &id,
                1,
                &key,
                None,
                &keys.keys,
                &compliance,
                (&by.key, &by.secret),
            )
            .unwrap();
            let conversation = Conversation {
                id: id.clone(),
                kind: ConversationKind::Channel,
                members: members.iter().map(|m| (*m).to_owned()).collect(),
                created_by: by.key.login.clone(),
                name: Some(seal_name(&key, &id, 1, name).unwrap()),
                epoch: 1,
                label: None,
            };
            let (status, _) = self
                .post(
                    "chat/conversations",
                    &json!({ "device": by.name, "conversation": conversation, "epoch": epoch }),
                )
                .await;
            (status, id, key)
        }

        /// `by` makes a meeting with `members` (Calendar's approved invite
        /// does this): its id.
        async fn meeting(&self, by: &Computer, members: &[&str]) -> String {
            let keys = self.keys(members).await;
            let compliance =
                glass_crypto::PublicKey::from_bytes(&hex::decode(&keys.compliance).unwrap())
                    .unwrap();
            let id = new_conversation_id().unwrap();
            let key = EpochKey::generate().unwrap();
            let epoch = Epoch::make(
                &id,
                1,
                &key,
                None,
                &keys.keys,
                &compliance,
                (&by.key, &by.secret),
            )
            .unwrap();
            let conversation = Conversation {
                id: id.clone(),
                kind: ConversationKind::Meeting,
                members: members.iter().map(|m| (*m).to_owned()).collect(),
                created_by: by.key.login.clone(),
                name: Some(seal_name(&key, &id, 1, "Northwind renewal").unwrap()),
                epoch: 1,
                label: None,
            };
            let (status, body) = self
                .post(
                    "chat/conversations",
                    &json!({ "device": by.name, "conversation": conversation, "epoch": epoch }),
                )
                .await;
            assert_eq!(status, 200, "{body}");
            id
        }

        /// `who` asks meeting `meeting` to `op`, signed by their member key.
        async fn meet(&self, who: &Computer, meeting: &str, op: MeetOp) -> (u16, Value) {
            let path = match &op {
                MeetOp::Join { .. } => "join",
                MeetOp::Keys => "keys",
                MeetOp::Rekey { .. } => "rekey",
                MeetOp::Leave => "leave",
            };
            let body = MeetBody {
                meeting: meeting.into(),
                op,
                device: who.name.clone(),
                login: who.key.login.clone(),
                key: who.key.id.clone(),
                at: "2026-10-19T14:02:11Z".into(),
            };
            let request = MeetRequest::sign(body, &who.secret).unwrap();
            self.post(&format!("meet/{meeting}/{path}"), &request).await
        }

        async fn say(
            &self,
            from: &Computer,
            conversation: &str,
            epoch: u32,
            key: &EpochKey,
            text: &str,
        ) -> (u16, Value) {
            let header = Header {
                v: 1,
                org: "acme".into(),
                conversation: conversation.into(),
                epoch,
                id: new_message_id(1_760_000_000_000).unwrap(),
                sender: from.key.login.clone(),
                key: from.key.id.clone(),
            };
            let body = Body {
                text: text.into(),
                attribution: None,
                reply_to: None,
                sent_at: "2026-10-19T14:02:11Z".into(),
                part: None,
                mentions: Vec::new(),
            };
            let message = Message::seal(&header, &body, key, &from.secret).unwrap();
            self.post(
                "chat/messages",
                &json!({ "device": from.name, "message": message }),
            )
            .await
        }

        async fn sync(&self, on: &Computer, since: u64, wait: u64) -> ChatSyncResponse {
            let (status, body) = self
                .post(
                    "chat/sync",
                    &json!({ "device": on.name, "logins": [on.key.login], "since": since, "wait_secs": wait }),
                )
                .await;
            assert_eq!(status, 200, "{body}");
            serde_json::from_value(body).unwrap()
        }
    }

    fn base64(bytes: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..=chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    }

    /// Every way the console could hold `secret`: as it is, hex, and
    /// base64 at each alignment (edges trimmed).
    fn forms(secret: &[u8]) -> Vec<Vec<u8>> {
        let mut out = vec![
            secret.to_vec(),
            hex::encode(secret).into_bytes(),
            hex::encode_upper(secret).into_bytes(),
        ];
        for pad in 0..3 {
            let mut shifted = vec![0u8; pad];
            shifted.extend_from_slice(secret);
            let encoded = base64(&shifted);
            out.push(
                encoded.as_bytes()[(pad * 4).div_ceil(3) + 1..encoded.len().saturating_sub(3)]
                    .to_vec(),
            );
        }
        out
    }

    fn found_in(haystack: &[u8], secret: &[u8]) -> bool {
        forms(secret)
            .iter()
            .any(|f| !f.is_empty() && haystack.windows(f.len()).any(|w| w == f.as_slice()))
    }

    fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(files(&path));
            } else {
                out.push((path.display().to_string(), std::fs::read(&path).unwrap()));
            }
        }
        out
    }

    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<u8>>>);

    impl Write for Log {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn gt3_the_console_never_holds_chat_plaintext() {
        let log = Log::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let org = Org::new().await;
        let priya = org.computer("laptop-priya", "priya").await;
        let dana = org.computer("laptop-dana", "dana").await;
        let (status, cv, key) = org
            .channel(&priya, &["priya", "dana"], "#CANARY-NAME-northwind-7f3a")
            .await;
        assert_eq!(status, 200);
        let (status, posted) = org
            .say(&priya, &cv, 1, &key, "CANARY-TEXT renewal at 4.2% 9b1c")
            .await;
        assert_eq!(status, 200, "{posted}");

        // Dana's computer gets it, opens it, and IT's key opens it too.
        let got = org.sync(&dana, 0, 0).await;
        let epoch = &got.epochs[0];
        epoch
            .verify(got.keys.iter().find(|k| k.login == "priya").unwrap())
            .expect("it travels whole, as signed");
        let dana_key = epoch.open(&dana.key.id, &dana.secret.agree).unwrap();
        let message = &got.messages[0];
        message
            .verify(got.keys.iter().find(|k| k.login == "priya").unwrap())
            .unwrap();
        assert_eq!(
            message.open(&dana_key).unwrap().text,
            "CANARY-TEXT renewal at 4.2% 9b1c"
        );
        assert_eq!(
            open_name(
                &dana_key,
                &cv,
                1,
                got.conversations[0].name.as_ref().unwrap()
            )
            .unwrap(),
            "#CANARY-NAME-northwind-7f3a"
        );
        let stored = org.console.state().chat.conversations[&cv].epochs[0].clone();
        assert_eq!(
            stored
                .open(glass_managed::chat::COMPLIANCE, &org.it)
                .unwrap(),
            key
        );

        // Nothing the console wrote, holds or logged has the text, the name
        // or the key that opens them.
        let held = serde_json::to_vec(&org.console.state().chat).unwrap();
        let logged = log.0.lock().unwrap().clone();
        let mut places = files(org.dir.path());
        places.push(("the chat state in memory".into(), held));
        places.push(("the console's log".into(), logged));
        for (place, bytes) in &places {
            for secret in [
                b"CANARY-TEXT".as_slice(),
                b"renewal at 4.2%",
                b"CANARY-NAME",
                b"northwind-7f3a",
                key.as_bytes().as_slice(),
            ] {
                assert!(
                    !found_in(bytes, secret),
                    "{place} holds {}",
                    String::from_utf8_lossy(secret)
                );
            }
        }
        assert!(
            places.len() >= 5,
            "the state folder was searched: {}",
            places.len()
        );
        assert!(
            places.iter().any(|(p, _)| p.ends_with("chat.json")),
            "chat.json was searched"
        );
        assert!(
            places
                .iter()
                .any(|(p, _)| p.ends_with("chat-messages.jsonl")),
            "the messages' log was searched"
        );

        // The search finds what it looks for: a planted copy, in each form.
        let secret = b"CANARY-TEXT renewal";
        for pad in 0..3 {
            let mut planted = b"..".repeat(pad);
            planted.extend_from_slice(secret);
            planted.extend_from_slice(b"..");
            assert!(found_in(&planted, b"CANARY-TEXT"));
            assert!(found_in(hex::encode(&planted).as_bytes(), b"CANARY-TEXT"));
            assert!(
                found_in(base64(&planted).as_bytes(), b"CANARY-TEXT"),
                "base64 at offset {pad}"
            );
        }
        assert!(found_in(base64(key.as_bytes()).as_bytes(), key.as_bytes()));
    }

    #[tokio::test]
    async fn ar2_a_blob_goes_only_to_its_peoples_computers_and_is_never_replaced() {
        use glass_managed::shared::{BlobKey, new_blob_id};
        let org = Org::new().await;
        let priya = org.computer("laptop-priya", "priya").await;
        let dana = org.computer("laptop-dana", "dana").await;
        let sam = org.computer("laptop-sam", "sam").await;
        let (status, cv, _) = org.channel(&priya, &["priya", "dana"], "Q3 forecast").await;
        assert_eq!(status, 200);
        let key = BlobKey::generate().unwrap();
        let id = new_blob_id().unwrap();
        let page = b"<h1>CANARY-BLOB Northwind 42k</h1>";
        let blob = key.seal(&cv, &id, page).unwrap();
        let put = |who: &Computer, blob| json!({ "device": who.name, "login": who.key.login, "blob": blob });
        let (status, body) = org.post("blobs", &put(&priya, &blob)).await;
        assert_eq!(status, 200, "{body}");

        // Dana's computer fetches it and opens it; Sam's can't, and nobody
        // can fetch it as someone else from their own computer.
        let get = |who: &Computer, login: &str| json!({ "device": who.name, "login": login, "conversation": cv, "id": id });
        let (status, got) = org.post("blobs/get", &get(&dana, "dana")).await;
        assert_eq!(status, 200, "{got}");
        let got: glass_managed::shared::SealedBlob = serde_json::from_value(got).unwrap();
        let sha = hex::encode(glass_crypto::sha256(page));
        assert_eq!(key.open(&got, &sha).unwrap(), page);
        for (who, login) in [(&sam, "sam"), (&sam, "dana")] {
            let (status, _) = org.post("blobs/get", &get(who, login)).await;
            assert_ne!(status, 200, "{login} from {}", who.name);
        }
        // Nor fetched under another conversation.
        let (_, other, _) = org.channel(&sam, &["sam", "dana"], "Other").await;
        let (status, _) = org
            .post(
                "blobs/get",
                &json!({ "device": dana.name, "login": "dana", "conversation": other, "id": id }),
            )
            .await;
        assert_ne!(status, 200);

        // A member can't replace it, and an outsider can't keep one there.
        let forged = BlobKey::generate()
            .unwrap()
            .seal(&cv, &id, b"<h1>mine</h1>")
            .unwrap();
        let (status, _) = org.post("blobs", &put(&dana, &forged)).await;
        assert_eq!(status, 409);
        let fresh = BlobKey::generate()
            .unwrap()
            .seal(&cv, &new_blob_id().unwrap(), b"x")
            .unwrap();
        let (status, _) = org.post("blobs", &put(&sam, &fresh)).await;
        assert_ne!(status, 200);

        // On disk: ciphertext only.
        for entry in std::fs::read_dir(org.dir.path().join("blobs")).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains("CANARY-BLOB"));
            assert!(!text.contains(&hex::encode(b"CANARY-BLOB")));
        }
    }

    /// Epoch `number` of `cv` from `by`, changing its people to `after`
    /// by adding `add`, sealed to `sealed_to`'s keys and IT.
    #[allow(clippy::too_many_arguments)]
    async fn change(
        org: &Org,
        by: &Computer,
        cv: &str,
        number: u32,
        previous: &EpochKey,
        after: &[&str],
        add: &[&str],
        sealed_to: &[&str],
    ) -> (u16, Value, EpochKey) {
        let keys = org.keys(sealed_to).await;
        let compliance =
            glass_crypto::PublicKey::from_bytes(&hex::decode(&keys.compliance).unwrap()).unwrap();
        let key = EpochKey::generate().unwrap();
        let epoch = Epoch::change(
            cv,
            number,
            &key,
            previous,
            &keys.keys,
            &compliance,
            (&by.key, &by.secret),
            (
                after.iter().map(|m| (*m).to_owned()).collect(),
                glass_managed::chat::MemberOp::Add {
                    people: add.iter().map(|m| (*m).to_owned()).collect(),
                },
            ),
        )
        .unwrap();
        let (status, body) = org
            .post("chat/rotate", &json!({ "device": by.name, "epoch": epoch }))
            .await;
        (status, body, key)
    }

    #[tokio::test]
    async fn dc4_a_doc_may_be_its_makers_alone_but_a_channel_is_two_people() {
        let org = Org::new().await;
        let priya = org.computer("laptop-priya", "priya").await;
        let keys = org.keys(&["priya"]).await;
        let compliance =
            glass_crypto::PublicKey::from_bytes(&hex::decode(&keys.compliance).unwrap()).unwrap();
        for (kind, want) in [
            (ConversationKind::Doc, 200),
            (ConversationKind::Channel, 400),
        ] {
            let id = new_conversation_id().unwrap();
            let key = EpochKey::generate().unwrap();
            let epoch = Epoch::make(
                &id,
                1,
                &key,
                None,
                &keys.keys,
                &compliance,
                (&priya.key, &priya.secret),
            )
            .unwrap();
            let conversation = Conversation {
                id: id.clone(),
                kind,
                members: vec!["priya".into()],
                created_by: "priya".into(),
                name: Some(seal_name(&key, &id, 1, "Notes").unwrap()),
                epoch: 1,
                label: None,
            };
            let (status, body) = org
                .post(
                    "chat/conversations",
                    &json!({ "device": priya.name, "conversation": conversation, "epoch": epoch }),
                )
                .await;
            assert_eq!(status, want, "{kind:?}: {body}");
        }
    }

    #[tokio::test]
    async fn cf91_a_change_of_people_is_one_epoch_from_a_member_and_the_newcomer_gets_the_history()
    {
        let org = Org::new().await;
        let priya = org.computer("laptop-priya", "priya").await;
        let dana = org.computer("laptop-dana", "dana").await;
        let sam = org.computer("laptop-sam", "sam").await;
        let (_, cv, key) = org.channel(&priya, &["dana", "priya"], "#deal").await;
        let (status, _) = org.say(&priya, &cv, 1, &key, "the terms").await;
        assert_eq!(status, 200);
        let before = org.sync(&sam, 0, 0).await.cursor;
        let all = ["dana", "priya", "sam"];

        // Sam isn't in it, so he can't add himself.
        let (status, body, _) = change(&org, &sam, &cv, 2, &key, &all, &["sam"], &all).await;
        assert_eq!(status, 409, "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("someone in a conversation"),
            "{body}"
        );
        // Sealed to someone short, it's refused.
        let (status, body, _) = change(
            &org,
            &priya,
            &cv,
            2,
            &key,
            &all,
            &["sam"],
            &["dana", "priya"],
        )
        .await;
        assert_eq!(status, 409, "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("exactly its members"),
            "{body}"
        );
        // A member adds him; a second change on the same epoch loses.
        let (status, body, two) = change(&org, &priya, &cv, 2, &key, &all, &["sam"], &all).await;
        assert_eq!(status, 200, "{body}");
        let (status, body, _) = change(&org, &dana, &cv, 2, &key, &all, &["sam"], &all).await;
        assert_eq!(status, 409, "{body}");
        assert!(
            body["error"].as_str().unwrap().contains("changed it first"),
            "{body}"
        );

        // Sam's computer hears of it and gets what was said before.
        let got = org.sync(&sam, before, 0).await;
        assert_eq!(got.conversations[0].members, all);
        assert_eq!(got.epochs.len(), 2);
        assert_eq!(
            got.messages.len(),
            1,
            "the history, though older than his cursor"
        );
        let newest = got.epochs[1].open(&sam.key.id, &sam.secret.agree).unwrap();
        assert_eq!(newest.as_bytes(), two.as_bytes());
        // Dana's, already caught up, isn't sent it again.
        let cursor = org.sync(&dana, 0, 0).await.cursor;
        assert!(org.sync(&dana, cursor, 0).await.messages.is_empty());
        // The old epoch is closed for posting.
        let (status, body) = org.say(&dana, &cv, 1, &key, "late").await;
        assert_eq!(status, 409, "{body}");
        let (status, _) = org.say(&sam, &cv, 2, &two, "thanks for adding me").await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn cf91_posts_from_outsiders_other_keys_and_other_epochs_are_refused() {
        let org = Org::new().await;
        let priya = org.computer("laptop-priya", "priya").await;
        let dana = org.computer("laptop-dana", "dana").await;
        let sam = org.computer("laptop-sam", "sam").await;
        let (_, cv, key) = org.channel(&priya, &["priya", "dana"], "#deal").await;

        // Sam isn't in it, even holding the key somehow.
        let (status, body) = org.say(&sam, &cv, 1, &key, "hello").await;
        assert_eq!(status, 409, "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("isn't in this conversation")
        );

        // Dana's computer can't post as Priya: the key isn't Priya's there.
        let as_priya = Computer {
            name: dana.name.clone(),
            key: priya.key.clone(),
            secret: MemberSecret::generate().unwrap(),
        };
        let (status, body) = org.say(&as_priya, &cv, 1, &key, "hi").await;
        assert_eq!(status, 409, "{body}");
        // Priya's own computer, with a signature from another key.
        let forged = Computer {
            name: priya.name.clone(),
            key: priya.key.clone(),
            secret: MemberSecret::generate().unwrap(),
        };
        let (status, body) = org.say(&forged, &cv, 1, &key, "hi").await;
        assert_eq!(status, 409, "{body}");
        assert!(body["error"].as_str().unwrap().contains("signature"));

        // Another epoch than the conversation's.
        let (status, body) = org.say(&priya, &cv, 2, &key, "hi").await;
        assert_eq!(status, 409, "{body}");
        assert!(body["error"].as_str().unwrap().contains("epoch"));
        let (status, _) = org.say(&priya, &cv, 1, &key, "hi").await;
        assert_eq!(status, 200);
        // A second computer for Priya waits for slice C2.
        let second = MemberSecret::generate().unwrap();
        let (status, _) = org
            .post(
                "chat/keys",
                &json!({ "key": second.public("priya", "desktop-priya").unwrap() }),
            )
            .await;
        assert_eq!(status, 409);
    }

    #[tokio::test]
    async fn cf91_an_epoch_not_sealed_to_it_and_every_member_is_refused() {
        let org = Org::new().await;
        let priya = org.computer("laptop-priya", "priya").await;
        let _dana = org.computer("laptop-dana", "dana").await;
        let keys = org.keys(&["priya", "dana"]).await;
        let compliance =
            glass_crypto::PublicKey::from_bytes(&hex::decode(&keys.compliance).unwrap()).unwrap();
        let id = new_conversation_id().unwrap();
        let key = EpochKey::generate().unwrap();
        let conversation = Conversation {
            id: id.clone(),
            kind: ConversationKind::Direct,
            members: vec!["priya".into(), "dana".into()],
            created_by: "priya".into(),
            name: None,
            epoch: 1,
            label: None,
        };
        let create = |epoch: Epoch| json!({ "device": "laptop-priya", "conversation": conversation, "epoch": epoch });

        // Without IT's envelope: re-signed, so only the envelopes are wrong.
        let mut epoch = Epoch::make(
            &id,
            1,
            &key,
            None,
            &keys.keys,
            &compliance,
            (&priya.key, &priya.secret),
        )
        .unwrap();
        epoch
            .body
            .envelopes
            .retain(|e| e.key != glass_managed::chat::COMPLIANCE);
        epoch.signature = hex::encode(
            priya
                .secret
                .sign
                .sign(&serde_json::to_vec(&epoch.body).unwrap())
                .unwrap(),
        );
        let (status, body) = org.post("chat/conversations", &create(epoch)).await;
        assert_eq!(status, 409, "{body}");
        assert!(body["error"].as_str().unwrap().contains("compliance"));

        // Without Dana's computer.
        let only_priya: Vec<MemberKey> = keys
            .keys
            .iter()
            .filter(|k| k.login == "priya")
            .cloned()
            .collect();
        let epoch = Epoch::make(
            &id,
            1,
            &key,
            None,
            &only_priya,
            &compliance,
            (&priya.key, &priya.secret),
        )
        .unwrap();
        let (status, _) = org.post("chat/conversations", &create(epoch)).await;
        assert_eq!(status, 409);

        let epoch = Epoch::make(
            &id,
            1,
            &key,
            None,
            &keys.keys,
            &compliance,
            (&priya.key, &priya.secret),
        )
        .unwrap();
        let (status, body) = org.post("chat/conversations", &create(epoch)).await;
        assert_eq!(status, 200, "{body}");
    }

    #[tokio::test]
    async fn cf21_someone_who_left_cannot_decrypt_after_the_new_key() {
        let org = Org::new().await;
        let (relay, _calls) = fake_relay(25).await;
        org.console.set_relay(Relay::new(&relay).unwrap());
        let dana = org.computer("laptop-dana", "dana").await;
        let priya = org.computer("laptop-priya", "priya").await;
        // Sam hasn't used chat yet when the meeting starts.
        let meeting = org.meeting(&dana, &["dana", "priya", "sam"]).await;
        let make = |by: &Computer, to: &[&Computer]| {
            let keys: Vec<MemberKey> = to.iter().map(|c| c.key.clone()).collect();
            MediaKeyRecord::make(
                &meeting,
                &MediaKey::generate().unwrap(),
                &keys,
                (&by.key, &by.secret),
            )
            .unwrap()
        };
        let join = |media: Option<MediaKeyRecord>| MeetOp::Join {
            offer: "v=0".into(),
            media,
        };
        let keys = |body: Value| serde_json::from_value::<MediaKeys>(body).unwrap();
        let first = make(&dana, &[&dana, &priya]);
        for (who, media) in [(&dana, Some(first.clone())), (&priya, None)] {
            let (status, body) = org.meet(who, &meeting, join(media)).await;
            assert_eq!(status, 200, "{body}");
        }
        let (_, body) = org.meet(&dana, &meeting, MeetOp::Keys).await;
        assert!(!keys(body).due, "everyone present holds the key");

        // Sam's computer, registered after the key was made, can't open it:
        // it brings a new one, sealed to everyone present and itself.
        let sam = org.computer("laptop-sam", "sam").await;
        let (status, body) = org.meet(&sam, &meeting, join(None)).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (
                409,
                Some("The meeting's media key isn't sealed to this computer; bring a new one.")
            )
        );
        let (status, _) = org
            .meet(&sam, &meeting, join(Some(make(&sam, &[&dana, &sam]))))
            .await;
        assert_eq!(status, 409, "sealed to everyone present, priya included");
        let late = make(&sam, &[&dana, &priya, &sam]);
        let (status, body) = org.meet(&sam, &meeting, join(Some(late.clone()))).await;
        assert_eq!(status, 200, "{body}");
        let joined: Joined = serde_json::from_value(body).unwrap();
        assert_eq!(joined.media, [late.clone(), first.clone()]);
        let (_, body) = org.meet(&dana, &meeting, MeetOp::Keys).await;
        let seen = keys(body);
        assert_eq!((&seen.media[0], seen.due), (&late, false));

        // Sam leaves: a new key is due, for dana and priya only.
        let (status, _) = org.meet(&sam, &meeting, MeetOp::Leave).await;
        assert_eq!(status, 200);
        let (_, body) = org.meet(&priya, &meeting, MeetOp::Keys).await;
        let now = keys(body);
        assert!(now.due);
        let mut present: Vec<&str> = now.participants.iter().map(|k| k.login.as_str()).collect();
        present.sort_unstable();
        assert_eq!(present, ["dana", "priya"]);

        // Sealed to sam too, or brought by sam: refused.
        let rekey = |media| MeetOp::Rekey { media };
        let (status, _) = org
            .meet(
                &priya,
                &meeting,
                rekey(make(&priya, &[&dana, &priya, &sam])),
            )
            .await;
        assert_eq!(status, 409);
        let (status, _) = org
            .meet(&sam, &meeting, rekey(make(&sam, &[&dana, &priya])))
            .await;
        assert_eq!(status, 409);
        // Priya's computer makes it; the first wins.
        let next = make(&priya, &[&dana, &priya]);
        let (status, body) = org.meet(&priya, &meeting, rekey(next.clone())).await;
        assert_eq!(status, 200, "{body}");
        let after = keys(body);
        assert!(!after.due);
        assert_eq!(
            after.media,
            [next.clone(), late],
            "the one before kept for frames in flight"
        );
        assert!(
            next.open(&sam.key.id, &sam.secret.agree).is_err(),
            "sam can't open it"
        );
        assert!(next.open(&dana.key.id, &dana.secret.agree).is_ok());
        let (status, body) = org
            .meet(&dana, &meeting, rekey(make(&dana, &[&dana, &priya])))
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (
                409,
                Some("The meeting's media key is new already; ask for keys again.")
            )
        );
    }

    #[tokio::test]
    async fn cf91_a_post_wakes_waiting_computers_within_a_second() {
        let org = Arc::new(Org::new().await);
        let priya = org.computer("laptop-priya", "priya").await;
        let dana = org.computer("laptop-dana", "dana").await;
        let (_, cv, key) = org.channel(&priya, &["priya", "dana"], "#deal").await;
        let since = org.sync(&dana, 0, 0).await.cursor;
        let waiting = {
            let org = Arc::clone(&org);
            tokio::spawn(async move {
                let started = std::time::Instant::now();
                let answer = org.sync(&dana, since, 20).await;
                (answer, started.elapsed())
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (status, _) = org.say(&priya, &cv, 1, &key, "are you there?").await;
        assert_eq!(status, 200);
        let (answer, took) = waiting.await.unwrap();
        assert_eq!(answer.messages.len(), 1);
        assert!(took < Duration::from_secs(1), "{took:?}");
    }

    /// glassd's chat on two computers, through the console's device API
    /// over HTTP with the client glassd uses: what one posts, the other
    /// checks and opens; the console's refusals arrive in words.
    #[tokio::test(flavor = "multi_thread")]
    async fn cf91_two_computers_chat_through_the_consoles_device_api() {
        use glass_actions::ActionProvider;
        use glass_app_chat::{ChatHandle, ChatProvider};

        let org = Org::new().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(Arc::clone(&org.console).serve(listener));
        let token = org.token.clone();
        let compliance = org.console.state().chat.compliance.clone();
        let dir = org.dir.path().join("computers");
        tokio::task::spawn_blocking(move || {
            let computer = |name: &str| {
                let handle = ChatHandle::new(dir.join(name));
                let key = glass_managed::device::DeviceKey::generate(name).unwrap();
                let client = glass_gateway_providers::ConsoleClient::new(&url, &token, true)
                    .unwrap()
                    .signed_by(Arc::new(key));
                client.register_device().unwrap();
                handle.connect("acme", name, Arc::new(client));
                let key = glass_crypto::PublicKey::from_bytes(&hex::decode(&compliance).unwrap())
                    .unwrap();
                let release = glass_managed::ChatRelease::new(&key);
                handle
                    .set_compliance(&release.compliance, &release.fingerprint)
                    .unwrap();
                let provider =
                    ChatProvider::new(handle.clone(), glass_managed::DirectoryHandle::new());
                (provider, handle)
            };
            let call = |on: &ChatProvider, login: &str, action: &str, input: Value| {
                let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
                ctx.principal.on_behalf_of.name = login.into();
                ctx.principal.agent = "chat".parse().unwrap();
                on.invoke(&ctx, &action.parse().unwrap(), &input)
            };
            let (a, a_chat) = computer("dev-a");
            let (b, b_chat) = computer("dev-b");
            call(&b, "dana", "chat.list", json!({})).unwrap();
            let sent = call(
                &a,
                "priya",
                "chat.post",
                json!({ "posts": [{ "to": ["dana"], "text": "The Northwind terms are in" }] }),
            )
            .unwrap();
            assert_eq!(sent.text, "Posted 1 message in dana.");

            assert!(b_chat.sync_once(0).unwrap());
            let list = call(&b, "dana", "chat.list", json!({})).unwrap();
            assert_eq!(list.data["conversations"][0]["unread"], 1, "{}", list.text);
            let id = list.data["conversations"][0]["id"].clone();
            let read = call(&b, "dana", "chat.read", json!({ "conversation": id })).unwrap();
            assert_eq!(
                read.data["messages"][0]["text"], "The Northwind terms are in",
                "{}",
                read.text
            );
            call(
                &b,
                "dana",
                "chat.post",
                json!({ "posts": [{ "conversation": id, "text": "Reading now" }] }),
            )
            .unwrap();
            a_chat.sync_once(0).unwrap();
            let read = call(&a, "priya", "chat.read", json!({ "conversation": id })).unwrap();
            assert_eq!(
                read.data["messages"][1]["text"], "Reading now",
                "{}",
                read.text
            );

            // Dana on a second computer: the console's refusal, in words.
            let (c, _) = computer("dev-c");
            let refused = call(&c, "dana", "chat.list", json!({}))
                .unwrap_err()
                .to_string();
            assert!(refused.contains("already has chat on dev-b"), "{refused}");
        })
        .await
        .unwrap();
    }

    /// A stand-in for glass-sfu's private API: two seats a room, answers
    /// naming the participant, and every call written down.
    /// A relay on loopback that seats `room_size` people a room.
    async fn fake_relay(room_size: usize) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        tokio::spawn(async move {
            let mut seats: std::collections::BTreeMap<String, std::collections::BTreeSet<u64>> =
                Default::default();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = glass_http::read_request(&mut stream).await.unwrap();
                seen.lock()
                    .unwrap()
                    .push(format!("{} {}", request.method, request.path));
                let room = request
                    .path
                    .split('/')
                    .nth(2)
                    .unwrap_or_default()
                    .to_owned();
                let response = match request.method.as_str() {
                    // Who has a seat (ADR 0052): asked before keys.
                    "GET" => glass_http::Response::json(
                        200,
                        &json!({ "participants": seats.get(&room).cloned().unwrap_or_default() }),
                    ),
                    "POST" => {
                        let body: Value = serde_json::from_slice(&request.body).unwrap();
                        let n = body["participant"].as_u64().unwrap();
                        let taken = seats.entry(room).or_default();
                        if taken.len() >= room_size && !taken.contains(&n) {
                            glass_http::Response::json(
                                409,
                                &json!({ "error": format!("the room has its {room_size} participants already") }),
                            )
                        } else {
                            taken.insert(n);
                            glass_http::Response::json(
                                200,
                                &json!({ "answer": format!("v=0 answer for {n}") }),
                            )
                        }
                    }
                    _ => {
                        match request.path.split('/').nth(4) {
                            Some(n) => {
                                seats.entry(room).or_default().remove(&n.parse().unwrap());
                            }
                            None => {
                                seats.remove(&room);
                            }
                        }
                        glass_http::Response::json(204, &Value::Null)
                    }
                };
                glass_http::write_response(&mut stream, response, false)
                    .await
                    .unwrap();
            }
        });
        (addr, calls)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cf21_two_computers_meet_through_the_console_which_keeps_no_media_key() {
        let org = Org::new().await;
        let (relay, calls) = fake_relay(2).await;
        org.console.set_relay(Relay::new(&relay).unwrap());
        let dana = org.computer("laptop-dana", "dana").await;
        let priya = org.computer("laptop-priya", "priya").await;
        let mei = org.computer("laptop-mei", "mei").await;
        let meeting = org.meeting(&dana, &["dana", "priya"]).await;

        // No session yet: the first to join makes the media key, sealed to
        // the meeting's people and nobody else.
        let (status, keys) = org.meet(&dana, &meeting, MeetOp::Keys).await;
        assert_eq!(status, 200, "{keys}");
        assert!(
            serde_json::from_value::<MediaKeys>(keys)
                .unwrap()
                .media
                .is_empty()
        );
        let (status, body) = org
            .meet(
                &dana,
                &meeting,
                MeetOp::Join {
                    offer: "v=0 dana".into(),
                    media: None,
                },
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (409, Some("The meeting has no media key yet; send one."))
        );
        let media_key = MediaKey::generate().unwrap();
        let record = MediaKeyRecord::make(
            &meeting,
            &media_key,
            &[dana.key.clone(), priya.key.clone()],
            (&dana.key, &dana.secret),
        )
        .unwrap();
        let (status, body) = org
            .meet(
                &dana,
                &meeting,
                MeetOp::Join {
                    offer: "v=0 dana".into(),
                    media: Some(record.clone()),
                },
            )
            .await;
        assert_eq!(status, 200, "{body}");
        let joined: Joined = serde_json::from_value(body).unwrap();
        assert_eq!(
            (joined.participant, joined.answer.as_str()),
            (1, "v=0 answer for 1")
        );

        // The second gets the first's key, even when it brings its own.
        let theirs = MediaKeyRecord::make(
            &meeting,
            &MediaKey::generate().unwrap(),
            &[dana.key.clone(), priya.key.clone()],
            (&priya.key, &priya.secret),
        )
        .unwrap();
        let (status, body) = org
            .meet(
                &priya,
                &meeting,
                MeetOp::Join {
                    offer: "v=0 priya".into(),
                    media: Some(theirs),
                },
            )
            .await;
        assert_eq!(status, 200, "{body}");
        let joined: Joined = serde_json::from_value(body).unwrap();
        assert_eq!(joined.participant, 2);
        assert_eq!(joined.media, std::slice::from_ref(&record));
        assert_eq!(
            joined.media[0]
                .open(&priya.key.id, &priya.secret.agree)
                .unwrap(),
            media_key
        );
        assert!(
            joined.keys.iter().any(|k| k.id == dana.key.id),
            "to check the maker's signature"
        );

        // Someone not in the meeting gets nothing, not even the record.
        let (status, body) = org.meet(&mei, &meeting, MeetOp::Keys).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (409, Some("You aren't in this meeting."))
        );
        let (status, _) = org
            .meet(
                &mei,
                &meeting,
                MeetOp::Join {
                    offer: "v=0 mei".into(),
                    media: None,
                },
            )
            .await;
        assert_eq!(status, 409);
        // Nor does a request someone else signed for dana.
        let mut forged = MeetRequest::sign(
            MeetBody {
                meeting: meeting.clone(),
                op: MeetOp::Keys,
                device: dana.name.clone(),
                login: "dana".into(),
                key: dana.key.id.clone(),
                at: "2026-10-19T14:02:11Z".into(),
            },
            &mei.secret,
        )
        .unwrap();
        forged.body.at = "2026-10-19T14:02:12Z".into();
        let (status, body) = org.post(&format!("meet/{meeting}/keys"), &forged).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (409, Some("The request's signature doesn't check."))
        );

        // A relay that seats fewer than the console thinks refuses the
        // third, and the seat it didn't take is given back.
        let three = org.meeting(&dana, &["dana", "priya", "mei"]).await;
        let record3 = MediaKeyRecord::make(
            &three,
            &MediaKey::generate().unwrap(),
            &[dana.key.clone(), priya.key.clone(), mei.key.clone()],
            (&dana.key, &dana.secret),
        )
        .unwrap();
        for (who, media) in [(&dana, Some(record3)), (&priya, None)] {
            let (status, body) = org
                .meet(
                    who,
                    &three,
                    MeetOp::Join {
                        offer: "v=0".into(),
                        media,
                    },
                )
                .await;
            assert_eq!(status, 200, "{body}");
        }
        let (status, body) = org
            .meet(
                &mei,
                &three,
                MeetOp::Join {
                    offer: "v=0".into(),
                    media: None,
                },
            )
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (409, Some("This meeting is full: 25 people are in it."))
        );
        // ADR 0052: Priya's computer drops out without a word; the relay
        // sees its connection go. Asked for keys, the console asks the
        // relay first: she has left, holding the key, so a new one is due,
        // for Dana alone.
        Relay::new(&relay).unwrap().leave(&three, 2).await.unwrap();
        let (status, keys) = org.meet(&dana, &three, MeetOp::Keys).await;
        assert_eq!(status, 200, "{keys}");
        let keys: MediaKeys = serde_json::from_value(keys).unwrap();
        assert!(keys.due);
        assert_eq!(
            keys.participants
                .iter()
                .map(|k| &k.login)
                .collect::<Vec<_>>(),
            ["dana"]
        );

        // Leaving frees the seat; the last one out ends the session, and
        // its media key goes with it.
        for who in [&dana, &priya] {
            let (status, _) = org.meet(who, &meeting, MeetOp::Leave).await;
            assert_eq!(status, 200);
        }
        let (_, keys) = org.meet(&priya, &meeting, MeetOp::Keys).await;
        assert!(
            serde_json::from_value::<MediaKeys>(keys)
                .unwrap()
                .media
                .is_empty()
        );
        let calls = calls.lock().unwrap().clone();
        assert!(
            calls.contains(&format!("DELETE /rooms/{meeting}/participants/1")),
            "{calls:?}"
        );
        assert!(
            calls.contains(&format!("DELETE /rooms/{meeting}")),
            "{calls:?}"
        );

        // Nothing the console wrote holds the media key, in any form.
        org.console.state().save_chat().unwrap();
        for (path, bytes) in files(org.dir.path()) {
            assert!(
                !found_in(&bytes, media_key.as_bytes()),
                "the media key is in {path}"
            );
        }
    }

    /// An org's chat after a pilot's weeks: `n` messages in 40 channels.
    /// The people are priya and dana; returns them with the first channel.
    async fn long_history(org: &Org, n: usize) -> (Computer, Computer, String, EpochKey) {
        let priya = org.computer("laptop-priya", "priya").await;
        let dana = org.computer("laptop-dana", "dana").await;
        let mut channels = Vec::new();
        for i in 0..40 {
            let (status, id, key) = org
                .channel(&priya, &["priya", "dana"], &format!("#c{i}"))
                .await;
            assert_eq!(status, 200);
            channels.push((id, key));
        }
        {
            let mut state = org.console.state();
            for i in 0..n {
                let (conversation, key) = &channels[i % channels.len()];
                let header = Header {
                    v: 1,
                    org: "acme".into(),
                    conversation: conversation.clone(),
                    epoch: 1,
                    id: new_message_id(1_760_000_000_000 + i as u64).unwrap(),
                    sender: priya.key.login.clone(),
                    key: priya.key.id.clone(),
                };
                let body = Body {
                    text: format!("Message {i} about the quarter's numbers and who's on it"),
                    attribution: None,
                    reply_to: None,
                    sent_at: "2026-10-19T14:02:11Z".into(),
                    part: None,
                    mentions: Vec::new(),
                };
                let mut message = Message::seal(&header, &body, key, &priya.secret).unwrap();
                state.chat.seq += 1;
                message.seq = state.chat.seq;
                message.at = Some("2026-10-19T14:02:11Z".into());
                state.chat.messages.push(message);
            }
            state.save_chat().unwrap();
        }
        let (id, key) = channels.swap_remove(0);
        (priya, dana, id, key)
    }

    /// The messages' log (ADR 0052): a post adds a line, a restart reads
    /// them back in order with the sequence number past them, a message
    /// posted again after it is still refused, and syncs still bring
    /// what's new and, to someone added, the whole history.
    #[tokio::test]
    async fn cf91_messages_are_a_log_that_outlives_a_restart() {
        let org = Org::new().await;
        let (priya, dana, cv, key) = long_history(&org, 5).await;
        // Seeded straight into memory: kept as a post keeps them.
        {
            let state = org.console.state();
            let log = org.dir.path().join("chat-messages.jsonl");
            let _ = std::fs::remove_file(&log);
            for message in &state.chat.messages {
                append_log(&log, message).unwrap();
            }
        }
        let header = Header {
            v: 1,
            org: "acme".into(),
            conversation: cv.clone(),
            epoch: 1,
            id: new_message_id(1_760_000_100_000).unwrap(),
            sender: priya.key.login.clone(),
            key: priya.key.id.clone(),
        };
        let body = Body {
            text: "after the restart?".into(),
            attribution: None,
            reply_to: None,
            sent_at: "2026-10-19T14:02:11Z".into(),
            part: None,
            mentions: Vec::new(),
        };
        let message = Message::seal(&header, &body, &key, &priya.secret).unwrap();
        let (status, _) = org
            .post(
                "chat/messages",
                &json!({ "device": priya.name, "message": message }),
            )
            .await;
        assert_eq!(status, 200);
        let before = org.console.state().chat.clone();
        assert_eq!(before.messages.len(), 6);
        let chat_json = std::fs::read_to_string(org.dir.path().join("chat.json")).unwrap();
        assert!(
            !chat_json.contains("\"messages\""),
            "chat.json keeps no messages"
        );

        let directory = LocalDirectory::starter("root");
        let reopened = State::open(org.dir.path(), "acme", "Acme Corp", &directory).unwrap();
        assert_eq!(reopened.chat.messages, before.messages);
        assert_eq!(reopened.chat.seq, before.seq);
        let mut chat = reopened.chat;
        let again = chat.post(&priya.name, "acme", message);
        assert!(
            matches!(&again, Err(Refused::Rule(m)) if m.contains("posted already")),
            "{again:?}"
        );
        let request = glass_managed::chat::ChatSyncRequest {
            device: dana.name.clone(),
            logins: vec![dana.key.login.clone()],
            since: before.seq - 1,
            wait_secs: 0,
        };
        assert_eq!(chat.sync(&request).messages.len(), 1);
        let request = glass_managed::chat::ChatSyncRequest {
            since: 0,
            ..request
        };
        assert_eq!(chat.sync(&request).messages.len(), 6);
    }

    /// A console before ADR 0052 kept messages in chat.json: they move to
    /// the log, once. A line a crash cut short is dropped.
    #[test]
    fn cf91_messages_kept_in_chat_json_move_to_the_log_and_a_torn_line_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("chat-messages.jsonl");
        let message = |seq: u64| Message {
            header: "{}".into(),
            nonce: String::new(),
            ct: String::new(),
            signature: String::new(),
            seq,
            at: None,
        };
        let mut old = ChatState {
            messages: vec![message(1), message(2)],
            seq: 2,
            ..ChatState::default()
        };
        assert!(open_log(&mut old, &log).unwrap(), "moved");
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
        // A third posted, a fourth cut short.
        append_log(&log, &message(3)).unwrap();
        let mut torn = std::fs::read(&log).unwrap();
        torn.extend_from_slice(br#"{"header":"{}","no"#);
        std::fs::write(&log, torn).unwrap();
        let mut reopened = ChatState::default();
        assert!(!open_log(&mut reopened, &log).unwrap());
        let seqs: Vec<u64> = reopened.messages.iter().map(|m| m.seq).collect();
        assert_eq!(seqs, [1, 2, 3]);
        assert_eq!(reopened.seq, 3);
        assert!(std::fs::read_to_string(&log).unwrap().ends_with('\n'));
    }

    /// What a post and a computer's sync cost on a long history. Run by
    /// hand: `cargo test -p glass-console --release speed_ -- --ignored
    /// --nocapture`.
    #[tokio::test]
    #[ignore = "the speed check: run by hand"]
    #[allow(clippy::print_stdout)]
    async fn speed_chat_on_a_pilots_history() {
        for n in [2_000, 20_000] {
            let org = Org::new().await;
            let (priya, dana, cv, key) = long_history(&org, n).await;
            let since = org.console.state().chat.seq;
            let mut posts = Vec::new();
            let mut syncs = Vec::new();
            for i in 0..10 {
                let started = std::time::Instant::now();
                let (status, _) = org.say(&priya, &cv, 1, &key, &format!("new {i}")).await;
                assert_eq!(status, 200);
                posts.push(started.elapsed());
                let started = std::time::Instant::now();
                let answer = org.sync(&dana, since + i, 0).await;
                assert_eq!(answer.messages.len(), 1);
                syncs.push(started.elapsed());
            }
            posts.sort();
            syncs.sort();
            println!(
                "{n} messages: post p50 {:?} max {:?}; sync of what's new p50 {:?} max {:?}",
                posts[5], posts[9], syncs[5], syncs[9]
            );
        }
    }
}
