//! Meetings' sessions as the console keeps them (ADR 0030 §6): who is in
//! each meeting's session, its media keys, and when a new one is due. The
//! console runs this; tests run the same code behind
//! [`super::testing::MemoryMeet`].
//!
//! Sessions live in memory only. A media key's record is sealed to
//! people's member keys and never to IT's, so the console can't open it;
//! it is still never written to disk, and it goes when the last person
//! leaves. The console checks what it can without opening anything: that
//! a request is signed by a key registered to its sender on that
//! computer, that the sender is in the meeting, that a record is signed by
//! someone in the meeting and sealed to nobody else, and the seats.
//!
//! A new media key is due when someone who holds the current one has left
//! (so they can't read what follows), or when someone present can't open
//! it (their computer's key was registered after it was made). Then the
//! first participant's computer to ask makes one, sealed to exactly the
//! people present ([`Rooms::rekey`]); a joiner who can't open the current
//! key brings one with its join.
//!
//! Someone whose computer drops out of the call (its window closed, the
//! computer asleep or off the network) never says so; the relay sees the
//! connection go. So the console asks the relay who it has before it
//! answers for a meeting's keys, and on a sweep, and anyone it seated
//! before it asked whom the relay no longer has has left
//! ([`Rooms::connected`], ADR 0052).

use std::collections::{BTreeMap, BTreeSet};

use super::{MAX_PEOPLE, MediaKeyRecord, MediaKeys, MeetOp, MeetRequest};
use crate::chat::MemberKey;

/// Media keys a session keeps: the current one and the one before it, for
/// frames in flight.
const KEPT: usize = 2;

/// What the rooms need to know of chat: registered member keys and
/// meetings' people.
pub trait Facts {
    /// The member key registered with id `id`.
    fn key(&self, id: &str) -> Option<MemberKey>;
    /// Meeting `id`'s people, if `id` is a meeting's conversation.
    fn meeting(&self, id: &str) -> Option<Vec<String>>;
}

/// Why the rooms said no.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomRefusal {
    /// Malformed: 400.
    Invalid(String),
    /// A rule says no: 409.
    Rule(String),
}

impl std::fmt::Display for RoomRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Rule(m) => f.write_str(m),
        }
    }
}

fn rule(m: &str) -> RoomRefusal {
    RoomRefusal::Rule(m.into())
}

#[derive(Debug, Default)]
struct Session {
    /// Current first.
    media: Vec<MediaKeyRecord>,
    /// (login, member key) → participant index, the SFrame KID.
    participants: BTreeMap<(String, String), u32>,
    next: u32,
    /// Member keys that left holding the current media key.
    left: BTreeSet<String>,
    /// When the relay took each seat ([`Rooms::admitted`]), on the rooms'
    /// clock. A seat not in it yet is never judged by what the relay has.
    admitted: BTreeMap<(String, String), u64>,
}

impl Session {
    fn present(&self) -> BTreeSet<&str> {
        self.participants.keys().map(|(_, k)| k.as_str()).collect()
    }

    /// The member keys the current media key is sealed to.
    fn holders(&self) -> BTreeSet<&str> {
        self.media
            .first()
            .map(|m| m.body.envelopes.iter().map(|e| e.key.as_str()).collect())
            .unwrap_or_default()
    }

    fn due(&self) -> bool {
        if self.media.is_empty() {
            return false;
        }
        let holders = self.holders();
        self.left.iter().any(|k| holders.contains(k.as_str()))
            || self.present().iter().any(|k| !holders.contains(k))
    }

    /// Takes `record` as the session's new current key.
    fn replace(&mut self, record: MediaKeyRecord) {
        self.media.insert(0, record);
        self.media.truncate(KEPT);
        self.left.clear();
    }

    /// `who` is gone: holding the current key, a new one is due.
    fn remove(&mut self, who: &(String, String)) -> Option<u32> {
        let participant = self.participants.remove(who);
        self.admitted.remove(who);
        if participant.is_some() && self.holders().contains(who.1.as_str()) {
            self.left.insert(who.1.clone());
        }
        participant
    }
}

/// Every meeting's session now.
#[derive(Debug, Default)]
pub struct Rooms {
    sessions: BTreeMap<String, Session>,
    /// Counts the relay's admissions, to tell which seats an answer of the
    /// relay's speaks for ([`Self::mark`]).
    clock: u64,
}

/// A seat in a meeting's session.
#[derive(Debug)]
pub struct Seat {
    pub participant: u32,
    /// Taken by this join, so a relay that refuses it gives it back.
    pub new: bool,
    /// Current first.
    pub media: Vec<MediaKeyRecord>,
    /// Who made the media key records, to check their signatures.
    pub keys: Vec<MemberKey>,
}

/// Who left, and whether the session ended with them.
#[derive(Debug)]
pub struct Left {
    pub participant: Option<u32>,
    pub ended: bool,
}

/// Who dropped out of the call without saying so, and whether the session
/// ended with them ([`Rooms::connected`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Dropped {
    pub participants: Vec<u32>,
    pub ended: bool,
}

/// Checks `request` for meeting `meeting` (the route's): signed by its
/// login's registered key on that computer, by someone in the meeting.
/// The meeting's people.
fn check(
    facts: &dyn Facts,
    meeting: &str,
    request: &MeetRequest,
) -> Result<Vec<String>, RoomRefusal> {
    let body = &request.body;
    if body.meeting != meeting {
        return Err(RoomRefusal::Invalid(
            "the meeting in the path and the request differ".into(),
        ));
    }
    let signed = facts
        .key(&body.key)
        .is_some_and(|k| request.verify(&k).is_ok());
    if !signed {
        return Err(rule("The request's signature doesn't check."));
    }
    match facts.meeting(meeting) {
        Some(people) if people.contains(&body.login) => Ok(people),
        _ => Err(rule("You aren't in this meeting.")),
    }
}

/// Whether `record` is a media key for `meeting` that `maker` (someone in
/// the meeting) signed, sealed to exactly `sealed_to`, when that is given.
fn checked(
    facts: &dyn Facts,
    meeting: &str,
    people: &[String],
    record: &MediaKeyRecord,
    sealed_to: Option<&BTreeSet<&str>>,
) -> Result<(), RoomRefusal> {
    let maker = facts
        .key(&record.body.made_by)
        .filter(|k| people.contains(&k.login));
    // `verify` also refuses a record sealed to IT's key.
    if record.body.meeting != meeting || !maker.is_some_and(|k| record.verify(&k).is_ok()) {
        return Err(rule(
            "The media key isn't signed by someone in the meeting.",
        ));
    }
    if let Some(sealed_to) = sealed_to {
        let envelopes: BTreeSet<&str> = record
            .body
            .envelopes
            .iter()
            .map(|e| e.key.as_str())
            .collect();
        if &envelopes != sealed_to {
            return Err(rule(
                "The new media key must be sealed to the people in the meeting now, and to \
                 nobody else; someone joined or left meanwhile, so ask for keys again.",
            ));
        }
    }
    Ok(())
}

fn makers(facts: &dyn Facts, media: &[MediaKeyRecord]) -> Vec<MemberKey> {
    media
        .iter()
        .filter_map(|m| facts.key(&m.body.made_by))
        .collect()
}

impl Rooms {
    /// A seat for the request's person in meeting `meeting`, with the
    /// session's media keys. The first joiner brings the first key. A
    /// joiner whose computer the current key isn't sealed to brings a new
    /// one, sealed to the people present and itself.
    pub fn join(
        &mut self,
        facts: &dyn Facts,
        meeting: &str,
        request: &MeetRequest,
    ) -> Result<Seat, RoomRefusal> {
        let people = check(facts, meeting, request)?;
        let body = &request.body;
        let MeetOp::Join { media, .. } = &body.op else {
            return Err(RoomRefusal::Invalid("this route joins".into()));
        };
        let session = self.sessions.entry(meeting.to_owned()).or_default();
        let who = (body.login.clone(), body.key.clone());
        if !session.participants.contains_key(&who) && session.participants.len() >= MAX_PEOPLE {
            return Err(rule("The meeting is full: 25 people are in it."));
        }
        if session.media.is_empty() {
            let Some(media) = media else {
                return Err(rule("The meeting has no media key yet; send one."));
            };
            checked(facts, meeting, &people, media, None)?;
            session.media.push(media.clone());
        } else if !session.holders().contains(body.key.as_str()) {
            let Some(media) = media else {
                return Err(rule(
                    "The meeting's media key isn't sealed to this computer; bring a new one.",
                ));
            };
            let mut sealed_to = session.present();
            sealed_to.insert(body.key.as_str());
            if media.body.made_by != body.key {
                return Err(rule("A joiner's new media key must be its own."));
            }
            checked(facts, meeting, &people, media, Some(&sealed_to))?;
            session.replace(media.clone());
        }
        // The relay is about to take (or retake) the seat's connection:
        // until it has, what it says of the seat is out of date.
        session.admitted.remove(&who);
        let (participant, new) = match session.participants.get(&who) {
            Some(n) => (*n, false),
            None => {
                session.next += 1;
                session.participants.insert(who, session.next);
                (session.next, true)
            }
        };
        let media = session.media.clone();
        Ok(Seat {
            participant,
            new,
            keys: makers(facts, &media),
            media,
        })
    }

    /// Gives back a seat the relay didn't take.
    pub fn unseat(&mut self, meeting: &str, request: &MeetRequest) {
        let who = (request.body.login.clone(), request.body.key.clone());
        if let Some(session) = self.sessions.get_mut(meeting) {
            session.participants.remove(&who);
            session.admitted.remove(&who);
            if session.participants.is_empty() {
                self.sessions.remove(meeting);
            }
        }
    }

    /// The relay took the request's seat (or answered about it): from now
    /// on, a seat the relay no longer has is gone ([`Self::connected`]).
    pub fn admitted(&mut self, meeting: &str, request: &MeetRequest) {
        let who = (request.body.login.clone(), request.body.key.clone());
        if let Some(session) = self.sessions.get_mut(meeting)
            && session.participants.contains_key(&who)
        {
            self.clock += 1;
            session.admitted.insert(who, self.clock);
        }
    }

    /// Where the rooms' clock is: taken before asking the relay who it
    /// has, and handed to [`Self::connected`] with its answer.
    pub fn mark(&self) -> u64 {
        self.clock
    }

    /// The relay's answer, asked at `mark`: the participants with a seat
    /// in meeting `meeting`'s room. Each seat it had taken by then that it
    /// no longer has dropped out of the call without saying so (ADR 0052):
    /// it leaves, as [`Self::leave`] would, and a new key is due if it held
    /// the current one; the last one out ends the session. A seat taken
    /// after `mark`, or being taken, isn't judged.
    pub fn connected(&mut self, meeting: &str, mark: u64, connected: &BTreeSet<u32>) -> Dropped {
        let Some(session) = self.sessions.get_mut(meeting) else {
            return Dropped::default();
        };
        let gone: Vec<(String, String)> = session
            .participants
            .iter()
            .filter(|(who, n)| {
                session.admitted.get(*who).is_some_and(|at| *at <= mark) && !connected.contains(n)
            })
            .map(|(who, _)| who.clone())
            .collect();
        let participants = gone.iter().filter_map(|who| session.remove(who)).collect();
        let ended = !gone.is_empty() && session.participants.is_empty();
        if ended {
            self.sessions.remove(meeting);
        }
        Dropped {
            participants,
            ended,
        }
    }

    /// The meetings with a session now.
    pub fn meetings(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    /// The session's media keys, for anyone in the meeting: a computer
    /// asks before it joins, to know whether to bring one, and in the
    /// meeting, to hear of a new one or make it when it is due.
    pub fn keys(
        &self,
        facts: &dyn Facts,
        meeting: &str,
        request: &MeetRequest,
    ) -> Result<MediaKeys, RoomRefusal> {
        check(facts, meeting, request)?;
        if request.body.op != MeetOp::Keys {
            return Err(RoomRefusal::Invalid("this route gives keys".into()));
        }
        Ok(self.answer(facts, meeting))
    }

    fn answer(&self, facts: &dyn Facts, meeting: &str) -> MediaKeys {
        let Some(session) = self.sessions.get(meeting) else {
            return MediaKeys::default();
        };
        MediaKeys {
            keys: makers(facts, &session.media),
            media: session.media.clone(),
            participants: session
                .present()
                .into_iter()
                .filter_map(|k| facts.key(k))
                .collect(),
            due: session.due(),
        }
    }

    /// A participant's computer brings the new media key that was due,
    /// sealed to exactly the people present. The first one wins; later
    /// ones are refused, and their computers ask for keys again.
    pub fn rekey(
        &mut self,
        facts: &dyn Facts,
        meeting: &str,
        request: &MeetRequest,
    ) -> Result<MediaKeys, RoomRefusal> {
        let people = check(facts, meeting, request)?;
        let body = &request.body;
        let MeetOp::Rekey { media } = &body.op else {
            return Err(RoomRefusal::Invalid("this route takes a new key".into()));
        };
        let Some(session) = self.sessions.get_mut(meeting) else {
            return Err(rule("Nobody is in the meeting."));
        };
        if !session
            .participants
            .contains_key(&(body.login.clone(), body.key.clone()))
        {
            return Err(rule("Only someone in the meeting now makes its new key."));
        }
        if !session.due() {
            return Err(rule(
                "The meeting's media key is new already; ask for keys again.",
            ));
        }
        if media.body.made_by != body.key {
            return Err(rule("A new media key must be its sender's own."));
        }
        let sealed_to = session.present();
        checked(facts, meeting, &people, media, Some(&sealed_to))?;
        session.replace(media.clone());
        Ok(self.answer(facts, meeting))
    }

    /// The request's person leaves; the last one out ends the session,
    /// and its media keys with it.
    pub fn leave(
        &mut self,
        facts: &dyn Facts,
        meeting: &str,
        request: &MeetRequest,
    ) -> Result<Left, RoomRefusal> {
        check(facts, meeting, request)?;
        if request.body.op != MeetOp::Leave {
            return Err(RoomRefusal::Invalid("this route leaves".into()));
        }
        let who = (request.body.login.clone(), request.body.key.clone());
        let Some(session) = self.sessions.get_mut(meeting) else {
            return Ok(Left {
                participant: None,
                ended: false,
            });
        };
        let participant = session.remove(&who);
        let ended = session.participants.is_empty();
        if ended {
            self.sessions.remove(meeting);
        }
        Ok(Left { participant, ended })
    }

    /// The media keys meeting `id`'s session keeps; none once it ends.
    pub fn media(&self, id: &str) -> Vec<MediaKeyRecord> {
        self.sessions
            .get(id)
            .map(|s| s.media.clone())
            .unwrap_or_default()
    }

    /// Who is in meeting `id`'s session, with their indexes.
    pub fn participants(&self, id: &str) -> Vec<(String, u32)> {
        self.sessions
            .get(id)
            .map(|s| {
                s.participants
                    .iter()
                    .map(|((login, _), n)| (login.clone(), *n))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::chat::{MemberSecret, new_conversation_id};
    use crate::meet::{MediaKey, MeetBody};

    struct Org {
        keys: HashMap<String, (MemberKey, MemberSecret)>,
        meeting: String,
    }

    impl Facts for Org {
        fn key(&self, id: &str) -> Option<MemberKey> {
            self.keys
                .values()
                .find(|(k, _)| k.id == id)
                .map(|(k, _)| k.clone())
        }
        fn meeting(&self, id: &str) -> Option<Vec<String>> {
            (id == self.meeting).then(|| self.keys.keys().cloned().collect())
        }
    }

    fn org() -> Org {
        let mut keys = HashMap::new();
        for login in ["dana", "priya"] {
            let secret = MemberSecret::generate().unwrap();
            keys.insert(
                login.to_owned(),
                (
                    secret.public(login, &format!("dev-{login}")).unwrap(),
                    secret,
                ),
            );
        }
        Org {
            keys,
            meeting: new_conversation_id().unwrap(),
        }
    }

    impl Org {
        fn request(&self, login: &str, op: MeetOp) -> MeetRequest {
            let (key, secret) = &self.keys[login];
            let body = MeetBody {
                meeting: self.meeting.clone(),
                op,
                device: key.device.clone(),
                login: login.into(),
                key: key.id.clone(),
                at: "2026-10-24T00:00:00Z".into(),
            };
            MeetRequest::sign(body, secret).unwrap()
        }

        /// `login` joins, bringing the session's first key if it has none.
        fn join(&self, rooms: &mut Rooms, login: &str) -> Seat {
            let (key, secret) = &self.keys[login];
            let members: Vec<MemberKey> = self.keys.values().map(|(k, _)| k.clone()).collect();
            let media = MediaKeyRecord::make(
                &self.meeting,
                &MediaKey::generate().unwrap(),
                &members,
                (key, secret),
            )
            .unwrap();
            let op = MeetOp::Join {
                offer: "v=0".into(),
                media: Some(media),
            };
            rooms
                .join(self, &self.meeting, &self.request(login, op))
                .unwrap()
        }
    }

    /// ADR 0052, found by `specs/tla/MeetKeys.tla` with a first version
    /// of the fix: the relay's answer to a question asked before a seat
    /// was taken doesn't have it, and must not count it as dropped out.
    #[test]
    fn adr0052_an_answer_from_before_a_seat_was_taken_doesnt_judge_it() {
        let org = org();
        let mut rooms = Rooms::default();
        let dana = org.join(&mut rooms, "dana");
        rooms.admitted(&org.meeting, &org.request("dana", MeetOp::Leave));
        // Dana's page asks for keys: the console asks the relay...
        let mark = rooms.mark();
        // ...while Priya joins and the relay takes her.
        let priya = org.join(&mut rooms, "priya");
        rooms.admitted(&org.meeting, &org.request("priya", MeetOp::Leave));
        // The relay's answer, from before Priya's seat.
        let dropped = rooms.connected(&org.meeting, mark, &[dana.participant].into());
        assert_eq!(dropped, Dropped::default());
        assert_eq!(rooms.participants(&org.meeting).len(), 2);
        // Asked after, an answer without her means she dropped out.
        let mark = rooms.mark();
        let dropped = rooms.connected(&org.meeting, mark, &[dana.participant].into());
        assert_eq!(dropped.participants, vec![priya.participant]);
        assert!(!dropped.ended);
        let keys = rooms
            .keys(&org, &org.meeting, &org.request("dana", MeetOp::Keys))
            .unwrap();
        assert!(keys.due, "she held the key: a new one is due");
    }

    /// ADR 0052: a computer joining again under its seat isn't judged by
    /// the relay until the relay has taken its new connection.
    #[test]
    fn adr0052_a_seat_being_taken_again_isnt_judged_until_the_relay_has_it() {
        let org = org();
        let mut rooms = Rooms::default();
        let dana = org.join(&mut rooms, "dana");
        rooms.admitted(&org.meeting, &org.request("dana", MeetOp::Leave));
        // Its connection dropped; it joins again before anyone asked.
        let again = org.join(&mut rooms, "dana");
        assert_eq!(again.participant, dana.participant);
        let mark = rooms.mark();
        assert_eq!(
            rooms.connected(&org.meeting, mark, &BTreeSet::new()),
            Dropped::default()
        );
        // The last seat the relay lost ends the session, keys and all.
        rooms.admitted(&org.meeting, &org.request("dana", MeetOp::Leave));
        let mark = rooms.mark();
        let dropped = rooms.connected(&org.meeting, mark, &BTreeSet::new());
        assert!(dropped.ended);
        assert!(rooms.media(&org.meeting).is_empty());
        assert!(rooms.meetings().is_empty());
    }
}
