//! Glasshouse meetings on the wire (CF-21, ADR 0030 decision 6).
//!
//! A meeting is a conversation of kind `meeting`: its organizer and
//! invitees, all in the directory. Its first epoch key seals its title and
//! time, as a channel's name ([`MeetingDetails`]), so IT's compliance key
//! opens those like any conversation's.
//!
//! Media is sealed under a separate, random [`MediaKey`] per meeting
//! session. It travels sealed to the member keys of the meeting's people
//! and to nothing else: [`MediaKeyRecord::make`] has no way to name IT's
//! key, and [`MediaKeyRecord::verify`] refuses a record that carries one.
//! The record is signed by the member key that made it, so the console
//! can't swap in a key of its own.
//!
//! A computer asks the console to join, for the session's keys and to
//! leave through [`MeetLink`], each request signed by the person's member
//! key ([`MeetRequest`]). Calendar and Meet reach the person's keys and
//! meeting conversations through [`Meetings`], which chat's store on the
//! computer implements, so neither app depends on Chat (ADR 0013).

use std::fmt;

use glass_crypto::{Sealed, SecretKey};
use serde::{Deserialize, Serialize};

use crate::chat::{
    COMPLIANCE, ChatError, ChatFailure, Envelope, LinkError, MemberKey, MemberSecret,
    is_conversation_id, open_name, seal_name,
};
use glass_crypto::EpochKey;

pub mod rooms;

/// The most people in a meeting, the organizer included (ADR 0030).
pub const MAX_PEOPLE: usize = 25;
/// The most people an organizer invites: everyone else.
pub const MAX_INVITEES: usize = MAX_PEOPLE - 1;
/// The longest WebRTC offer or answer.
pub const MAX_SDP_BYTES: usize = 64 * 1024;

const MEDIA_INFO: &[u8] = b"glass-meet media v1";

type Result<T> = std::result::Result<T, ChatError>;

fn unhex(s: &str, what: &str) -> Result<Vec<u8>> {
    hex::decode(s).map_err(|_| ChatError::Invalid(format!("{what} isn't hex")))
}

fn compact<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// Whether `id` is a media key's id: `md-` and 16 hex digits.
pub fn is_media_key_id(id: &str) -> bool {
    id.strip_prefix("md-").is_some_and(|rest| {
        rest.len() == 16
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// What a meeting is, sealed under its first epoch key as its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingDetails {
    pub title: String,
    /// Wall-clock minutes in the organizer's time zone, as Calendar keeps
    /// them: `2026-10-20T14:30`.
    pub start: String,
    pub end: String,
}

impl MeetingDetails {
    /// The name a meeting's conversation carries before it's sealed.
    pub fn to_name(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Sealed under `meeting`'s first epoch key, as a channel's name is.
    pub fn seal(&self, key: &EpochKey, meeting: &str) -> Result<String> {
        seal_name(key, meeting, 1, &self.to_name())
    }

    /// What [`Self::seal`] sealed.
    pub fn open(key: &EpochKey, meeting: &str, sealed: &str) -> Result<Self> {
        let text = open_name(key, meeting, 1, sealed)?;
        serde_json::from_str(&text).map_err(|_| ChatError::Open)
    }
}

/// A meeting session's media key: 32 random bytes that every sender's
/// frame key comes from (HKDF, with the participant's index as SFrame's key
/// id). The page holds it in memory only; nothing on the computer writes
/// it down, so it has no `Debug` that shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct MediaKey([u8; 32]);

impl fmt::Debug for MediaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaKey(..)")
    }
}

impl MediaKey {
    pub fn generate() -> Result<Self> {
        Ok(Self(glass_crypto::random()?))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self(bytes.try_into().map_err(|_| ChatError::Open)?))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// As the page gets it in `meet.join`'s and `meet.keys`'s results:
    /// standard base64, which the page's `atob` reads.
    pub fn to_base64(&self) -> String {
        base64(&self.0)
    }
}

/// Standard base64 with padding (RFC 4648 §4).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn media_aad(meeting: &str, id: &str, key: &str) -> Vec<u8> {
    format!("{meeting}|{id}|{key}").into_bytes()
}

/// What a media key's record says, signed by the member key that made it.
/// The signature covers every envelope, so whoever relays it sends it
/// whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaKeyBody {
    /// The meeting's conversation id.
    pub meeting: String,
    /// `md-` and 16 hex digits, chosen by the computer that made it.
    pub id: String,
    /// The key sealed to each member key of the meeting's people. Never
    /// one for [`COMPLIANCE`].
    pub envelopes: Vec<Envelope>,
    /// The member key that made it.
    pub made_by: String,
}

/// A meeting session's media key as the console keeps it until the session
/// ends: sealed to the meeting's people only, and signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaKeyRecord {
    pub body: MediaKeyBody,
    pub signature: String,
}

impl MediaKeyRecord {
    /// Seals `key` to each of `members` and signs it as `maker`. There is
    /// no compliance key to pass: a meeting's media is never sealed to IT
    /// (ADR 0030, CF-21), and nothing outlives the session.
    pub fn make(
        meeting: &str,
        key: &MediaKey,
        members: &[MemberKey],
        maker: (&MemberKey, &MemberSecret),
    ) -> Result<Self> {
        let id = format!("md-{}", hex::encode(glass_crypto::random::<8>()?));
        let mut envelopes = Vec::with_capacity(members.len());
        for member in members {
            let (agree, _) = member.keys()?;
            let sealed = glass_crypto::seal(
                &agree,
                MEDIA_INFO,
                &media_aad(meeting, &id, &member.id),
                key.as_bytes(),
            )?;
            envelopes.push(Envelope {
                key: member.id.clone(),
                enc: hex::encode(sealed.enc),
                ct: hex::encode(sealed.ct),
            });
        }
        let body = MediaKeyBody {
            meeting: meeting.into(),
            id,
            envelopes,
            made_by: maker.0.id.clone(),
        };
        let signature = hex::encode(maker.1.sign.sign(&compact(&body))?);
        Ok(Self { body, signature })
    }

    /// Whether `maker` (the member key `made_by` names) signed it, and it
    /// is shaped as a media key may be: an envelope for IT's compliance
    /// key is refused.
    pub fn verify(&self, maker: &MemberKey) -> Result<()> {
        if !is_conversation_id(&self.body.meeting) || !is_media_key_id(&self.body.id) {
            return Err(ChatError::Invalid(
                "a media key's record is malformed".into(),
            ));
        }
        if self.body.envelopes.iter().any(|e| e.key == COMPLIANCE) {
            return Err(ChatError::Invalid(
                "a meeting's media key was sealed to IT's compliance key, which it never is".into(),
            ));
        }
        if maker.id != self.body.made_by {
            return Err(ChatError::Signature);
        }
        let (_, sign) = maker.keys()?;
        if sign.verify(
            &compact(&self.body),
            &unhex(&self.signature, "a signature")?,
        ) {
            Ok(())
        } else {
            Err(ChatError::Signature)
        }
    }

    /// Opens the envelope sealed to member key `key_id` with its agreement
    /// key `secret`. Check [`Self::verify`] first.
    pub fn open(&self, key_id: &str, secret: &SecretKey) -> Result<MediaKey> {
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
            MEDIA_INFO,
            &media_aad(&self.body.meeting, &self.body.id, key_id),
        )?;
        MediaKey::from_bytes(&bytes)
    }
}

/// What a member asks of a meeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MeetOp {
    /// Join the meeting's session with the page's WebRTC offer (SDP). The
    /// console checks membership and the 25-person limit, picks a relay
    /// and hands it the offer.
    Join {
        offer: String,
        /// A media key this computer made, for a meeting with no session
        /// yet. The console keeps the first it gets for a session and
        /// answers everyone with that one.
        #[serde(default)]
        media: Option<MediaKeyRecord>,
    },
    /// The session's current media keys (several while one replaces
    /// another).
    Keys,
    /// The new media key that was due, sealed to exactly the people in
    /// the session now (someone who held the last one left, or someone
    /// present can't open it).
    Rekey { media: MediaKeyRecord },
    /// Leave the session.
    Leave,
}

/// A request about one meeting, as its member key signs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetBody {
    /// The meeting's conversation id.
    pub meeting: String,
    pub op: MeetOp,
    /// The computer asking, as the fleet names it.
    pub device: String,
    pub login: String,
    /// The member key that signs.
    pub key: String,
    /// When it was signed (RFC 3339): the console refuses one more than
    /// two minutes off, and replays.
    pub at: String,
}

/// `POST /device/v1/meet/<id>/join`, `…/keys` and `…/leave`: a
/// [`MeetBody`] and its member key's signature over the body as compact
/// JSON, in field order (as an epoch's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetRequest {
    pub body: MeetBody,
    pub signature: String,
}

impl MeetRequest {
    /// Signs `body` with the member key's signing half.
    pub fn sign(body: MeetBody, secret: &MemberSecret) -> Result<Self> {
        let signature = hex::encode(secret.sign.sign(&compact(&body))?);
        Ok(Self { body, signature })
    }

    /// Whether `key` signed it, and is the person's key on the computer it
    /// names.
    pub fn verify(&self, key: &MemberKey) -> Result<()> {
        let body = &self.body;
        if key.id != body.key || key.login != body.login || key.device != body.device {
            return Err(ChatError::Signature);
        }
        let (_, sign) = key.keys()?;
        if sign.verify(&compact(body), &unhex(&self.signature, "a signature")?) {
            Ok(())
        } else {
            Err(ChatError::Signature)
        }
    }
}

/// What the console answers a join with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Joined {
    /// The relay's WebRTC answer (SDP).
    pub answer: String,
    /// This participant's index in the session: SFrame's key id for what
    /// it sends.
    pub participant: u32,
    /// The session's media keys, current first, each whole.
    #[serde(default)]
    pub media: Vec<MediaKeyRecord>,
    /// The member keys that made them.
    #[serde(default)]
    pub keys: Vec<MemberKey>,
}

/// What the console answers a request for keys with: nothing when the
/// meeting has no session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaKeys {
    /// Current first, each whole.
    #[serde(default)]
    pub media: Vec<MediaKeyRecord>,
    /// The member keys that made them.
    #[serde(default)]
    pub keys: Vec<MemberKey>,
    /// The member keys of the people in the session now: what a new
    /// media key is sealed to.
    #[serde(default)]
    pub participants: Vec<MemberKey>,
    /// A new media key is due: someone who held the current one left, or
    /// someone present can't open it.
    #[serde(default)]
    pub due: bool,
}

/// How a computer reaches its console for meetings: the device API's
/// meeting routes (ADR 0030, decision 5). Every request is a
/// [`MeetRequest`] whose op matches the route, and every method blocks.
/// glassd's goes through `ConsoleClient`; tests use
/// [`testing::MemoryMeet`].
///
/// The console checks, without reading media: the request's signature by
/// a member key registered to that login on that computer; that the login
/// is in the meeting's conversation; the 25-person limit; and that a media
/// key it keeps is signed by a member's key and sealed to no compliance
/// key. It deletes a session's media keys when the session ends.
pub trait MeetLink: Send + Sync + fmt::Debug {
    /// `POST /device/v1/meet/<id>/join`: the offer in; the relay's answer,
    /// the participant's index and the session's media keys back.
    fn join(&self, request: &MeetRequest) -> std::result::Result<Joined, LinkError>;
    /// `POST /device/v1/meet/<id>/keys`: the session's media keys, for a
    /// member; none before anyone joins.
    fn keys(&self, request: &MeetRequest) -> std::result::Result<MediaKeys, LinkError>;
    /// `POST /device/v1/meet/<id>/rekey`: the new media key that was due;
    /// the session's keys back.
    fn rekey(&self, request: &MeetRequest) -> std::result::Result<MediaKeys, LinkError>;
    /// `POST /device/v1/meet/<id>/leave`.
    fn leave(&self, request: &MeetRequest) -> std::result::Result<(), LinkError>;
}

/// One meeting a person is in, as their computer opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingInfo {
    /// The conversation's id.
    pub id: String,
    pub organizer: String,
    /// Logins, the organizer included.
    pub members: Vec<String>,
    /// Title and time; none when this computer can't open them.
    pub details: Option<MeetingDetails>,
}

/// What meetings need of chat on a computer: the person's member key and
/// the meeting conversations glassd keeps for them. Chat's store implements
/// it (`glass-app-chat`); Calendar and Meet take it, so neither depends on
/// Chat (ADR 0013).
pub trait Meetings: Send + Sync + fmt::Debug {
    /// Whether this computer can have meetings: managed, and IT has set
    /// chat up.
    fn ready(&self) -> std::result::Result<(), ChatFailure>;

    /// `login`'s member key here. With `make`, the person is in one of
    /// their own apps, so one is made and registered when there's none; an
    /// agent's call never makes one (ADR 0030).
    fn member(
        &self,
        login: &str,
        make: bool,
    ) -> std::result::Result<Option<(MemberKey, MemberSecret)>, ChatFailure>;

    /// The meetings `login` is in on this computer.
    fn meetings(&self, login: &str) -> Vec<MeetingInfo>;

    /// Makes meeting `id`'s conversation of `people` (the organizer
    /// included): its title and time sealed under its first epoch key,
    /// sealed to each of their member keys and to IT, signed by the
    /// organizer's member key and registered with the console. `make` as
    /// for [`Self::member`].
    fn create(
        &self,
        organizer: &str,
        make: bool,
        id: &str,
        people: &[String],
        details: &MeetingDetails,
    ) -> std::result::Result<(), ChatFailure>;

    /// The member keys the console has for `logins`, each checked against
    /// its id.
    fn lookup(&self, logins: &[String]) -> std::result::Result<Vec<MemberKey>, ChatFailure>;
}

/// A console in memory for tests of meetings: it keeps sessions with the
/// console's checks and answers at once, beside the chat console whose
/// keys and conversations it reads.
pub mod testing {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use super::rooms::{Facts, RoomRefusal, Rooms};
    use super::*;
    use crate::chat::ConversationKind;
    use crate::chat::testing::MemoryLink;

    /// The console's meetings in memory: the same [`Rooms`] the console
    /// runs, beside a [`MemoryLink`]'s chat.
    #[derive(Debug)]
    pub struct MemoryMeet {
        chat: Arc<MemoryLink>,
        rooms: Mutex<Rooms>,
        offers: Mutex<Vec<String>>,
        offline: AtomicBool,
    }

    struct ChatFacts<'a>(&'a MemoryLink);

    impl Facts for ChatFacts<'_> {
        fn key(&self, id: &str) -> Option<MemberKey> {
            self.0.key(id)
        }
        fn meeting(&self, id: &str) -> Option<Vec<String>> {
            self.0
                .conversation(id)
                .filter(|c| c.kind == ConversationKind::Meeting)
                .map(|c| c.members)
        }
    }

    fn refused(r: RoomRefusal) -> LinkError {
        LinkError::Refused(r.to_string())
    }

    impl MemoryMeet {
        /// Meetings beside `chat`, the same console's chat.
        pub fn new(chat: Arc<MemoryLink>) -> Self {
            Self {
                chat,
                rooms: Mutex::new(Rooms::default()),
                offers: Mutex::new(Vec::new()),
                offline: AtomicBool::new(false),
            }
        }

        /// Out of reach, or back.
        pub fn offline(&self, offline: bool) {
            self.offline.store(offline, Ordering::SeqCst);
        }

        fn rooms(&self) -> std::sync::MutexGuard<'_, Rooms> {
            self.rooms.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn online(&self) -> std::result::Result<(), LinkError> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(LinkError::Unreachable("connection refused".into()));
            }
            Ok(())
        }

        /// The media keys meeting `id`'s session keeps; none once it ends.
        pub fn media(&self, id: &str) -> Vec<MediaKeyRecord> {
            self.rooms().media(id)
        }

        /// Who is in meeting `id`'s session, with their indexes.
        pub fn participants(&self, id: &str) -> Vec<(String, u32)> {
            self.rooms().participants(id)
        }

        /// Every offer a join carried, in order.
        pub fn offers(&self) -> Vec<String> {
            self.offers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl MeetLink for MemoryMeet {
        fn join(&self, request: &MeetRequest) -> std::result::Result<Joined, LinkError> {
            self.online()?;
            let MeetOp::Join { offer, .. } = &request.body.op else {
                return Err(LinkError::Refused("This route joins.".into()));
            };
            let meeting = &request.body.meeting;
            let seat = self
                .rooms()
                .join(&ChatFacts(&self.chat), meeting, request)
                .map_err(refused)?;
            self.offers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(offer.clone());
            Ok(Joined {
                answer: format!(
                    "v=0\r\ns=glass-sfu answer for participant {}\r\n",
                    seat.participant
                ),
                participant: seat.participant,
                media: seat.media,
                keys: seat.keys,
            })
        }

        fn keys(&self, request: &MeetRequest) -> std::result::Result<MediaKeys, LinkError> {
            self.online()?;
            self.rooms()
                .keys(&ChatFacts(&self.chat), &request.body.meeting, request)
                .map_err(refused)
        }

        fn rekey(&self, request: &MeetRequest) -> std::result::Result<MediaKeys, LinkError> {
            self.online()?;
            self.rooms()
                .rekey(&ChatFacts(&self.chat), &request.body.meeting, request)
                .map_err(refused)
        }

        fn leave(&self, request: &MeetRequest) -> std::result::Result<(), LinkError> {
            self.online()?;
            self.rooms()
                .leave(&ChatFacts(&self.chat), &request.body.meeting, request)
                .map(|_| ())
                .map_err(refused)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::new_conversation_id;

    fn person(login: &str) -> (MemberKey, MemberSecret) {
        let secret = MemberSecret::generate().unwrap();
        (secret.public(login, "dev-1").unwrap(), secret)
    }

    #[test]
    fn cf21_a_media_key_record_is_sealed_to_the_meetings_people_only() {
        let (dana, dana_s) = person("dana");
        let (priya, priya_s) = person("priya");
        let it = SecretKey::generate().unwrap();
        let meeting = new_conversation_id().unwrap();
        let key = MediaKey::generate().unwrap();
        let record = MediaKeyRecord::make(
            &meeting,
            &key,
            &[dana.clone(), priya.clone()],
            (&dana, &dana_s),
        )
        .unwrap();
        record.verify(&dana).unwrap();
        // Only the people's own keys: none for IT, and IT's key opens none.
        let sealed_to: Vec<&str> = record
            .body
            .envelopes
            .iter()
            .map(|e| e.key.as_str())
            .collect();
        assert_eq!(sealed_to, [dana.id.as_str(), priya.id.as_str()]);
        assert!(record.open(COMPLIANCE, &it).is_err());
        for envelope in &record.body.envelopes {
            assert!(record.open(&envelope.key, &it).is_err());
        }
        assert_eq!(record.open(&priya.id, &priya_s.agree).unwrap(), key);
        assert!(record.open(&priya.id, &dana_s.agree).is_err());

        // A record that someone sealed to IT anyway is refused.
        let mut to_it = record.clone();
        to_it.body.envelopes.push(Envelope {
            key: COMPLIANCE.into(),
            enc: to_it.body.envelopes[0].enc.clone(),
            ct: to_it.body.envelopes[0].ct.clone(),
        });
        to_it.signature = hex::encode(dana_s.sign.sign(&compact(&to_it.body)).unwrap());
        assert!(to_it.verify(&dana).is_err());
        // And nobody else can sign as its maker.
        assert_eq!(record.verify(&priya), Err(ChatError::Signature));
        let mut swapped = record.clone();
        swapped.body.envelopes.pop();
        assert_eq!(swapped.verify(&dana), Err(ChatError::Signature));
        assert_eq!(format!("{key:?}"), "MediaKey(..)", "never printed");
    }

    #[test]
    fn cf21_a_meeting_request_is_signed_by_the_persons_member_key() {
        let (dana, dana_s) = person("dana");
        let (priya, _) = person("priya");
        let body = MeetBody {
            meeting: new_conversation_id().unwrap(),
            op: MeetOp::Join {
                offer: "v=0\r\n".into(),
                media: None,
            },
            device: "dev-1".into(),
            login: "dana".into(),
            key: dana.id.clone(),
            at: "2026-10-19T14:02:11Z".into(),
        };
        let request = MeetRequest::sign(body, &dana_s).unwrap();
        request.verify(&dana).unwrap();
        assert!(request.verify(&priya).is_err());
        let mut changed = request.clone();
        changed.body.op = MeetOp::Leave;
        assert!(changed.verify(&dana).is_err());
        // On the wire: the op names itself.
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["body"]["op"]["join"]["offer"], "v=0\r\n");
        let keys = MeetBody {
            op: MeetOp::Keys,
            ..request.body.clone()
        };
        assert_eq!(serde_json::to_value(&keys).unwrap()["op"], "keys");
    }

    #[test]
    fn cf21_a_meetings_title_and_time_are_sealed_like_a_channel_name() {
        let meeting = new_conversation_id().unwrap();
        let key = EpochKey::generate().unwrap();
        let details = MeetingDetails {
            title: "Northwind renewal".into(),
            start: "2026-10-20T14:00".into(),
            end: "2026-10-20T15:00".into(),
        };
        let sealed = details.seal(&key, &meeting).unwrap();
        assert!(!sealed.contains(&hex::encode("Northwind")));
        assert_eq!(
            MeetingDetails::open(&key, &meeting, &sealed).unwrap(),
            details
        );
        // The same as the name a channel would carry.
        assert_eq!(
            open_name(&key, &meeting, 1, &sealed).unwrap(),
            details.to_name()
        );
        assert!(is_media_key_id("md-0123456789abcdef"));
        assert!(!is_media_key_id("md-../../x"));
    }

    #[test]
    fn cf21_the_page_gets_the_media_key_in_base64() {
        // RFC 4648 §10's vectors.
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
        let key = MediaKey::from_bytes(&[0xff; 32]).unwrap();
        assert_eq!(key.to_base64().len(), 44);
        assert!(key.to_base64().starts_with("////"));
    }
}
