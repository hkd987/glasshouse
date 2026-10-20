//! The shared space (ADR 0033): what people make together, artifacts now
//! and docs and sheets next, kept as sealed chat conversations of their
//! own kinds. A message in one carries a [`Part`] inside its sealed body:
//! a version, a comment, a reaction or a resolve. Large content goes in a
//! sealed blob whose key travels only inside the sealed message that names
//! it; the console keeps the blob's ciphertext and reads none of it.

use glass_crypto::EpochKey;
use glass_types::Label;
use serde::{Deserialize, Serialize};

use crate::chat::{
    ChatError, ChatFailure, ConversationKind, LinkError, Result, is_conversation_id, open_name,
    seal_name,
};

/// The largest blob: an artifact's HTML with its images inline. Blobs
/// travel as hex, inside the console's 4 MiB request limit.
pub const MAX_BLOB_BYTES: usize = 3 * 512 * 1024;

/// The longest title an item has.
pub const MAX_TITLE_CHARS: usize = 200;

/// `bl-` and 32 hex digits.
pub fn is_blob_id(id: &str) -> bool {
    id.len() == 35
        && id.starts_with("bl-")
        && id[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn new_blob_id() -> Result<String> {
    Ok(format!("bl-{}", hex::encode(glass_crypto::random::<16>()?)))
}

/// Where a comment is pinned: an element of the page (its path from the
/// page's body), or a point in thousandths of the page's width and height.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<u16>,
}

/// What an item's sealed name holds: its title and its label (ADR 0034
/// §5). The label sealed here is the one computers trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemName {
    pub title: String,
    pub label: Label,
}

impl ItemName {
    /// Sealed under conversation `id`'s first epoch key.
    pub fn seal(&self, key: &EpochKey, id: &str) -> Result<String> {
        let text = serde_json::to_string(self).map_err(|e| ChatError::Invalid(e.to_string()))?;
        seal_name(key, id, 1, &text)
    }

    pub fn open(key: &EpochKey, id: &str, sealed: &str) -> Result<Self> {
        serde_json::from_str(&open_name(key, id, 1, sealed)?).map_err(|_| ChatError::Open)
    }
}

/// What a message in a shared item says beyond its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Part {
    /// A version of the item: the blob that holds it and the key that
    /// opens it. The message's text is the note that comes with it.
    Version {
        number: u32,
        blob: String,
        /// The blob's key, hex. Only ever inside a sealed body.
        key: String,
        /// SHA-256 of the content, hex.
        sha256: String,
        bytes: u64,
        /// `text/html` for an artifact, [`crate::skills::SKILL_MIME`] for a
        /// skill.
        mime: String,
        /// A skill's merged proposal (ADR 0034 §7).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merged: Option<String>,
        /// A skill version's scores, from the run its owner kept.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scores: Option<crate::skills::Scores>,
    },
    /// A proposed change to a skill (ADR 0034 §6): its bundle, as a
    /// version names one, and the base's and its scores from one run. The
    /// message's text says what changed and why.
    Proposal {
        blob: String,
        key: String,
        sha256: String,
        bytes: u64,
        /// The version it changes.
        base: u32,
        before: crate::skills::Scores,
        after: crate::skills::Scores,
    },
    /// A comment, on the version it was made on, pinned when `anchor` is
    /// given. The message's text is the comment.
    Comment {
        version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<Anchor>,
    },
    /// A reaction to message `to`.
    Reaction { to: String, emoji: String },
    /// Comment `comment` resolved, or reopened.
    Resolve { comment: String, resolved: bool },
    /// In a chat conversation: an item shared there, shown as a card.
    Shared {
        item: String,
        kind: String,
        title: String,
    },
    /// A doc's edit (ADR 0035 §2): a Yjs v1 update, base64.
    Update { data: String },
    /// An edit too large for a message, in a sealed blob.
    UpdateBlob {
        blob: String,
        key: String,
        sha256: String,
        bytes: u64,
    },
    /// The whole document, in a sealed blob, covering the log up to
    /// sequence number `upto`; a version in its history when named.
    Snapshot {
        blob: String,
        key: String,
        sha256: String,
        bytes: u64,
        upto: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Who edits and who comments, from the item's owner; everyone else in
    /// it views.
    Roles {
        editors: Vec<String>,
        commenters: Vec<String>,
    },
}

/// A blob's key: a fresh random key per blob, never reused.
#[derive(Clone)]
pub struct BlobKey(EpochKey);

impl std::fmt::Debug for BlobKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BlobKey(..)")
    }
}

fn blob_context(id: &str) -> Vec<u8> {
    format!("{id}|blob").into_bytes()
}

impl BlobKey {
    pub fn generate() -> Result<Self> {
        Ok(Self(EpochKey::generate()?))
    }

    /// The key a [`Part::Version`] carries.
    pub fn from_hex(text: &str) -> Result<Self> {
        let bytes = hex::decode(text).map_err(|_| ChatError::Open)?;
        Ok(Self(EpochKey::from_bytes(&bytes)?))
    }

    /// For a [`Part::Version`], inside a sealed body only.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.as_bytes())
    }

    /// Seals `bytes` as blob `id` of `conversation`.
    pub fn seal(&self, conversation: &str, id: &str, bytes: &[u8]) -> Result<SealedBlob> {
        if bytes.len() > MAX_BLOB_BYTES {
            return Err(ChatError::Invalid(format!(
                "it is {} KB; the most is {} KB",
                bytes.len().div_ceil(1024),
                MAX_BLOB_BYTES / 1024
            )));
        }
        let context = blob_context(id);
        let (nonce, ct) = self
            .0
            .message_key(&context)?
            .seal(conversation.as_bytes(), bytes)?;
        Ok(SealedBlob {
            id: id.to_owned(),
            conversation: conversation.to_owned(),
            nonce: hex::encode(nonce),
            ct: hex::encode(ct),
        })
    }

    /// Opens `blob`, checking it is the content `sha256` names.
    pub fn open(&self, blob: &SealedBlob, sha256: &str) -> Result<Vec<u8>> {
        let context = blob_context(&blob.id);
        let nonce = hex::decode(&blob.nonce).map_err(|_| ChatError::Open)?;
        let ct = hex::decode(&blob.ct).map_err(|_| ChatError::Open)?;
        let plain =
            self.0
                .message_key(&context)?
                .open(&nonce, blob.conversation.as_bytes(), &ct)?;
        if hex::encode(glass_crypto::sha256(&plain)) != sha256 {
            return Err(ChatError::Invalid(
                "a version's content isn't what its message says".into(),
            ));
        }
        Ok(plain)
    }
}

/// A sealed blob as it travels and as the console keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedBlob {
    pub id: String,
    /// The conversation it belongs to: only its members' computers get it.
    pub conversation: String,
    pub nonce: String,
    pub ct: String,
}

impl SealedBlob {
    /// Whether it is shaped as a blob may be.
    pub fn check(&self) -> std::result::Result<(), String> {
        if !is_blob_id(&self.id) || !is_conversation_id(&self.conversation) {
            return Err("a blob's id or conversation is malformed".into());
        }
        // Hex, the sealed bytes and their 16-byte tag.
        if self.ct.len() > 2 * (MAX_BLOB_BYTES + 16) || self.nonce.len() != 24 {
            return Err(format!("a blob is {} KB at most", MAX_BLOB_BYTES / 1024));
        }
        Ok(())
    }
}

/// `POST /device/v1/blobs`: a person's computer keeps a blob for a
/// conversation they are in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutBlob {
    pub device: String,
    pub login: String,
    pub blob: SealedBlob,
}

/// `POST /device/v1/blobs/get`: a blob, for a member's computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetBlob {
    pub device: String,
    pub login: String,
    pub conversation: String,
    pub id: String,
}

/// How a computer keeps and fetches blobs at its console. glassd's goes
/// through `ConsoleClient`; tests use `chat::testing::MemoryLink`.
pub trait BlobLink: Send + Sync + std::fmt::Debug {
    fn put(&self, request: &PutBlob) -> std::result::Result<(), LinkError>;
    fn get(&self, request: &GetBlob) -> std::result::Result<SealedBlob, LinkError>;
}

/// One message of a shared item, opened on a member's computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemMessage {
    pub id: String,
    pub seq: u64,
    pub sender: String,
    /// When the console took it, or when it was sent.
    pub at: String,
    pub text: String,
    /// `by <agent> for <person>` when an agent posted it (AU-10).
    pub attribution: Option<String>,
    pub reply_to: Option<String>,
    pub part: Option<Part>,
}

/// A shared item as a member's computer opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub kind: ConversationKind,
    pub title: String,
    /// As sealed with its name; chat's own conversations are internal.
    pub label: Label,
    pub members: Vec<String>,
    pub created_by: String,
    /// In order; what this computer couldn't open is left out.
    pub messages: Vec<ItemMessage>,
}

/// The shared space on a computer (ADR 0033): chat's store implements it,
/// so the apps that keep items (Artifacts, Docs, Sheets) reach it without
/// depending on Chat (ADR 0013). Every method blocks.
pub trait Shared: Send + Sync + std::fmt::Debug {
    /// Whether `login` can use the shared space here: the computer is
    /// managed, IT's Chat release is applied, and they have keys. With
    /// `make`, their keys are made and registered if they have none (the
    /// person's own app); without, an agent never makes them.
    fn ready(&self, login: &str, make: bool) -> std::result::Result<(), ChatFailure>;
    /// `login`'s items of `kind`, their messages opened.
    fn items(&self, login: &str, kind: ConversationKind) -> Vec<Item>;
    /// `login`'s item `id`, its messages opened; only that one is opened.
    fn item(&self, login: &str, id: &str) -> Option<Item>;
    /// Item `id` with only its messages after sequence number `seq`
    /// opened: what is new since a reader last looked.
    fn since(&self, login: &str, id: &str, seq: u64) -> Option<Item> {
        let mut item = self.item(login, id)?;
        item.messages.retain(|m| m.seq > seq);
        Some(item)
    }
    /// A new item of `kind` titled `title` and labeled `label`, for
    /// `login` and `members`, sealed to their computers and IT's
    /// compliance key. Its id.
    fn create(
        &self,
        login: &str,
        kind: ConversationKind,
        title: &str,
        label: Label,
        members: &[String],
    ) -> std::result::Result<String, ChatFailure>;
    /// Posts a message to item or conversation `to`, which `login` is in.
    /// Its id.
    fn post(
        &self,
        login: &str,
        to: &str,
        text: &str,
        part: Option<Part>,
        attribution: Option<String>,
        reply_to: Option<String>,
    ) -> std::result::Result<String, ChatFailure>;
    /// Keeps a sealed blob at the console for an item `login` is in.
    fn put_blob(&self, login: &str, blob: &SealedBlob) -> std::result::Result<(), ChatFailure>;
    /// Fetches blob `id` of `item`.
    fn get_blob(
        &self,
        login: &str,
        item: &str,
        id: &str,
    ) -> std::result::Result<SealedBlob, ChatFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ar1_a_blob_opens_only_with_its_key_and_as_its_message_says() {
        let conversation = crate::chat::new_conversation_id().unwrap();
        let id = new_blob_id().unwrap();
        let key = BlobKey::generate().unwrap();
        let page = b"<h1>Q3 forecast</h1>".to_vec();
        let sha = hex::encode(glass_crypto::sha256(&page));
        let blob = key.seal(&conversation, &id, &page).unwrap();
        blob.check().unwrap();
        assert!(!blob.ct.contains(&hex::encode(b"forecast")), "sealed");
        assert_eq!(key.open(&blob, &sha).unwrap(), page);
        let again = BlobKey::from_hex(&key.to_hex()).unwrap();
        assert_eq!(again.open(&blob, &sha).unwrap(), page);
        // Another key, another conversation's blob, or other content: no.
        assert!(BlobKey::generate().unwrap().open(&blob, &sha).is_err());
        let mut moved = blob.clone();
        moved.conversation = crate::chat::new_conversation_id().unwrap();
        assert!(key.open(&moved, &sha).is_err());
        assert!(key.open(&blob, &"0".repeat(64)).is_err());
        assert_eq!(format!("{key:?}"), "BlobKey(..)", "never printed");
        // Too large to keep.
        assert!(
            key.seal(&conversation, &id, &vec![0; MAX_BLOB_BYTES + 1])
                .is_err()
        );
    }

    #[test]
    fn ar3_parts_travel_tagged() {
        let comment = Part::Comment {
            version: 2,
            anchor: Some(Anchor {
                path: Some("main > table:nth-of-type(1)".into()),
                x: None,
                y: None,
            }),
        };
        let json = serde_json::to_string(&comment).unwrap();
        assert!(json.starts_with(r#"{"type":"comment""#), "{json}");
        assert_eq!(serde_json::from_str::<Part>(&json).unwrap(), comment);
    }
}
