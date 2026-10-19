//! The `meet` app's actions (CF-21, ADR 0030 decision 6): the Glasshouse
//! meetings a person is in, and joining one.
//!
//! Only people take these, in Meet and Calendar: policy makes each one
//! Never for an agent, whatever its rules allow. Joining gets the
//! session's media key from the console, or makes one when there's none,
//! sealed to the member keys of the meeting's people and never to IT's
//! compliance key. The key goes back to the page, which holds it in memory
//! only. Nothing here writes it down or puts it in a result's text, a
//! preview or an error, so it reaches no audit record and no log: the
//! audit log records who joined and left a meeting, and when.
//!
//! A meeting is a chat conversation of kind `meeting`, kept in chat's
//! store; this crate reaches it through [`Meetings`], not through Chat
//! (ADR 0013).

use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Preview, TaskContext, parse_input,
};
use glass_api::{AskLabels, display_line};
use glass_managed::DirectoryHandle;
use glass_managed::chat::{ChatFailure, LinkError, MemberKey, MemberSecret, is_conversation_id};
use glass_managed::meet::{
    MAX_SDP_BYTES, MediaKey, MediaKeyRecord, MeetBody, MeetLink, MeetOp, MeetRequest, MeetingInfo,
    Meetings,
};
use glass_types::{ActionName, SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};

pub const LIST: &str = "meet.list";
pub const JOIN: &str = "meet.join";
pub const KEYS: &str = "meet.keys";
pub const LEAVE: &str = "meet.leave";

/// The person's own apps for meetings: a call from one of them makes the
/// person's member key if they have none, as opening Chat does.
const PEOPLES_APPS: &[&str] = &["meet", "calendar"];

/// How this computer reaches its console for meetings. Until
/// [`Self::connect`], joining says this computer has no meetings.
#[derive(Clone, Default)]
pub struct MeetHandle(Arc<RwLock<Option<Arc<dyn MeetLink>>>>);

impl fmt::Debug for MeetHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeetHandle")
            .field("connected", &self.link().is_some())
            .finish()
    }
}

impl MeetHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reaches the console's meeting routes through `link`.
    pub fn connect(&self, link: Arc<dyn MeetLink>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Some(link);
    }

    fn link(&self) -> Option<Arc<dyn MeetLink>> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Meetings on this computer: the person's meeting conversations and keys
/// from chat's store, and the console's meeting routes.
#[derive(Debug, Clone)]
pub struct MeetProvider {
    meetings: Arc<dyn Meetings>,
    handle: MeetHandle,
    directory: DirectoryHandle,
}

impl MeetProvider {
    pub fn new(
        meetings: Arc<dyn Meetings>,
        handle: MeetHandle,
        directory: DirectoryHandle,
    ) -> Self {
        Self {
            meetings,
            handle,
            directory,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeetingRef {
    meeting: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinInput {
    meeting: String,
    /// The page's WebRTC offer (SDP).
    offer: String,
}

/// A refusal the page or agent reads: `doing` failed because of `why`.
/// Never given a key: these reach the activity feed and the audit log.
fn failed(doing: &str, why: impl fmt::Display) -> ActionError {
    let why = why.to_string();
    ActionError::Failed(format!("{doing}: {}.", why.trim_end_matches('.')))
}

fn check_meeting(id: &str) -> Result<(), ActionError> {
    if is_conversation_id(id) {
        Ok(())
    } else {
        Err(ActionError::InvalidInput(format!(
            "{id:?} is not a meeting id; meet.list shows yours"
        )))
    }
}

/// What a meeting is called, as one display line.
fn title(meeting: &MeetingInfo) -> String {
    meeting
        .details
        .as_ref()
        .map(|d| display_line(&d.title).into_owned())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "a meeting".into())
}

impl MeetProvider {
    fn me(ctx: &TaskContext) -> &str {
        &ctx.principal.on_behalf_of.name
    }

    fn peoples_app(ctx: &TaskContext) -> bool {
        PEOPLES_APPS.contains(&ctx.principal.agent.as_str())
    }

    /// A person's name from the directory, or their login.
    fn name(&self, login: &str) -> String {
        self.directory
            .person(login)
            .map(|p| p.name)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| login.to_owned())
    }

    /// Meeting `id`, if the person is in it. Reads this computer's store
    /// only, so a preview can use it.
    fn find(&self, me: &str, id: &str) -> Option<MeetingInfo> {
        self.meetings.meetings(me).into_iter().find(|m| m.id == id)
    }

    fn in_it(&self, me: &str, id: &str) -> Result<MeetingInfo, ActionError> {
        self.find(me, id).ok_or_else(|| {
            ActionError::NotFound(format!(
                "you aren't in a meeting {id}; meet.list shows yours"
            ))
        })
    }

    fn link(&self, doing: &str) -> Result<Arc<dyn MeetLink>, ActionError> {
        self.handle.link().ok_or_else(|| {
            failed(
                doing,
                "this computer can't reach meetings: it isn't managed by an organization, \
                 or its organization's server doesn't offer them yet",
            )
        })
    }

    /// The person's member key: made for them in their own app, never for
    /// an agent.
    fn member(
        &self,
        ctx: &TaskContext,
        doing: &str,
    ) -> Result<(MemberKey, MemberSecret), ActionError> {
        let me = Self::me(ctx);
        match self.meetings.member(me, Self::peoples_app(ctx)) {
            Ok(Some(member)) => Ok(member),
            Ok(None) => Err(failed(
                doing,
                format!("{me} hasn't opened Calendar or Chat on this computer yet"),
            )),
            Err(e) => Err(failed(doing, e)),
        }
    }

    fn request(
        key: &MemberKey,
        secret: &MemberSecret,
        meeting: &str,
        op: MeetOp,
        doing: &str,
    ) -> Result<MeetRequest, ActionError> {
        let body = MeetBody {
            meeting: meeting.to_owned(),
            op,
            device: key.device.clone(),
            login: key.login.clone(),
            key: key.id.clone(),
            at: Timestamp::now().to_string(),
        };
        MeetRequest::sign(body, secret).map_err(|e| failed(doing, e))
    }

    /// A new media key for `meeting`'s session, sealed to its people's
    /// member keys and to nothing else (ADR 0030).
    fn make_media(
        &self,
        meeting: &MeetingInfo,
        key: &MemberKey,
        secret: &MemberSecret,
        doing: &str,
    ) -> Result<MediaKeyRecord, ActionError> {
        let mut keys: Vec<MemberKey> = self
            .meetings
            .lookup(&meeting.members)
            .map_err(|e| failed(doing, e))?
            .into_iter()
            .filter(|k| meeting.members.contains(&k.login))
            .collect();
        if !keys.iter().any(|k| k.id == key.id) {
            keys.push(key.clone());
        }
        let media = MediaKey::generate().map_err(|e| failed(doing, e))?;
        MediaKeyRecord::make(&meeting.id, &media, &keys, (key, secret))
            .map_err(|e| failed(doing, e))
    }

    /// The media keys in `records` this computer can trust and open:
    /// signed by a member key of someone in the meeting, sealed to nobody
    /// outside it, and to this person's key.
    fn open_media(
        meeting: &MeetingInfo,
        records: &[MediaKeyRecord],
        makers: &[MemberKey],
        key: &MemberKey,
        secret: &MemberSecret,
        doing: &str,
    ) -> Result<Vec<(String, MediaKey)>, ActionError> {
        let mut opened = Vec::new();
        let mut problem = "the meeting has no media key yet".to_owned();
        for record in records {
            let maker = makers.iter().find(|k| {
                k.id == record.body.made_by
                    && meeting.members.contains(&k.login)
                    && k.keys().is_ok()
            });
            let checked = if record.body.meeting != meeting.id {
                Err("the meeting's media key is for another meeting".to_owned())
            } else if let Some(maker) = maker {
                record.verify(maker).map_err(|e| e.to_string())
            } else {
                Err(
                    "the meeting's media key isn't signed by anyone in the meeting, so it wasn't used"
                        .to_owned(),
                )
            };
            // Keys aren't given to computers that came later until slice M2.
            match checked.and_then(|()| {
                record.open(&key.id, &secret.agree).map_err(|_| {
                    "the meeting started before this computer's key was registered, so its \
                     media key isn't sealed to it; join once the meeting starts again"
                        .to_owned()
                })
            }) {
                Ok(media) => opened.push((record.body.id.clone(), media)),
                Err(why) => problem = why,
            }
        }
        if opened.is_empty() {
            return Err(failed(doing, problem));
        }
        Ok(opened)
    }

    fn list(&self, ctx: &TaskContext) -> Result<ActionOutput, ActionError> {
        let me = Self::me(ctx);
        let unavailable = match self.meetings.member(me, Self::peoples_app(ctx)) {
            Ok(Some(_)) => None,
            Ok(None) => Some(format!(
                "{me} hasn't opened Calendar or Chat on this computer yet"
            )),
            // Registering a first key needs the console; a list queues nothing.
            Err(ChatFailure::Link(LinkError::Unreachable(_))) => {
                Some("your organization's server can't be reached".into())
            }
            Err(e) => Some(e.to_string()),
        };
        if let Some(why) = unavailable {
            // Not an error: Calendar shows the week either way.
            let why = why.trim_end_matches('.').to_owned();
            return Ok(ActionOutput {
                text: format!("No meetings: {why}."),
                data: json!({ "meetings": [], "available": false, "note": why }),
                changes: Vec::new(),
            });
        }
        let mut meetings = self.meetings.meetings(me);
        let start = |m: &MeetingInfo| m.details.as_ref().map(|d| d.start.clone());
        meetings.sort_by(|a, b| start(a).cmp(&start(b)).then_with(|| a.id.cmp(&b.id)));
        let mut text = format!(
            "{} meeting{}.",
            meetings.len(),
            if meetings.len() == 1 { "" } else { "s" }
        );
        let views: Vec<Value> = meetings
            .iter()
            .map(|m| {
                let title = title(m);
                let (start, end) = m
                    .details
                    .as_ref()
                    .map(|d| (d.start.clone(), d.end.clone()))
                    .unwrap_or_default();
                text.push_str(&format!("\n- {start}–{end} {title} [{}]", m.id));
                json!({
                    "id": m.id,
                    "title": title,
                    "start": start,
                    "end": end,
                    "organizer": m.organizer,
                    "organizer_name": self.name(&m.organizer),
                    "people": m.members.iter().map(|p| json!({ "login": p, "name": self.name(p) })).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(ActionOutput {
            text,
            data: json!({ "meetings": views, "available": true }),
            changes: Vec::new(),
        })
    }

    fn join(&self, ctx: &TaskContext, input: JoinInput) -> Result<ActionOutput, ActionError> {
        const DOING: &str = "Not joined";
        let me = Self::me(ctx);
        let link = self.link(DOING)?;
        let (key, secret) = self.member(ctx, DOING)?;
        let meeting = self.in_it(me, &input.meeting)?;
        let title = title(&meeting);
        // The session's key, or a new one if nobody has joined: the
        // console keeps the first it gets and answers everyone with it.
        let current = link
            .keys(&Self::request(
                &key,
                &secret,
                &meeting.id,
                MeetOp::Keys,
                DOING,
            )?)
            .map_err(|e| failed(DOING, ChatFailure::Link(e)))?;
        let media = if current.media.is_empty() {
            Some(self.make_media(&meeting, &key, &secret, DOING)?)
        } else {
            None
        };
        let op = MeetOp::Join {
            offer: input.offer,
            media,
        };
        let joined = link
            .join(&Self::request(&key, &secret, &meeting.id, op, DOING)?)
            .map_err(|e| failed(DOING, ChatFailure::Link(e)))?;
        let opened = if joined.answer.trim().is_empty() || joined.answer.len() > MAX_SDP_BYTES {
            Err(failed(DOING, "the relay's answer is malformed"))
        } else {
            Self::open_media(&meeting, &joined.media, &joined.keys, &key, &secret, DOING)
        };
        let (media_id, media) = match opened {
            Ok(mut opened) => opened.remove(0),
            Err(e) => {
                // Admitted without a key it can use: don't hold a seat.
                if let Ok(leave) = Self::request(&key, &secret, &meeting.id, MeetOp::Leave, DOING) {
                    let _ = link.leave(&leave);
                }
                return Err(e);
            }
        };
        Ok(ActionOutput {
            text: format!("Joined {title} as participant {}.", joined.participant),
            data: json!({
                "meeting": meeting.id,
                "title": title,
                "answer": joined.answer,
                "participant": joined.participant,
                "media_key": media.to_base64(),
                "media_key_id": media_id,
            }),
            changes: Vec::new(),
        })
    }

    fn keys(&self, ctx: &TaskContext, input: MeetingRef) -> Result<ActionOutput, ActionError> {
        const DOING: &str = "No keys";
        let me = Self::me(ctx);
        let link = self.link(DOING)?;
        let (key, secret) = self.member(ctx, DOING)?;
        let meeting = self.in_it(me, &input.meeting)?;
        let title = title(&meeting);
        let answer = link
            .keys(&Self::request(
                &key,
                &secret,
                &meeting.id,
                MeetOp::Keys,
                DOING,
            )?)
            .map_err(|e| failed(DOING, ChatFailure::Link(e)))?;
        let opened = if answer.media.is_empty() {
            Vec::new()
        } else {
            Self::open_media(&meeting, &answer.media, &answer.keys, &key, &secret, DOING)?
        };
        let text = match opened.len() {
            0 => format!("Nobody is in {title} yet, so it has no media key."),
            1 => format!("1 media key for {title}."),
            n => format!("{n} media keys for {title}."),
        };
        Ok(ActionOutput {
            text,
            data: json!({
                "meeting": meeting.id,
                "media_keys": opened
                    .iter()
                    .map(|(id, media)| json!({ "id": id, "key": media.to_base64() }))
                    .collect::<Vec<_>>(),
            }),
            changes: Vec::new(),
        })
    }

    fn leave(&self, ctx: &TaskContext, input: MeetingRef) -> Result<ActionOutput, ActionError> {
        const DOING: &str = "Not left";
        let me = Self::me(ctx);
        let link = self.link(DOING)?;
        let (key, secret) = self.member(ctx, DOING)?;
        let meeting = self.in_it(me, &input.meeting)?;
        link.leave(&Self::request(
            &key,
            &secret,
            &meeting.id,
            MeetOp::Leave,
            DOING,
        )?)
        .map_err(|e| failed(DOING, ChatFailure::Link(e)))?;
        Ok(ActionOutput {
            text: format!("Left {}.", title(&meeting)),
            data: json!({ "meeting": meeting.id, "left": true }),
            changes: Vec::new(),
        })
    }

    /// The meeting a preview names: its title when the person is in it.
    /// Never refuses on membership, so an agent's call reads as policy's
    /// refusal, and invoke checks.
    fn preview_meeting(&self, ctx: &TaskContext, id: &str) -> Result<String, ActionError> {
        check_meeting(id)?;
        Ok(self
            .find(Self::me(ctx), id)
            .map_or_else(|| "a meeting".into(), |m| title(&m)))
    }
}

impl ActionProvider for MeetProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let meeting = json!({
            "type": "string",
            "pattern": "^cv-[0-9a-f]{32}$",
            "description": "A meeting's id from meet.list"
        });
        let one_meeting = json!({
            "type": "object",
            "properties": { "meeting": meeting },
            "required": ["meeting"],
            "additionalProperties": false
        });
        vec![
            ActionDef {
                name: LIST.parse().expect("static name"),
                title: "See your meetings".into(),
                description: "The Glasshouse meetings the person is in: title, start, end, \
                              organizer and people, in start order."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: JOIN.parse().expect("static name"),
                title: "Join meetings".into(),
                description: "Join a meeting with the page's WebRTC offer. Returns the relay's \
                              answer, the participant's index and the session's media key, \
                              which the page keeps in memory only."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "meeting": meeting,
                        "offer": { "type": "string", "minLength": 1, "maxLength": MAX_SDP_BYTES }
                    },
                    "required": ["meeting", "offer"],
                    "additionalProperties": false
                }),
                // The person's camera and microphone leave the machine.
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review meeting", "Approve and join"),
            },
            ActionDef {
                name: KEYS.parse().expect("static name"),
                title: "Get meetings' media keys".into(),
                description: "The media keys of a meeting the person is in: the current one, \
                              and the one before it while one replaces another."
                    .into(),
                input_schema: one_meeting.clone(),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: LEAVE.parse().expect("static name"),
                title: "Leave meetings".into(),
                description: "Leave a meeting the person joined.".into(),
                input_schema: one_meeting,
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review meeting", "Approve and leave"),
            },
        ]
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        let (summary, title) = match action.as_str() {
            LIST => {
                let _: NoInput = parse_input(input)?;
                return Ok(Preview {
                    summary: "See your meetings".into(),
                    targets: vec!["Meetings".into()],
                    ..Preview::default()
                });
            }
            JOIN => {
                let input: JoinInput = parse_input(input)?;
                check_offer(&input.offer)?;
                let title = self.preview_meeting(ctx, &input.meeting)?;
                (format!("Join {title}"), title)
            }
            KEYS => {
                let input: MeetingRef = parse_input(input)?;
                let title = self.preview_meeting(ctx, &input.meeting)?;
                (format!("Get the media keys for {title}"), title)
            }
            LEAVE => {
                let input: MeetingRef = parse_input(input)?;
                let title = self.preview_meeting(ctx, &input.meeting)?;
                (format!("Leave {title}"), title)
            }
            other => {
                return Err(ActionError::InvalidInput(format!(
                    "meet has no action {other}"
                )));
            }
        };
        Ok(Preview {
            summary,
            targets: vec![title],
            ..Preview::default()
        })
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        match action.as_str() {
            LIST => {
                let _: NoInput = parse_input(input)?;
                self.list(ctx)
            }
            JOIN => {
                let input: JoinInput = parse_input(input)?;
                check_meeting(&input.meeting)?;
                check_offer(&input.offer)?;
                self.join(ctx, input)
            }
            KEYS => {
                let input: MeetingRef = parse_input(input)?;
                check_meeting(&input.meeting)?;
                self.keys(ctx, input)
            }
            LEAVE => {
                let input: MeetingRef = parse_input(input)?;
                check_meeting(&input.meeting)?;
                self.leave(ctx, input)
            }
            other => Err(ActionError::InvalidInput(format!(
                "meet has no action {other}"
            ))),
        }
    }
}

fn check_offer(offer: &str) -> Result<(), ActionError> {
    if offer.trim().is_empty() || offer.len() > MAX_SDP_BYTES {
        return Err(ActionError::InvalidInput(format!(
            "an offer is a WebRTC session description of 1 to {MAX_SDP_BYTES} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
