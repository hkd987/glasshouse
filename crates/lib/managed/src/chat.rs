//! Chat's records on the wire (ADR 0030): member keys, conversations,
//! epochs with their sealed keys, and sealed messages. The console stores
//! and relays them without being able to read them; computers make and
//! open them. One implementation of the formats, here, for both sides.
//!
//! Every byte string is hex. Every signed thing is signed over SHA-256 of
//! exact bytes that travel with it (a message's header text, an epoch's
//! body as compact JSON in field order), so nobody re-encodes before
//! checking.

use glass_crypto::{CryptoError, EpochKey, PublicKey, Sealed, SecretKey};
use serde::{Deserialize, Serialize};

/// The longest message, in characters (ADR 0030).
pub const MAX_TEXT_CHARS: usize = 8000;
/// The most a sealed message may be.
pub const MAX_SEALED_BYTES: usize = 16 * 1024;
/// The most people in a conversation.
pub const MAX_MEMBERS: usize = 1000;
/// The envelope that IT's compliance key opens, by this key id.
pub const COMPLIANCE: &str = "compliance";

const ENVELOPE_INFO: &[u8] = b"glass-chat envelope v1";

/// Why a record was refused, in words for a log or a refusal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatError {
    #[error("{0}")]
    Invalid(String),
    #[error("a signature doesn't check")]
    Signature,
    #[error("couldn't open it")]
    Open,
}

impl From<CryptoError> for ChatError {
    fn from(e: CryptoError) -> Self {
        match e {
            CryptoError::Open => Self::Open,
            other => Self::Invalid(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, ChatError>;

fn unhex(s: &str, what: &str) -> Result<Vec<u8>> {
    hex::decode(s).map_err(|_| ChatError::Invalid(format!("{what} isn't hex")))
}

fn plain_id(s: &str, prefix: &str, len: usize) -> bool {
    s.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == len
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Whether `id` is a conversation id: `cv-` and 32 hex digits.
pub fn is_conversation_id(id: &str) -> bool {
    plain_id(id, "cv-", 32)
}

/// Whether `id` is a message id: 32 hex digits, time first.
pub fn is_message_id(id: &str) -> bool {
    plain_id(id, "", 32)
}

/// A new conversation id.
pub fn new_conversation_id() -> Result<String> {
    Ok(format!("cv-{}", hex::encode(glass_crypto::random::<16>()?)))
}

/// A new message id: milliseconds since 1970, then randomness, so ids sort
/// by when they were written.
pub fn new_message_id(unix_ms: u64) -> Result<String> {
    Ok(format!(
        "{:012x}{}",
        unix_ms & 0xffff_ffff_ffff,
        hex::encode(glass_crypto::random::<10>()?)
    ))
}

/// A person's keys on one computer: one to receive sealed keys, one to
/// sign with (ADR 0030, decision 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberKey {
    /// `mk-` and 16 hex digits of the public halves' SHA-256.
    pub id: String,
    pub login: String,
    /// The computer it lives on, as the fleet names it.
    pub device: String,
    /// The agreement key, uncompressed SEC1, hex.
    pub agree: String,
    /// The signing key, the same way.
    pub sign: String,
}

impl MemberKey {
    pub fn id_for(agree: &PublicKey, sign: &PublicKey) -> String {
        let mut both = agree.to_bytes().to_vec();
        both.extend_from_slice(&sign.to_bytes());
        format!("mk-{}", &hex::encode(glass_crypto::sha256(&both))[..16])
    }

    pub fn new(login: &str, device: &str, agree: &PublicKey, sign: &PublicKey) -> Self {
        Self {
            id: Self::id_for(agree, sign),
            login: login.into(),
            device: device.into(),
            agree: hex::encode(agree.to_bytes()),
            sign: hex::encode(sign.to_bytes()),
        }
    }

    /// The public halves, checked on the curve and against the id.
    pub fn keys(&self) -> Result<(PublicKey, PublicKey)> {
        let agree = PublicKey::from_bytes(&unhex(&self.agree, "a member key")?)?;
        let sign = PublicKey::from_bytes(&unhex(&self.sign, "a member key")?)?;
        if Self::id_for(&agree, &sign) != self.id {
            return Err(ChatError::Invalid(
                "a member key's id doesn't match it".into(),
            ));
        }
        Ok((agree, sign))
    }
}

/// A person's secret halves, kept by glassd, root only.
#[derive(Debug)]
pub struct MemberSecret {
    pub agree: SecretKey,
    pub sign: SecretKey,
}

impl MemberSecret {
    pub fn generate() -> Result<Self> {
        Ok(Self {
            agree: SecretKey::generate()?,
            sign: SecretKey::generate()?,
        })
    }

    pub fn public(&self, login: &str, device: &str) -> Result<MemberKey> {
        Ok(MemberKey::new(
            login,
            device,
            &self.agree.public()?,
            &self.sign.public()?,
        ))
    }

    /// Both scalars, for the key file.
    pub fn to_bytes(&self) -> Result<[u8; 64]> {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.agree.to_bytes()?);
        out[32..].copy_from_slice(&self.sign.to_bytes()?);
        Ok(out)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 64 {
            return Err(ChatError::Invalid("a member key file is 64 bytes".into()));
        }
        Ok(Self {
            agree: SecretKey::from_bytes(&bytes[..32])?,
            sign: SecretKey::from_bytes(&bytes[32..])?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    /// Two people.
    Direct,
    /// A few people, no name of its own.
    Group,
    /// Named, with a topic.
    Channel,
    /// A Glasshouse meeting (CF-21): its organizer and invitees, all in
    /// the directory. Its sealed name is the title and time
    /// ([`crate::meet::MeetingDetails`]). Calendar and Meet show it; Chat
    /// doesn't.
    Meeting,
    /// An artifact in the shared space (ADR 0033): its audience, its
    /// versions and the feedback on it. The Artifacts app shows it; Chat
    /// doesn't.
    Artifact,
    /// A skill in the shared space (ADR 0034): who may use it and propose
    /// changes, its versions with their evals, and the proposals.
    Skill,
    /// A doc (ADR 0035): one Yjs document, edited live by its editors.
    Doc,
}

impl ConversationKind {
    /// Whether the Chat app shows conversations of this kind.
    pub fn is_chat(self) -> bool {
        matches!(self, Self::Direct | Self::Group | Self::Channel)
    }

    /// Whether it is an item of the shared space, whose sealed name is an
    /// [`crate::shared::ItemName`].
    pub fn is_item(self) -> bool {
        matches!(self, Self::Artifact | Self::Skill | Self::Doc)
    }

    /// The app that shows it, which news in it wakes.
    pub fn app(self) -> &'static str {
        match self {
            Self::Direct | Self::Group | Self::Channel => "chat",
            Self::Meeting => "calendar",
            Self::Artifact => "artifacts",
            Self::Skill => "skills",
            Self::Doc => "docs",
        }
    }
}

/// An epoch's key sealed to one member key, or to IT's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// A member key's id, or [`COMPLIANCE`].
    pub key: String,
    pub enc: String,
    pub ct: String,
}

fn envelope_aad(conversation: &str, epoch: u32, key: &str) -> Vec<u8> {
    format!("{conversation}|{epoch}|{key}").into_bytes()
}

/// What an epoch says, signed by the member key that made it. The
/// signature covers the envelopes too, so whoever relays an epoch sends it
/// whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpochBody {
    pub conversation: String,
    pub number: u32,
    /// The epoch it follows; none for the first.
    #[serde(default)]
    pub based_on: Option<u32>,
    /// The previous epoch key, sealed under this one (hex).
    #[serde(default)]
    pub previous: Option<String>,
    pub envelopes: Vec<Envelope>,
    /// The member key that made it.
    pub made_by: String,
    /// Who is in the conversation from this epoch on, when it was made by
    /// a change of members (ADR 0030 C2). The epochs are the signed
    /// membership chain: each change is signed by a member who was in it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    /// The change this epoch makes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<MemberOp>,
}

/// A change to who is in a conversation, made by a new epoch, so people
/// added see the whole history through the epochs' backward chain, as in
/// Slack (ADR 0030 §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemberOp {
    /// People added by a member.
    Add { people: Vec<String> },
}

/// Checks epoch `next` as the change after epoch `number` of a
/// conversation of `kind` whose members were `before`, made by member
/// `maker`. The members after it, sorted. Every computer and the console
/// run the same check (ADR 0030 C2).
pub fn check_change(
    kind: ConversationKind,
    before: &[String],
    number: u32,
    next: &Epoch,
    maker: &str,
) -> Result<Vec<String>> {
    let bad = |m: &str| Err(ChatError::Invalid(m.into()));
    if !matches!(kind, ConversationKind::Channel | ConversationKind::Group) {
        return bad("only channels and groups change who is in them");
    }
    if next.body.number != number + 1
        || next.body.based_on != Some(number)
        || next.body.previous.is_none()
    {
        return bad("a change follows the conversation's current epoch, sealing it");
    }
    if !before.iter().any(|m| m == maker) {
        return bad("only someone in a conversation changes who is in it");
    }
    let Some(MemberOp::Add { people }) = &next.body.op else {
        return bad("an epoch after the first says what changed");
    };
    if people.is_empty() || people.iter().any(|p| before.contains(p)) {
        return bad("add people who aren't in it yet");
    }
    let mut after: Vec<String> = before.iter().chain(people).cloned().collect();
    after.sort();
    after.dedup();
    if after.len() != before.len() + people.len() || after.len() > MAX_MEMBERS {
        return bad("each person once, and not too many");
    }
    if next.body.members != after {
        return bad("the epoch's members aren't the change it says");
    }
    Ok(after)
}

/// `POST /device/v1/chat/rotate`: a conversation's next epoch, made by a
/// change of members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RotateEpoch {
    pub device: String,
    pub epoch: Epoch,
}

impl EpochBody {
    fn signed_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// One epoch of a conversation: its key, sealed to every member's
/// computer and to IT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Epoch {
    pub body: EpochBody,
    pub signature: String,
}

impl Epoch {
    /// Makes epoch `number` of `conversation`: seals `key` to each of
    /// `members` and to IT's `compliance` key, and signs it as `maker`.
    pub fn make(
        conversation: &str,
        number: u32,
        key: &EpochKey,
        previous: Option<&EpochKey>,
        members: &[MemberKey],
        compliance: &PublicKey,
        maker: (&MemberKey, &MemberSecret),
    ) -> Result<Self> {
        Self::sealed(
            conversation,
            number,
            key,
            previous,
            members,
            compliance,
            maker,
            (Vec::new(), None),
        )
    }

    /// Makes the epoch after `previous`'s that changes who is in
    /// `conversation`: `change` is who is in it after and what changed.
    #[allow(clippy::too_many_arguments)]
    pub fn change(
        conversation: &str,
        number: u32,
        key: &EpochKey,
        previous: &EpochKey,
        members: &[MemberKey],
        compliance: &PublicKey,
        maker: (&MemberKey, &MemberSecret),
        change: (Vec<String>, MemberOp),
    ) -> Result<Self> {
        Self::sealed(
            conversation,
            number,
            key,
            Some(previous),
            members,
            compliance,
            maker,
            (change.0, Some(change.1)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn sealed(
        conversation: &str,
        number: u32,
        key: &EpochKey,
        previous: Option<&EpochKey>,
        members: &[MemberKey],
        compliance: &PublicKey,
        maker: (&MemberKey, &MemberSecret),
        change: (Vec<String>, Option<MemberOp>),
    ) -> Result<Self> {
        let mut envelopes = Vec::with_capacity(members.len() + 1);
        for member in members {
            let (agree, _) = member.keys()?;
            let sealed = glass_crypto::seal(
                &agree,
                ENVELOPE_INFO,
                &envelope_aad(conversation, number, &member.id),
                key.as_bytes(),
            )?;
            envelopes.push(Envelope {
                key: member.id.clone(),
                enc: hex::encode(sealed.enc),
                ct: hex::encode(sealed.ct),
            });
        }
        let to_it = glass_crypto::seal(
            compliance,
            ENVELOPE_INFO,
            &envelope_aad(conversation, number, COMPLIANCE),
            key.as_bytes(),
        )?;
        envelopes.push(Envelope {
            key: COMPLIANCE.into(),
            enc: hex::encode(to_it.enc),
            ct: hex::encode(to_it.ct),
        });
        let previous = match previous {
            Some(p) => Some(hex::encode(
                key.seal_previous(p, &envelope_aad(conversation, number, "previous"))?,
            )),
            None => None,
        };
        let body = EpochBody {
            conversation: conversation.into(),
            number,
            based_on: number.checked_sub(1).filter(|n| *n > 0),
            previous,
            envelopes,
            made_by: maker.0.id.clone(),
            members: change.0,
            op: change.1,
        };
        let signature = hex::encode(maker.1.sign.sign(&body.signed_bytes())?);
        Ok(Self { body, signature })
    }

    /// Whether `maker` (the member key `made_by` names) signed it.
    pub fn verify(&self, maker: &MemberKey) -> Result<()> {
        if maker.id != self.body.made_by {
            return Err(ChatError::Signature);
        }
        let (_, sign) = maker.keys()?;
        if sign.verify(
            &self.body.signed_bytes(),
            &unhex(&self.signature, "a signature")?,
        ) {
            Ok(())
        } else {
            Err(ChatError::Signature)
        }
    }

    /// The key of the epoch before this one, opened with this one's `key`
    /// (the backward chain, ADR 0030 §1).
    pub fn open_previous(&self, key: &EpochKey) -> Result<EpochKey> {
        let sealed = unhex(
            self.body.previous.as_deref().ok_or(ChatError::Open)?,
            "a previous key",
        )?;
        Ok(key.open_previous(
            &sealed,
            &envelope_aad(&self.body.conversation, self.body.number, "previous"),
        )?)
    }

    /// Opens the envelope sealed to `key_id` with `secret` (a member's
    /// agreement key, or IT's compliance key).
    pub fn open(&self, key_id: &str, secret: &SecretKey) -> Result<EpochKey> {
        let envelope = self
            .body
            .envelopes
            .iter()
            .find(|e| e.key == key_id)
            .ok_or(ChatError::Open)?;
        let enc: [u8; glass_crypto::PUBLIC_KEY_LEN] = unhex(&envelope.enc, "an envelope")?
            .try_into()
            .map_err(|_| ChatError::Open)?;
        let sealed = Sealed {
            enc,
            ct: unhex(&envelope.ct, "an envelope")?,
        };
        let bytes = glass_crypto::open(
            secret,
            &sealed,
            ENVELOPE_INFO,
            &envelope_aad(&self.body.conversation, self.body.number, key_id),
        )?;
        Ok(EpochKey::from_bytes(&bytes)?)
    }
}

/// A conversation as the console keeps it: who is in it and its sealed
/// name. Ids and members are metadata (GT-3); the name isn't readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    pub id: String,
    pub kind: ConversationKind,
    /// Logins, in the directory.
    pub members: Vec<String>,
    pub created_by: String,
    /// The name, sealed under the first epoch's key (hex nonce ‖ ct), for a
    /// channel.
    #[serde(default)]
    pub name: Option<String>,
    /// The epoch messages are sealed under now.
    pub epoch: u32,
    /// An item's label in the clear, for IT's inventory (ADR 0034 §5).
    /// Computers trust the copy sealed with its name, and refuse an item
    /// whose two copies differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<glass_types::Label>,
}

fn name_context(conversation: &str, epoch: u32) -> Vec<u8> {
    format!("{conversation}|{epoch}|name").into_bytes()
}

/// Seals a channel's name under its epoch key.
pub fn seal_name(key: &EpochKey, conversation: &str, epoch: u32, name: &str) -> Result<String> {
    let context = name_context(conversation, epoch);
    let (nonce, ct) = key.message_key(&context)?.seal(&context, name.as_bytes())?;
    Ok(format!("{}{}", hex::encode(nonce), hex::encode(ct)))
}

pub fn open_name(key: &EpochKey, conversation: &str, epoch: u32, sealed: &str) -> Result<String> {
    let bytes = unhex(sealed, "a name")?;
    if bytes.len() < glass_crypto::NONCE_LEN {
        return Err(ChatError::Open);
    }
    let context = name_context(conversation, epoch);
    let (nonce, ct) = bytes.split_at(glass_crypto::NONCE_LEN);
    let plain = key.message_key(&context)?.open(nonce, &context, ct)?;
    String::from_utf8(plain).map_err(|_| ChatError::Open)
}

/// A message's clear part: metadata, and the sealed body's associated
/// data. Travels as the exact JSON text it was sealed and signed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub v: u8,
    pub org: String,
    pub conversation: String,
    pub epoch: u32,
    pub id: String,
    pub sender: String,
    /// The sender's member key.
    pub key: String,
}

/// What a message says, sealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Body {
    pub text: String,
    /// `by <agent> for <person>` when an agent posted it (AU-10).
    #[serde(default)]
    pub attribution: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    pub sent_at: String,
    /// What a message in a shared item carries (ADR 0033): a version, a
    /// comment, a reaction or a resolve; in chat, an item shared there, or
    /// an edit, delete, reaction, pin or topic (ADR 0030 C4a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<crate::shared::Part>,
    /// Members named `@login` in the text, worked out by the sender's
    /// computer (CF-92).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
}

/// A sealed message as it travels and as the console keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    /// The [`Header`] as JSON text, exactly as sealed and signed.
    pub header: String,
    pub nonce: String,
    pub ct: String,
    /// The sender's member key over SHA-256(header ‖ nonce ‖ ct).
    pub signature: String,
    /// The console's number for it, across the org; 0 before it has one.
    #[serde(default)]
    pub seq: u64,
    /// When the console took it (RFC 3339).
    #[serde(default)]
    pub at: Option<String>,
}

fn message_context(header: &Header) -> Vec<u8> {
    format!("{}|{}|{}", header.conversation, header.epoch, header.id).into_bytes()
}

fn signed_message_bytes(header: &str, nonce: &[u8], ct: &[u8]) -> Vec<u8> {
    let mut out = header.as_bytes().to_vec();
    out.extend_from_slice(nonce);
    out.extend_from_slice(ct);
    out
}

impl Message {
    /// Seals `body` under `key` for `header`, signed as `sender`.
    pub fn seal(
        header: &Header,
        body: &Body,
        key: &EpochKey,
        sender: &MemberSecret,
    ) -> Result<Self> {
        if body.text.chars().count() > MAX_TEXT_CHARS {
            return Err(ChatError::Invalid(format!(
                "a message is at most {MAX_TEXT_CHARS} characters"
            )));
        }
        let header_text =
            serde_json::to_string(header).map_err(|e| ChatError::Invalid(e.to_string()))?;
        let plain = serde_json::to_vec(body).map_err(|e| ChatError::Invalid(e.to_string()))?;
        let (nonce, ct) = key
            .message_key(&message_context(header))?
            .seal(header_text.as_bytes(), &plain)?;
        let signature = sender
            .sign
            .sign(&signed_message_bytes(&header_text, &nonce, &ct))?;
        let message = Self {
            header: header_text,
            nonce: hex::encode(nonce),
            ct: hex::encode(ct),
            signature: hex::encode(signature),
            seq: 0,
            at: None,
        };
        if message.ct.len() / 2 > MAX_SEALED_BYTES {
            return Err(ChatError::Invalid("the message is too long".into()));
        }
        Ok(message)
    }

    /// The header, parsed and checked for shape.
    pub fn header(&self) -> Result<Header> {
        let header: Header = serde_json::from_str(&self.header)
            .map_err(|_| ChatError::Invalid("a message's header doesn't parse".into()))?;
        if header.v != 1 || !is_conversation_id(&header.conversation) || !is_message_id(&header.id)
        {
            return Err(ChatError::Invalid("a message's header is malformed".into()));
        }
        if unhex(&self.ct, "a message")?.len() > MAX_SEALED_BYTES {
            return Err(ChatError::Invalid("the message is too long".into()));
        }
        Ok(header)
    }

    /// Whether the member key `sender` signed it.
    pub fn verify(&self, sender: &MemberKey) -> Result<()> {
        let header = self.header()?;
        if header.key != sender.id || header.sender != sender.login {
            return Err(ChatError::Signature);
        }
        let (_, sign) = sender.keys()?;
        let nonce = unhex(&self.nonce, "a nonce")?;
        let ct = unhex(&self.ct, "a message")?;
        if sign.verify(
            &signed_message_bytes(&self.header, &nonce, &ct),
            &unhex(&self.signature, "a signature")?,
        ) {
            Ok(())
        } else {
            Err(ChatError::Signature)
        }
    }

    /// Opens it with its epoch's key. Check [`Message::verify`] first.
    pub fn open(&self, key: &EpochKey) -> Result<Body> {
        let header = self.header()?;
        let plain = key.message_key(&message_context(&header))?.open(
            &unhex(&self.nonce, "a nonce")?,
            self.header.as_bytes(),
            &unhex(&self.ct, "a message")?,
        )?;
        serde_json::from_slice(&plain).map_err(|_| ChatError::Open)
    }
}

// ---- The device API's requests and answers -------------------------------

/// `POST /device/v1/chat/keys`: register a person's key on this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterKey {
    pub key: MemberKey,
}

/// `POST /device/v1/chat/keys/lookup`: people's member keys, and IT's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupKeys {
    pub logins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keys {
    pub keys: Vec<MemberKey>,
    /// IT's compliance key (hex), which every epoch is also sealed to.
    pub compliance: String,
}

/// `POST /device/v1/chat/conversations`: a new conversation and its
/// first epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateConversation {
    /// The computer asking. Slice C1 takes its word with the enrollment
    /// token; signed device requests come with ADR 0028's device keys.
    pub device: String,
    pub conversation: Conversation,
    pub epoch: Epoch,
}

/// `POST /device/v1/chat/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostMessage {
    /// The computer posting (see [`CreateConversation::device`]).
    pub device: String,
    pub message: Message,
}

/// `POST /device/v1/chat/sync`: what changed for the people on this
/// computer since `since`, waiting up to `wait_secs` for something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSyncRequest {
    pub device: String,
    /// The logins with a member key on this computer.
    pub logins: Vec<String>,
    #[serde(default)]
    pub since: u64,
    #[serde(default)]
    pub wait_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSyncResponse {
    /// The conversations those people are in.
    #[serde(default)]
    pub conversations: Vec<Conversation>,
    /// Their epochs, whole: the maker's signature covers every envelope.
    #[serde(default)]
    pub epochs: Vec<Epoch>,
    /// Messages after `since`, oldest first.
    #[serde(default)]
    pub messages: Vec<Message>,
    /// The member keys that signed what's here.
    #[serde(default)]
    pub keys: Vec<MemberKey>,
    /// Ask again with this.
    pub cursor: u64,
}

/// Why a request to the console didn't go through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The console couldn't be reached, or didn't accept this computer.
    #[error("{0}")]
    Unreachable(String),
    /// The console refused the request, and says why.
    #[error("{0}")]
    Refused(String),
}

/// Why chat, or a meeting, couldn't do something on a computer, in words
/// for a refusal.
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

impl From<ChatError> for ChatFailure {
    fn from(e: ChatError) -> Self {
        Self::Local(e.to_string())
    }
}

impl From<CryptoError> for ChatFailure {
    fn from(e: CryptoError) -> Self {
        Self::Local(e.to_string())
    }
}

/// How a computer reaches its console for chat: the device API's chat
/// routes (ADR 0030). glassd's goes through `ConsoleClient`, the one crate
/// that leaves the machine; tests use a fake. Every method blocks.
pub trait ChatLink: crate::shared::BlobLink + Send + Sync + std::fmt::Debug {
    fn register(&self, request: &RegisterKey) -> std::result::Result<(), LinkError>;
    fn lookup(&self, request: &LookupKeys) -> std::result::Result<Keys, LinkError>;
    /// The new conversation's sequence number.
    fn create(&self, request: &CreateConversation) -> std::result::Result<u64, LinkError>;
    /// The message's sequence number.
    fn post(&self, request: &PostMessage) -> std::result::Result<u64, LinkError>;
    /// A conversation's next epoch, changing who is in it; its sequence
    /// number. The console takes the first one based on the current epoch
    /// and refuses the rest.
    fn rotate(&self, request: &RotateEpoch) -> std::result::Result<u64, LinkError>;
    fn sync(&self, request: &ChatSyncRequest) -> std::result::Result<ChatSyncResponse, LinkError>;
}

/// A console in memory for tests of computers' chat: it keeps and relays
/// records with the console's main checks, and answers at once.
pub mod testing {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[derive(Debug, Default)]
    struct Kept {
        keys: Vec<MemberKey>,
        conversations: Vec<(Conversation, Vec<Epoch>)>,
        messages: Vec<Message>,
        seq: u64,
        blobs: Vec<crate::shared::SealedBlob>,
        /// (conversation, login, sequence number) for each person added.
        joined: Vec<(String, String, u64)>,
    }

    #[derive(Debug)]
    pub struct MemoryLink {
        compliance: String,
        kept: Mutex<Kept>,
        offline: AtomicBool,
    }

    fn refused<T>(m: &str) -> std::result::Result<T, LinkError> {
        Err(LinkError::Refused(m.into()))
    }

    impl MemoryLink {
        /// A console whose org's compliance key is `compliance`.
        pub fn new(compliance: &PublicKey) -> Self {
            Self {
                compliance: hex::encode(compliance.to_bytes()),
                kept: Mutex::new(Kept::default()),
                offline: AtomicBool::new(false),
            }
        }

        /// Out of reach, or back.
        pub fn offline(&self, offline: bool) {
            self.offline.store(offline, Ordering::SeqCst);
        }

        fn kept(&self) -> std::result::Result<std::sync::MutexGuard<'_, Kept>, LinkError> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(LinkError::Unreachable("connection refused".into()));
            }
            Ok(self
                .kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner))
        }

        /// Every message posted, in order.
        pub fn messages(&self) -> Vec<Message> {
            self.kept
                .lock()
                .map(|k| k.messages.clone())
                .unwrap_or_default()
        }

        /// Keeps `message` as if posted, unchecked: what a dishonest
        /// console could send.
        pub fn inject(&self, mut message: Message) {
            if let Ok(mut kept) = self.kept.lock() {
                kept.seq += 1;
                message.seq = kept.seq;
                kept.messages.push(message);
            }
        }

        /// Every blob kept, as the console keeps it.
        pub fn blobs(&self) -> Vec<crate::shared::SealedBlob> {
            self.kept
                .lock()
                .map(|k| k.blobs.clone())
                .unwrap_or_default()
        }

        /// Every conversation made, with its epochs.
        pub fn conversations(&self) -> Vec<(Conversation, Vec<Epoch>)> {
            self.kept
                .lock()
                .map(|k| k.conversations.clone())
                .unwrap_or_default()
        }

        /// The registered member key `id`, for the meetings console.
        pub(crate) fn key(&self, id: &str) -> Option<MemberKey> {
            self.kept
                .lock()
                .ok()?
                .keys
                .iter()
                .find(|k| k.id == id)
                .cloned()
        }

        /// Conversation `id`, for the meetings console.
        pub(crate) fn conversation(&self, id: &str) -> Option<Conversation> {
            self.kept
                .lock()
                .ok()?
                .conversations
                .iter()
                .find(|(c, _)| c.id == id)
                .map(|(c, _)| c.clone())
        }
    }

    impl MemoryLink {
        /// Whether `login`, on `device` with a registered key, is in
        /// `conversation`: the console's check for blobs.
        fn member_here(
            kept: &Kept,
            device: &str,
            login: &str,
            conversation: &str,
        ) -> std::result::Result<(), LinkError> {
            if !kept
                .keys
                .iter()
                .any(|k| k.login == login && k.device == device)
            {
                return refused("That person has no key on this computer.");
            }
            let member = kept
                .conversations
                .iter()
                .any(|(c, _)| c.id == conversation && c.members.iter().any(|m| m == login));
            if member {
                Ok(())
            } else {
                refused("You aren't in it.")
            }
        }
    }

    impl crate::shared::BlobLink for MemoryLink {
        fn put(&self, request: &crate::shared::PutBlob) -> std::result::Result<(), LinkError> {
            let mut kept = self.kept()?;
            request.blob.check().map_err(LinkError::Refused)?;
            Self::member_here(
                &kept,
                &request.device,
                &request.login,
                &request.blob.conversation,
            )?;
            if kept.blobs.iter().any(|b| b.id == request.blob.id) {
                return refused("That blob is kept already.");
            }
            kept.blobs.push(request.blob.clone());
            Ok(())
        }

        fn get(
            &self,
            request: &crate::shared::GetBlob,
        ) -> std::result::Result<crate::shared::SealedBlob, LinkError> {
            let kept = self.kept()?;
            Self::member_here(
                &kept,
                &request.device,
                &request.login,
                &request.conversation,
            )?;
            kept.blobs
                .iter()
                .find(|b| b.id == request.id && b.conversation == request.conversation)
                .cloned()
                .ok_or_else(|| LinkError::Refused("No such blob.".into()))
        }
    }

    impl ChatLink for MemoryLink {
        fn register(&self, request: &RegisterKey) -> std::result::Result<(), LinkError> {
            let mut kept = self.kept()?;
            let key = &request.key;
            if key.keys().is_err() {
                return refused("a member key doesn't check");
            }
            if !kept.keys.iter().any(|k| k.id == key.id) {
                kept.keys
                    .retain(|k| !(k.login == key.login && k.device == key.device));
                kept.keys.push(key.clone());
            }
            Ok(())
        }

        fn lookup(&self, request: &LookupKeys) -> std::result::Result<Keys, LinkError> {
            let kept = self.kept()?;
            Ok(Keys {
                keys: kept
                    .keys
                    .iter()
                    .filter(|k| request.logins.contains(&k.login))
                    .cloned()
                    .collect(),
                compliance: self.compliance.clone(),
            })
        }

        fn create(&self, request: &CreateConversation) -> std::result::Result<u64, LinkError> {
            let mut kept = self.kept()?;
            let conversation = &request.conversation;
            if kept
                .conversations
                .iter()
                .any(|(c, _)| c.id == conversation.id)
            {
                return refused("That conversation exists already.");
            }
            if conversation.kind == ConversationKind::Meeting
                && conversation.members.len() > crate::meet::MAX_PEOPLE
            {
                return refused("A meeting is for up to 25 people.");
            }
            let maker = kept
                .keys
                .iter()
                .find(|k| k.id == request.epoch.body.made_by && k.device == request.device);
            if !maker.is_some_and(|k| request.epoch.verify(k).is_ok()) {
                return refused("The epoch's signature doesn't check.");
            }
            if !request
                .epoch
                .body
                .envelopes
                .iter()
                .any(|e| e.key == COMPLIANCE)
            {
                return refused("Every epoch is sealed to IT's compliance key too.");
            }
            kept.seq += 1;
            kept.conversations
                .push((request.conversation.clone(), vec![request.epoch.clone()]));
            Ok(kept.seq)
        }

        fn post(&self, request: &PostMessage) -> std::result::Result<u64, LinkError> {
            let mut kept = self.kept()?;
            let Ok(header) = request.message.header() else {
                return refused("a message's header is malformed");
            };
            let Some((conversation, _)) = kept
                .conversations
                .iter()
                .find(|(c, _)| c.id == header.conversation)
            else {
                return refused("No such conversation.");
            };
            if !conversation.members.contains(&header.sender) {
                return refused("The sender isn't in this conversation.");
            }
            if header.epoch != conversation.epoch {
                return refused("The message is sealed under an old epoch; seal it again.");
            }
            let key = kept.keys.iter().find(|k| {
                k.id == header.key && k.login == header.sender && k.device == request.device
            });
            if !key.is_some_and(|k| request.message.verify(k).is_ok()) {
                return refused("The message's signature doesn't check.");
            }
            if kept.messages.iter().any(|m| {
                m.header()
                    .is_ok_and(|h| h.conversation == header.conversation && h.id == header.id)
            }) {
                return refused("That message was posted already.");
            }
            kept.seq += 1;
            let mut message = request.message.clone();
            message.seq = kept.seq;
            message.at = Some("2026-10-19T14:02:11Z".into());
            kept.messages.push(message);
            Ok(kept.seq)
        }

        fn rotate(&self, request: &RotateEpoch) -> std::result::Result<u64, LinkError> {
            let mut kept = self.kept()?;
            let epoch = &request.epoch;
            let maker = kept
                .keys
                .iter()
                .find(|k| k.id == epoch.body.made_by && k.device == request.device)
                .cloned();
            let Some(maker) = maker.filter(|k| epoch.verify(k).is_ok()) else {
                return refused("The epoch's signature doesn't check.");
            };
            let Some(at) = kept
                .conversations
                .iter()
                .position(|(c, _)| c.id == epoch.body.conversation)
            else {
                return refused("No such conversation.");
            };
            let (conversation, _) = &kept.conversations[at];
            if epoch.body.based_on != Some(conversation.epoch) {
                return refused("Someone changed it first; fetch the newest epoch.");
            }
            let after = check_change(
                conversation.kind,
                &conversation.members,
                conversation.epoch,
                epoch,
                &maker.login,
            )
            .map_err(|e| LinkError::Refused(e.to_string()))?;
            let want: std::collections::BTreeSet<&str> = kept
                .keys
                .iter()
                .filter(|k| after.contains(&k.login))
                .map(|k| k.id.as_str())
                .chain([COMPLIANCE])
                .collect();
            let have: std::collections::BTreeSet<&str> = epoch
                .body
                .envelopes
                .iter()
                .map(|e| e.key.as_str())
                .collect();
            if have != want || have.len() != epoch.body.envelopes.len() {
                return refused(
                    "The epoch must be sealed to exactly its members' computers and IT.",
                );
            }
            let added: Vec<String> = after
                .iter()
                .filter(|l| !conversation.members.contains(l))
                .cloned()
                .collect();
            kept.seq += 1;
            let seq = kept.seq;
            let id = epoch.body.conversation.clone();
            for login in added {
                kept.joined.push((id.clone(), login, seq));
            }
            let (conversation, epochs) = &mut kept.conversations[at];
            conversation.members = after;
            conversation.epoch = epoch.body.number;
            epochs.push(epoch.clone());
            Ok(seq)
        }

        fn sync(
            &self,
            request: &ChatSyncRequest,
        ) -> std::result::Result<ChatSyncResponse, LinkError> {
            let kept = self.kept()?;
            let logins: Vec<&String> = kept
                .keys
                .iter()
                .filter(|k| k.device == request.device && request.logins.contains(&k.login))
                .map(|k| &k.login)
                .collect();
            let theirs: Vec<&(Conversation, Vec<Epoch>)> = kept
                .conversations
                .iter()
                .filter(|(c, _)| c.members.iter().any(|m| logins.contains(&m)))
                .collect();
            let ids: Vec<&str> = theirs.iter().map(|(c, _)| c.id.as_str()).collect();
            let people: Vec<&String> = theirs.iter().flat_map(|(c, _)| &c.members).collect();
            Ok(ChatSyncResponse {
                conversations: theirs.iter().map(|(c, _)| c.clone()).collect(),
                epochs: theirs.iter().flat_map(|(_, e)| e.clone()).collect(),
                messages: kept
                    .messages
                    .iter()
                    .filter(|m| {
                        m.header().is_ok_and(|h| {
                            ids.contains(&h.conversation.as_str())
                                && (m.seq > request.since
                                    // Someone added since gets the history.
                                    || kept.joined.iter().any(|(c, l, at)| {
                                        *c == h.conversation
                                            && logins.contains(&l)
                                            && *at > request.since
                                    }))
                        })
                    })
                    .cloned()
                    .collect(),
                keys: kept
                    .keys
                    .iter()
                    .filter(|k| people.contains(&&k.login))
                    .cloned()
                    .collect(),
                cursor: kept.seq,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(login: &str) -> (MemberKey, MemberSecret) {
        let secret = MemberSecret::generate().unwrap();
        (secret.public(login, "dev-1").unwrap(), secret)
    }

    #[test]
    fn cf91_a_message_opens_for_members_and_its_signature_names_the_sender() {
        let (priya, priya_s) = person("priya");
        let (dana, dana_s) = person("dana");
        let it = SecretKey::generate().unwrap();
        let cv = new_conversation_id().unwrap();
        let key = EpochKey::generate().unwrap();
        let epoch = Epoch::make(
            &cv,
            1,
            &key,
            None,
            &[priya.clone(), dana.clone()],
            &it.public().unwrap(),
            (&priya, &priya_s),
        )
        .unwrap();
        epoch.verify(&priya).unwrap();
        assert!(epoch.verify(&dana).is_err(), "dana didn't make it");
        // Each member's computer, and IT, open the same key.
        assert_eq!(epoch.open(&dana.id, &dana_s.agree).unwrap(), key);
        assert_eq!(epoch.open(COMPLIANCE, &it).unwrap(), key);
        assert!(epoch.open(&dana.id, &priya_s.agree).is_err());

        let header = Header {
            v: 1,
            org: "acme".into(),
            conversation: cv.clone(),
            epoch: 1,
            id: new_message_id(1_760_000_000_000).unwrap(),
            sender: "priya".into(),
            key: priya.id.clone(),
        };
        let body = Body {
            text: "The Northwind terms are in".into(),
            attribution: None,
            reply_to: None,
            sent_at: "2026-10-19T14:02:11Z".into(),
            part: None,
            mentions: Vec::new(),
        };
        let message = Message::seal(&header, &body, &key, &priya_s).unwrap();
        message.verify(&priya).unwrap();
        assert_eq!(message.open(&key).unwrap(), body);
        assert!(!message.ct.contains(&hex::encode("Northwind")));

        // Dana can't sign as Priya, nor change who sent it.
        let forged = Message::seal(&header, &body, &key, &dana_s).unwrap();
        assert_eq!(forged.verify(&priya), Err(ChatError::Signature));
        let mut changed = message.clone();
        changed.header = changed.header.replace("\"priya\"", "\"dana\"");
        assert!(changed.verify(&dana).is_err());
        assert!(
            changed.open(&key).is_err(),
            "the header is bound to the seal"
        );

        let name = seal_name(&key, &cv, 1, "#northwind-deal").unwrap();
        assert_eq!(open_name(&key, &cv, 1, &name).unwrap(), "#northwind-deal");
        assert!(!name.contains(&hex::encode("northwind")));
    }

    #[test]
    fn cf21_a_meeting_is_a_conversation_kind_on_the_wire() {
        let kinds = [
            (ConversationKind::Direct, "\"direct\""),
            (ConversationKind::Group, "\"group\""),
            (ConversationKind::Channel, "\"channel\""),
            (ConversationKind::Meeting, "\"meeting\""),
        ];
        for (kind, wire) in kinds {
            assert_eq!(serde_json::to_string(&kind).unwrap(), wire);
            assert_eq!(
                serde_json::from_str::<ConversationKind>(wire).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn cf91_ids_and_member_keys_are_checked() {
        assert!(is_conversation_id(&new_conversation_id().unwrap()));
        assert!(!is_conversation_id("cv-../../etc"));
        assert!(is_message_id(&new_message_id(1).unwrap()));
        assert!(!is_message_id("0192F3"));
        let (mut key, _) = person("sam");
        key.id = "mk-0000000000000000".into();
        assert!(key.keys().is_err(), "an id must match its keys");
    }
}
