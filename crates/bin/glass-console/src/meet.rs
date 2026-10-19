//! Meetings on our servers (ADR 0030 §6, slice M1): who is in which
//! meeting's session, the session's media key as its first joiner made it,
//! and the relay that carries the media.
//!
//! Sessions live in memory only. The media key record is sealed to the
//! people's member keys and never to IT's, so the console can't open it;
//! it is still never written to disk, and it goes when the last person
//! leaves. The console checks what it can without opening anything: that
//! a request is signed by a key registered to its sender on that
//! computer, that the sender is in the meeting, that the record is signed
//! by someone in the meeting and sealed to nobody else, and the seats.

use std::collections::BTreeMap;

use glass_managed::chat::{ConversationKind, MemberKey};
use glass_managed::meet::{MAX_PEOPLE, MediaKeyRecord, MediaKeys, MeetOp, MeetRequest};

use crate::chat::ChatState;
use crate::state::Refused;

#[derive(Debug, Default)]
struct Session {
    media: Vec<MediaKeyRecord>,
    /// (login, member key) → participant index, the SFrame KID.
    participants: BTreeMap<(String, String), u32>,
    next: u32,
}

/// Every meeting's session now.
#[derive(Debug, Default)]
pub struct Rooms {
    sessions: BTreeMap<String, Session>,
}

/// A seat in a meeting's session.
#[derive(Debug)]
pub struct Seat {
    pub participant: u32,
    /// Taken by this join, so a relay that refuses it gives it back.
    pub new: bool,
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

fn rule(m: &str) -> Refused {
    Refused::Rule(m.into())
}

/// Checks `request` for meeting `meeting` (the route's): signed by its
/// login's registered key on that computer, by someone in the meeting.
/// The meeting's people.
fn check(chat: &ChatState, meeting: &str, request: &MeetRequest) -> Result<Vec<String>, Refused> {
    let body = &request.body;
    if body.meeting != meeting {
        return Err(Refused::Invalid(vec![
            "the meeting in the path and the request differ".into(),
        ]));
    }
    let signed = chat
        .key(&body.key)
        .is_some_and(|k| request.verify(k).is_ok());
    if !signed {
        return Err(rule("The request's signature doesn't check."));
    }
    match chat.conversations.get(meeting) {
        Some(stored)
            if stored.conversation.kind == ConversationKind::Meeting
                && stored.conversation.members.contains(&body.login) =>
        {
            Ok(stored.conversation.members.clone())
        }
        _ => Err(rule("You aren't in this meeting.")),
    }
}

fn makers(chat: &ChatState, media: &[MediaKeyRecord]) -> Vec<MemberKey> {
    media
        .iter()
        .filter_map(|m| chat.key(&m.body.made_by).cloned())
        .collect()
}

impl Rooms {
    /// A seat for the request's person in meeting `meeting`, with the
    /// session's media key: the first joiner's, which this join must
    /// carry when there is none yet.
    pub fn join(
        &mut self,
        chat: &ChatState,
        meeting: &str,
        request: &MeetRequest,
    ) -> Result<Seat, Refused> {
        let members = check(chat, meeting, request)?;
        let body = &request.body;
        let MeetOp::Join { media, .. } = &body.op else {
            return Err(Refused::Invalid(vec!["this route joins".into()]));
        };
        let session = self.sessions.entry(meeting.to_owned()).or_default();
        if session.media.is_empty() {
            let Some(media) = media else {
                return Err(rule("The meeting has no media key yet; send one."));
            };
            let maker = chat
                .key(&media.body.made_by)
                .filter(|k| members.contains(&k.login));
            // `verify` also refuses a record sealed to IT's key.
            if media.body.meeting != meeting || !maker.is_some_and(|k| media.verify(k).is_ok()) {
                return Err(rule(
                    "The media key isn't signed by someone in the meeting.",
                ));
            }
            session.media.push(media.clone());
        }
        let who = (body.login.clone(), body.key.clone());
        let (participant, new) = match session.participants.get(&who) {
            Some(n) => (*n, false),
            None if session.participants.len() >= MAX_PEOPLE => {
                return Err(rule("The meeting is full: 25 people are in it."));
            }
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
            keys: makers(chat, &media),
            media,
        })
    }

    /// Gives back a seat the relay didn't take.
    pub fn unseat(&mut self, meeting: &str, request: &MeetRequest) {
        let who = (request.body.login.clone(), request.body.key.clone());
        if let Some(session) = self.sessions.get_mut(meeting) {
            session.participants.remove(&who);
            if session.participants.is_empty() {
                self.sessions.remove(meeting);
            }
        }
    }

    /// The session's media keys, for anyone in the meeting: a computer
    /// asks before it joins, to make one only if there is none.
    pub fn keys(
        &self,
        chat: &ChatState,
        meeting: &str,
        request: &MeetRequest,
    ) -> Result<MediaKeys, Refused> {
        check(chat, meeting, request)?;
        if request.body.op != MeetOp::Keys {
            return Err(Refused::Invalid(vec!["this route gives keys".into()]));
        }
        let media = self
            .sessions
            .get(meeting)
            .map(|s| s.media.clone())
            .unwrap_or_default();
        Ok(MediaKeys {
            keys: makers(chat, &media),
            media,
        })
    }

    /// The request's person leaves; the last one out ends the session,
    /// and its media key with it.
    pub fn leave(
        &mut self,
        chat: &ChatState,
        meeting: &str,
        request: &MeetRequest,
    ) -> Result<Left, Refused> {
        check(chat, meeting, request)?;
        if request.body.op != MeetOp::Leave {
            return Err(Refused::Invalid(vec!["this route leaves".into()]));
        }
        let who = (request.body.login.clone(), request.body.key.clone());
        let Some(session) = self.sessions.get_mut(meeting) else {
            return Ok(Left {
                participant: None,
                ended: false,
            });
        };
        let participant = session.participants.remove(&who);
        let ended = session.participants.is_empty();
        if ended {
            self.sessions.remove(meeting);
        }
        Ok(Left { participant, ended })
    }
}
