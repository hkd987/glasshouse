//! The `calendar` app's actions (NV-3), backed by the first-party calendar
//! store in the user's home: one JSON file per event in `~/Calendar/Events`.
//!
//! Reading is quiet. Holds and events on the user's own calendar are local
//! and undoable (snapshots restore the event files), so creating and
//! changing them defaults to Notify; deleting one asks first, as every
//! delete does (PRD approval tiers). Invitations leave the machine and
//! can't be recalled, so `calendar.invite` defaults to Ask. Until the
//! calendar service lands, "sent" invitations are written to
//! `~/Calendar/Sent`.
//!
//! "Own" events are the ones the user organizes; invitations others sent
//! them are read-only here. Every event an action writes carries "by
//! [agent] for [user]" (AU-10), in the file and in its extended attribute.
//!
//! Times are wall-clock minutes in the user's time zone,
//! `YYYY-MM-DDTHH:MM`; their fixed width makes text order time order.

use std::path::{Path, PathBuf};

use glass_api::{AskLabels, ChangeKind};
use glass_managed::DirectoryHandle;
use glass_platform::fs::ConfinedDir;
use glass_types::{ActionName, SideEffect, Tier, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, attribute,
    parse_input,
};

const CALENDAR_DIR: &str = "Calendar";
const EVENTS: &str = "Events";
const SENT: &str = "Sent";
const MAX_EVENT_BYTES: u64 = 64 * 1024;
/// Most event files one listing reads.
const MAX_EVENTS: usize = 5000;
/// Longest range one listing covers.
const MAX_RANGE_DAYS: i64 = 62;
/// Longest single event.
const MAX_EVENT_DAYS: i64 = 14;
const MAX_INVITEES: usize = 50;
const MAX_LINE: usize = 200;
const MAX_NOTES: usize = 4000;

/// The calendar, inviting as the person the org's directory says they are.
#[derive(Debug, Clone, Default)]
pub struct CalendarProvider {
    directory: DirectoryHandle,
}

impl CalendarProvider {
    /// A calendar whose invitations name the person's name and address in
    /// `directory` as the organizer (AD-1), or their login without one.
    pub fn new(directory: DirectoryHandle) -> Self {
        Self { directory }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Event,
    /// Time set aside, tentatively: "Hold for Northwind prep".
    Hold,
}

/// One event, as the store keeps it and actions return it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// The file's name without `.json`; the file name wins over this field.
    #[serde(default)]
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub kind: EventKind,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub attendees: Vec<String>,
    /// Whose event it is: the user's login name for their own, the
    /// sender's address for an invitation they received.
    pub organizer: String,
    /// "by [agent] for [user]" for whoever last created or changed it
    /// (AU-10). Empty for events the user made outside Glasshouse.
    #[serde(default)]
    pub attribution: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Range {
    from: String,
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventRef {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewHold {
    title: String,
    start: String,
    end: String,
    #[serde(default)]
    notes: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewEvent {
    title: String,
    start: String,
    end: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    notes: String,
    /// Listed on the event; nobody is told until `calendar.invite`.
    #[serde(default)]
    attendees: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Changes {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
    id: String,
    to: Vec<String>,
}

fn time_schema(what: &str) -> Value {
    json!({
        "type": "string",
        "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}$",
        "description": format!("{what}, local time, like 2026-10-20T14:30")
    })
}

fn id_schema() -> Value {
    json!({ "type": "string", "pattern": "^[a-z0-9][a-z0-9-]{0,63}$" })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

impl CalendarProvider {
    fn calendar_dir(ctx: &TaskContext) -> PathBuf {
        ctx.home.join(CALENDAR_DIR)
    }

    fn events_dir(ctx: &TaskContext) -> PathBuf {
        Self::calendar_dir(ctx).join(EVENTS)
    }

    fn event_path(ctx: &TaskContext, id: &str) -> PathBuf {
        Self::events_dir(ctx).join(format!("{id}.json"))
    }

    fn open(ctx: &TaskContext) -> Result<ConfinedDir, ActionError> {
        ConfinedDir::open(&Self::calendar_dir(ctx))
            .map_err(|e| ActionError::io("calendar store", e))
    }

    fn check_id(id: &str) -> Result<(), ActionError> {
        let ok = (1..=64).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !id.starts_with('-');
        if ok {
            Ok(())
        } else {
            Err(ActionError::InvalidInput(format!(
                "{id:?} is not an event id"
            )))
        }
    }

    fn load(cal: &ConfinedDir, id: &str) -> Result<Event, ActionError> {
        Self::check_id(id)?;
        let rel = Path::new(EVENTS).join(format!("{id}.json"));
        let raw = cal
            .read(&rel, MAX_EVENT_BYTES)
            .map_err(|e| ActionError::io(format!("event {id}"), e))?;
        let mut event: Event = serde_json::from_slice(&raw)
            .map_err(|e| ActionError::Failed(format!("event {id} is damaged: {e}")))?;
        event.id = id.to_owned();
        Ok(event)
    }

    /// Every readable event, in start order. Files that aren't events are
    /// skipped: the store is the user's folder, not ours alone.
    fn load_all(cal: &ConfinedDir) -> Result<Vec<Event>, ActionError> {
        let entries = match cal.list(Path::new(EVENTS)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ActionError::io("calendar store", e)),
        };
        let mut events = Vec::new();
        for entry in entries.iter().filter(|e| !e.is_dir).take(MAX_EVENTS) {
            let Some(id) = entry.name.strip_suffix(".json") else {
                continue;
            };
            if Self::check_id(id).is_err() {
                continue;
            }
            let Ok(event) = Self::load(cal, id) else {
                continue;
            };
            let valid = Minute::parse("start", &event.start)
                .and_then(|start| Minute::parse("end", &event.end).map(|end| start < end))
                .unwrap_or(false);
            if valid {
                events.push(event);
            }
        }
        events.sort_by(|a, b| a.start.cmp(&b.start).then(a.id.cmp(&b.id)));
        Ok(events)
    }

    /// Refuses events the user doesn't organize: others' invitations are
    /// theirs to change.
    fn own<'a>(ctx: &TaskContext, event: &'a Event) -> Result<&'a Event, ActionError> {
        let user = &ctx.principal.on_behalf_of.name;
        if &event.organizer == user {
            Ok(event)
        } else {
            Err(ActionError::OutOfScope(format!(
                "\"{}\" is {}'s event, not one on {user}'s own calendar; only its organizer can change it",
                event.title, event.organizer
            )))
        }
    }

    fn line(field: &str, text: &str, required: bool) -> Result<String, ActionError> {
        let text = text.trim();
        if required && text.is_empty() {
            return Err(ActionError::InvalidInput(format!("{field} can't be empty")));
        }
        if text.chars().count() > MAX_LINE || text.chars().any(char::is_control) {
            return Err(ActionError::InvalidInput(format!(
                "{field} must be one line of at most {MAX_LINE} characters"
            )));
        }
        Ok(text.to_owned())
    }

    fn notes(text: &str) -> Result<String, ActionError> {
        if text.chars().count() > MAX_NOTES {
            return Err(ActionError::InvalidInput(format!(
                "notes are limited to {MAX_NOTES} characters"
            )));
        }
        Ok(text.to_owned())
    }

    fn emails(addresses: &[String]) -> Result<Vec<String>, ActionError> {
        if addresses.len() > MAX_INVITEES {
            return Err(ActionError::InvalidInput(format!(
                "at most {MAX_INVITEES} people per event"
            )));
        }
        for addr in addresses {
            let ok = addr.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty()
                    && domain.contains('.')
                    && !addr.chars().any(|c| {
                        c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';')
                    })
            });
            if !ok {
                return Err(ActionError::InvalidInput(format!(
                    "{addr:?} is not an email address"
                )));
            }
        }
        Ok(addresses.to_vec())
    }

    /// Checks a start and end: real minutes, in order, at most two weeks
    /// apart.
    fn span(start: &str, end: &str) -> Result<(Minute, Minute), ActionError> {
        let (start, end) = (Minute::parse("start", start)?, Minute::parse("end", end)?);
        if end <= start {
            return Err(ActionError::InvalidInput(
                "an event must end after it starts".into(),
            ));
        }
        if end.minutes() - start.minutes() > MAX_EVENT_DAYS * 24 * 60 {
            return Err(ActionError::InvalidInput(format!(
                "an event can last at most {MAX_EVENT_DAYS} days"
            )));
        }
        Ok((start, end))
    }

    fn describe(event: &Event) -> String {
        match (
            Minute::parse("start", &event.start),
            Minute::parse("end", &event.end),
        ) {
            (Ok(start), Ok(end)) => format!("{}: {}", when(start, end), event.title),
            _ => event.title.clone(),
        }
    }

    fn new_event(ctx: &TaskContext, input: &Value, kind: EventKind) -> Result<Event, ActionError> {
        let (title, start, end, location, notes, attendees) = match kind {
            EventKind::Hold => {
                let hold: NewHold = parse_input(input)?;
                (
                    hold.title,
                    hold.start,
                    hold.end,
                    String::new(),
                    hold.notes,
                    Vec::new(),
                )
            }
            EventKind::Event => {
                let event: NewEvent = parse_input(input)?;
                (
                    event.title,
                    event.start,
                    event.end,
                    event.location,
                    event.notes,
                    event.attendees,
                )
            }
        };
        let (start, end) = Self::span(&start, &end)?;
        Ok(Event {
            id: String::new(),
            title: Self::line("title", &title, true)?,
            start: start.text(),
            end: end.text(),
            kind,
            location: Self::line("location", &location, false)?,
            notes: Self::notes(&notes)?,
            attendees: Self::emails(&attendees)?,
            organizer: ctx.principal.on_behalf_of.name.clone(),
            attribution: ctx.principal.attribution(),
        })
    }

    /// The event after `changes`, checked as a new one would be.
    fn changed(
        ctx: &TaskContext,
        mut event: Event,
        changes: Changes,
    ) -> Result<Event, ActionError> {
        Self::own(ctx, &event)?;
        if let Some(title) = changes.title {
            event.title = Self::line("title", &title, true)?;
        }
        if let Some(location) = changes.location {
            event.location = Self::line("location", &location, false)?;
        }
        if let Some(notes) = changes.notes {
            event.notes = Self::notes(&notes)?;
        }
        let start = changes.start.unwrap_or(event.start.clone());
        let end = changes.end.unwrap_or(event.end.clone());
        let (start, end) = Self::span(&start, &end)?;
        event.start = start.text();
        event.end = end.text();
        event.attribution = ctx.principal.attribution();
        Ok(event)
    }

    fn save(
        ctx: &TaskContext,
        cal: &ConfinedDir,
        event: &Event,
        new: bool,
    ) -> std::io::Result<PathBuf> {
        let text = serde_json::to_vec_pretty(event).map_err(std::io::Error::other)?;
        let rel = Path::new(EVENTS).join(format!("{}.json", event.id));
        if new {
            cal.write_new(&rel, &text, Some(ctx.owner))?;
        } else {
            cal.write(&rel, &text, Some(ctx.owner))?;
        }
        let path = Self::event_path(ctx, &event.id);
        attribute(ctx, &path);
        Ok(path)
    }

    /// The invitation as it would go out, in iCalendar form.
    fn invitation(&self, ctx: &TaskContext, event: &Event, to: &[String]) -> String {
        let stamp = |text: &str| text.replace(['-', ':'], "") + "00";
        let organizer = self.directory.sender(&ctx.principal.on_behalf_of.name);
        let mut ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Glasshouse//Calendar//EN\r\nMETHOD:REQUEST\r\n\
             BEGIN:VEVENT\r\nUID:{id}@glasshouse\r\nDTSTAMP:{now}\r\nDTSTART:{start}\r\nDTEND:{end}\r\n\
             SUMMARY:{title}\r\nORGANIZER;CN=\"{name}\":mailto:{address}\r\n",
            id = event.id,
            now = Timestamp::now(),
            start = stamp(&event.start),
            end = stamp(&event.end),
            title = ics_text(&event.title),
            // A quoted parameter can't hold a quote.
            name = organizer.name.replace('"', "'"),
            address = organizer.address(),
        );
        if !event.location.is_empty() {
            ics.push_str(&format!("LOCATION:{}\r\n", ics_text(&event.location)));
        }
        for addr in to {
            ics.push_str(&format!("ATTENDEE;RSVP=TRUE:mailto:{addr}\r\n"));
        }
        ics.push_str(&format!(
            "X-GLASSHOUSE-ATTRIBUTION:{}\r\nX-GLASSHOUSE-TASK:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            ctx.principal.attribution(),
            ctx.principal.task
        ));
        ics
    }

    fn new_id() -> String {
        let id = uuid::Uuid::now_v7().simple().to_string();
        format!("ev-{}", &id[id.len() - 12..])
    }
}

/// Escapes text for an iCalendar property value (RFC 5545 3.3.11).
fn ics_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

impl ActionProvider for CalendarProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let new_event = |hold: bool| {
            let mut properties = json!({
                "title": { "type": "string", "minLength": 1, "maxLength": MAX_LINE },
                "start": time_schema("When it starts"),
                "end": time_schema("When it ends"),
                "notes": { "type": "string", "maxLength": MAX_NOTES }
            });
            if !hold {
                properties["location"] = json!({ "type": "string", "maxLength": MAX_LINE });
                properties["attendees"] = json!({
                    "type": "array", "items": { "type": "string" }, "maxItems": MAX_INVITEES,
                    "description": "Listed on the event. Nobody is told until calendar.invite."
                });
            }
            object(properties, &["title", "start", "end"])
        };
        vec![
            ActionDef {
                name: "calendar.list".parse().expect("static name"),
                title: "Read the calendar".into(),
                description: format!(
                    "List the user's events and holds that overlap a time range (at most \
                     {MAX_RANGE_DAYS} days), in start order. Times are local, like 2026-10-20T14:30; \
                     a bare date like 2026-10-20 means its first minute."
                ),
                input_schema: object(
                    json!({
                        "from": { "type": "string", "description": "Start of the range (a date or a time)" },
                        "to": { "type": "string", "description": "End of the range, exclusive (a date or a time)" }
                    }),
                    &["from", "to"],
                ),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: "calendar.read".parse().expect("static name"),
                title: "Read calendar events".into(),
                description: "Read one event: time, place, notes, attendees and organizer.".into(),
                input_schema: object(json!({ "id": id_schema() }), &["id"]),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: "calendar.hold".parse().expect("static name"),
                title: "Create calendar holds".into(),
                description: "Set time aside on the user's own calendar, tentatively. Nobody else \
                              is told. Undoable."
                    .into(),
                input_schema: new_event(true),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Notify,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: "calendar.create".parse().expect("static name"),
                title: "Add calendar events".into(),
                description: "Add an event to the user's own calendar. Attendees are listed but \
                              not invited; send invitations with calendar.invite. Undoable."
                    .into(),
                input_schema: new_event(false),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Notify,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: "calendar.update".parse().expect("static name"),
                title: "Change calendar events".into(),
                description: "Change the title, time, place or notes of an event the user \
                              organizes. Others' events can't be changed. Undoable."
                    .into(),
                input_schema: object(
                    json!({
                        "id": id_schema(),
                        "title": { "type": "string", "minLength": 1, "maxLength": MAX_LINE },
                        "start": time_schema("New start"),
                        "end": time_schema("New end"),
                        "location": { "type": "string", "maxLength": MAX_LINE },
                        "notes": { "type": "string", "maxLength": MAX_NOTES }
                    }),
                    &["id"],
                ),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Notify,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: "calendar.delete".parse().expect("static name"),
                title: "Delete calendar events".into(),
                description: "Delete an event the user organizes. Undoable; nobody is told.".into(),
                input_schema: object(json!({ "id": id_schema() }), &["id"]),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review event", "Approve and delete"),
            },
            ActionDef {
                name: "calendar.invite".parse().expect("static name"),
                title: "Send calendar invitations".into(),
                description: format!(
                    "Invite people to an event the user organizes (up to {MAX_INVITEES}). \
                     Invitations leave the machine and can't be recalled."
                ),
                input_schema: object(
                    json!({
                        "id": id_schema(),
                        "to": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": MAX_INVITEES }
                    }),
                    &["id", "to"],
                ),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review invitations", "Approve and send"),
            },
        ]
    }

    fn prepare(&self, ctx: &TaskContext) -> Result<(), ActionError> {
        let home = ConfinedDir::open(&ctx.home).map_err(|e| ActionError::io("home folder", e))?;
        for folder in [EVENTS, SENT] {
            let rel = Path::new(CALENDAR_DIR).join(folder);
            if !home.exists(&rel) {
                home.create_dir_all_as(&rel, ctx.owner)
                    .map_err(|e| ActionError::io("calendar folders", e))?;
            }
        }
        Ok(())
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        let events = Self::events_dir(ctx);
        let change = |summary: String, event: &Event| Preview {
            summary,
            targets: vec![event.title.clone()],
            items: vec![Self::describe(event)],
            reads: Vec::new(),
            writes: vec![events.clone()],
            snapshot_roots: vec![events.clone()],
        };
        match action.as_str() {
            "calendar.list" => {
                let Range { from, to } = parse_input(input)?;
                let (from, to) = range(&from, &to)?;
                Ok(Preview {
                    summary: format!("Read the calendar, {}", span_text(from, to)),
                    targets: Vec::new(),
                    items: Vec::new(),
                    reads: vec![events.clone()],
                    writes: Vec::new(),
                    snapshot_roots: Vec::new(),
                })
            }
            "calendar.read" => {
                let EventRef { id } = parse_input(input)?;
                Self::check_id(&id)?;
                Ok(Preview {
                    summary: format!("Read calendar event {id}"),
                    targets: vec![id.clone()],
                    items: Vec::new(),
                    reads: vec![Self::event_path(ctx, &id)],
                    writes: Vec::new(),
                    snapshot_roots: Vec::new(),
                })
            }
            "calendar.hold" => {
                let event = Self::new_event(ctx, input, EventKind::Hold)?;
                Ok(change(format!("Hold {}", Self::describe(&event)), &event))
            }
            "calendar.create" => {
                let event = Self::new_event(ctx, input, EventKind::Event)?;
                Ok(change(
                    format!("Add event {}", Self::describe(&event)),
                    &event,
                ))
            }
            "calendar.update" => {
                let changes: Changes = parse_input(input)?;
                let event = Self::load(&Self::open(ctx)?, &changes.id)?;
                let event = Self::changed(ctx, event, changes)?;
                Ok(change(
                    format!("Change event {}", Self::describe(&event)),
                    &event,
                ))
            }
            "calendar.delete" => {
                let EventRef { id } = parse_input(input)?;
                let event = Self::load(&Self::open(ctx)?, &id)?;
                Self::own(ctx, &event)?;
                Ok(change(
                    format!("Delete event {}", Self::describe(&event)),
                    &event,
                ))
            }
            "calendar.invite" => {
                let Invite { id, to } = parse_input(input)?;
                if to.is_empty() {
                    return Err(ActionError::InvalidInput(
                        "invite at least one person".into(),
                    ));
                }
                let to = Self::emails(&to)?;
                let event = Self::load(&Self::open(ctx)?, &id)?;
                Self::own(ctx, &event)?;
                let n = to.len();
                Ok(Preview {
                    summary: format!(
                        "Send {n} invitation{}: {}",
                        if n == 1 { "" } else { "s" },
                        event.title
                    ),
                    targets: to.clone(),
                    items: to
                        .iter()
                        .map(|addr| format!("To {addr}: {}", Self::describe(&event)))
                        .collect(),
                    reads: vec![Self::event_path(ctx, &id)],
                    writes: vec![Self::calendar_dir(ctx).join(SENT)],
                    snapshot_roots: Vec::new(),
                })
            }
            _ => Err(ActionError::InvalidInput(format!(
                "calendar has no action {action}"
            ))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        let cal = Self::open(ctx)?;
        match action.as_str() {
            "calendar.list" => {
                let Range { from, to } = parse_input(input)?;
                let (from, to) = range(&from, &to)?;
                let (from_text, to_text) = (from.text(), to.text());
                let events: Vec<Event> = Self::load_all(&cal)?
                    .into_iter()
                    .filter(|e| e.start < to_text && e.end > from_text)
                    .collect();
                let n = events.len();
                Ok(ActionOutput {
                    text: format!(
                        "{n} event{} {}",
                        if n == 1 { "" } else { "s" },
                        span_text(from, to)
                    ),
                    data: json!({ "from": from_text, "to": to_text, "events": events }),
                    changes: Vec::new(),
                })
            }
            "calendar.read" => {
                let EventRef { id } = parse_input(input)?;
                let event = Self::load(&cal, &id)?;
                Ok(ActionOutput {
                    text: Self::describe(&event),
                    data: json!({ "event": event }),
                    changes: Vec::new(),
                })
            }
            "calendar.hold" | "calendar.create" => {
                let kind = if action.as_str() == "calendar.hold" {
                    EventKind::Hold
                } else {
                    EventKind::Event
                };
                let mut event = Self::new_event(ctx, input, kind)?;
                // A fresh name; a taken one (two writers at once) gets another.
                let mut saved = None;
                for _ in 0..5 {
                    event.id = Self::new_id();
                    match Self::save(ctx, &cal, &event, true) {
                        Ok(path) => {
                            saved = Some(path);
                            break;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(ActionError::io("saving event", e)),
                    }
                }
                let path = saved.ok_or_else(|| ActionError::Failed("no free event id".into()))?;
                let line = Self::describe(&event);
                Ok(ActionOutput {
                    text: format!(
                        "{} {line}",
                        if kind == EventKind::Hold {
                            "Held"
                        } else {
                            "Added"
                        }
                    ),
                    data: json!({ "event": event }),
                    changes: vec![Change {
                        kind: ChangeKind::Created,
                        target: format!("Calendar: {line}"),
                        reversible: true,
                        paths: vec![path],
                    }],
                })
            }
            "calendar.update" => {
                let changes: Changes = parse_input(input)?;
                let event = Self::load(&cal, &changes.id)?;
                let event = Self::changed(ctx, event, changes)?;
                let path = Self::save(ctx, &cal, &event, false)
                    .map_err(|e| ActionError::io("saving event", e))?;
                let line = Self::describe(&event);
                Ok(ActionOutput {
                    text: format!("Changed {line}"),
                    data: json!({ "event": event }),
                    changes: vec![Change {
                        kind: ChangeKind::Modified,
                        target: format!("Calendar: {line}"),
                        reversible: true,
                        paths: vec![path],
                    }],
                })
            }
            "calendar.delete" => {
                let EventRef { id } = parse_input(input)?;
                let event = Self::load(&cal, &id)?;
                Self::own(ctx, &event)?;
                cal.remove_file(&Path::new(EVENTS).join(format!("{id}.json")))
                    .map_err(|e| ActionError::io("deleting event", e))?;
                let line = Self::describe(&event);
                Ok(ActionOutput {
                    text: format!("Deleted {line}"),
                    data: json!({ "deleted": id }),
                    changes: vec![Change {
                        kind: ChangeKind::Deleted,
                        target: format!("Calendar: {line}"),
                        reversible: true,
                        paths: vec![Self::event_path(ctx, &id)],
                    }],
                })
            }
            "calendar.invite" => {
                let Invite { id, to } = parse_input(input)?;
                if to.is_empty() {
                    return Err(ActionError::InvalidInput(
                        "invite at least one person".into(),
                    ));
                }
                let to = Self::emails(&to)?;
                let event = Self::load(&cal, &id)?;
                Self::own(ctx, &event)?;
                let text = self.invitation(ctx, &event, &to);
                let stamp = Timestamp::now().unix_seconds();
                let mut sent = None;
                for attempt in 1..=100 {
                    let name = if attempt == 1 {
                        format!("{stamp}-{id}.ics")
                    } else {
                        format!("{stamp}-{id}-{attempt}.ics")
                    };
                    let rel = Path::new(SENT).join(name);
                    match cal.write_new(&rel, text.as_bytes(), Some(ctx.owner)) {
                        Ok(()) => {
                            sent = Some(Self::calendar_dir(ctx).join(rel));
                            break;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(ActionError::io("sending invitations", e)),
                    }
                }
                let path = sent.ok_or_else(|| ActionError::Failed("no free file name".into()))?;
                attribute(ctx, &path);
                let line = Self::describe(&event);
                Ok(ActionOutput {
                    text: format!(
                        "Sent {} invitation{} to {line}",
                        to.len(),
                        if to.len() == 1 { "" } else { "s" }
                    ),
                    data: json!({ "invited": to, "event": event.id }),
                    changes: to
                        .iter()
                        .map(|addr| Change {
                            kind: ChangeKind::Sent,
                            target: format!("Invitation to {addr}: {line}"),
                            reversible: false,
                            paths: vec![path.clone()],
                        })
                        .collect(),
                })
            }
            _ => Err(ActionError::InvalidInput(format!(
                "calendar has no action {action}"
            ))),
        }
    }
}

/// A listing's range: real bounds, in order, at most [`MAX_RANGE_DAYS`].
fn range(from: &str, to: &str) -> Result<(Minute, Minute), ActionError> {
    let (from, to) = (Minute::bound("from", from)?, Minute::bound("to", to)?);
    if to <= from {
        return Err(ActionError::InvalidInput(
            "the range must end after it starts".into(),
        ));
    }
    if to.minutes() - from.minutes() > MAX_RANGE_DAYS * 24 * 60 {
        return Err(ActionError::InvalidInput(format!(
            "list at most {MAX_RANGE_DAYS} days at a time"
        )));
    }
    Ok((from, to))
}

/// A wall-clock minute, `YYYY-MM-DDTHH:MM`, in the user's time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Minute {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

impl Minute {
    fn parse(field: &str, text: &str) -> Result<Self, ActionError> {
        let bad = || {
            ActionError::InvalidInput(format!(
                "{field} {text:?} is not a time like 2026-10-20T14:30"
            ))
        };
        let bytes = text.as_bytes();
        if bytes.len() != 16
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b':'
        {
            return Err(bad());
        }
        let number = |from: usize, to: usize| -> Result<i64, ActionError> {
            let digits = &text[from..to];
            if digits.bytes().all(|b| b.is_ascii_digit()) {
                digits.parse().map_err(|_| bad())
            } else {
                Err(bad())
            }
        };
        let minute = Self {
            year: number(0, 4)?,
            month: number(5, 7)?,
            day: number(8, 10)?,
            hour: number(11, 13)?,
            minute: number(14, 16)?,
        };
        let real = (2000..=2100).contains(&minute.year)
            && (1..=12).contains(&minute.month)
            && (1..=days_in_month(minute.year, minute.month)).contains(&minute.day)
            && minute.hour < 24
            && minute.minute < 60;
        if real { Ok(minute) } else { Err(bad()) }
    }

    /// A range bound: a minute, or a date for its first minute.
    fn bound(field: &str, text: &str) -> Result<Self, ActionError> {
        if text.len() == 10 {
            Self::parse(field, &format!("{text}T00:00"))
        } else {
            Self::parse(field, text)
        }
    }

    /// Days since 1970-01-01 (H. Hinnant's days_from_civil).
    fn days(&self) -> i64 {
        let year = if self.month <= 2 {
            self.year - 1
        } else {
            self.year
        };
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let day_of_year = (153 * ((self.month + 9) % 12) + 2) / 5 + self.day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    fn minutes(&self) -> i64 {
        self.days() * 24 * 60 + self.hour * 60 + self.minute
    }

    fn text(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute
        )
    }

    /// "Tue Oct 20"
    fn date(&self) -> String {
        let weekday = WEEKDAYS[(self.days() + 4).rem_euclid(7) as usize];
        let month = MONTHS[(self.month - 1) as usize];
        format!("{weekday} {month} {}", self.day)
    }

    /// "14:30"
    fn clock(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// "Tue Oct 20, 14:00–15:00", or both dates when it spans days.
fn when(start: Minute, end: Minute) -> String {
    if start.days() == end.days() {
        format!("{}, {}–{}", start.date(), start.clock(), end.clock())
    } else {
        format!(
            "{} {} – {} {}",
            start.date(),
            start.clock(),
            end.date(),
            end.clock()
        )
    }
}

/// "from Mon Oct 19 to Sun Oct 25" for whole days, else with the times.
fn span_text(from: Minute, to: Minute) -> String {
    let midnight = |m: Minute| m.hour == 0 && m.minute == 0;
    if midnight(from) && midnight(to) {
        match to.days() - from.days() {
            1 => format!("on {}", from.date()),
            days => format!(
                "from {} to {}",
                from.date(),
                add_days(from, days - 1).date()
            ),
        }
    } else {
        format!(
            "from {} {} to {} {}",
            from.date(),
            from.clock(),
            to.date(),
            to.clock()
        )
    }
}

/// `start` moved forward by `days` whole days (the time stays).
fn add_days(start: Minute, days: i64) -> Minute {
    let mut m = start;
    for _ in 0..days {
        m.day += 1;
        if m.day > days_in_month(m.year, m.month) {
            m.day = 1;
            m.month += 1;
            if m.month > 12 {
                m.month = 1;
                m.year += 1;
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use glass_actions::testing::context;

    fn setup() -> (tempfile::TempDir, TaskContext) {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context(dir.path());
        let meta = fs::metadata(dir.path()).unwrap();
        ctx.owner = glass_platform::fs::Owner {
            uid: meta.uid(),
            gid: meta.gid(),
        };
        CalendarProvider::default().prepare(&ctx).unwrap();
        (dir, ctx)
    }

    fn name(action: &str) -> ActionName {
        action.parse().unwrap()
    }

    /// An invitation someone else sent, as the calendar service would file it.
    fn seed_others(dir: &Path) {
        let event = json!({
            "title": "Northwind renewal", "start": "2026-10-21T10:00", "end": "2026-10-21T11:00",
            "kind": "event", "organizer": "dana@northwind.example", "attendees": ["demo@corp.example"]
        });
        fs::write(
            dir.join("Calendar/Events/ev-northwind.json"),
            serde_json::to_vec(&event).unwrap(),
        )
        .unwrap();
    }

    fn hold(ctx: &TaskContext) -> Event {
        let out = CalendarProvider::default()
            .invoke(
                ctx,
                &name("calendar.hold"),
                &json!({ "title": "Prep for Northwind", "start": "2026-10-20T14:00", "end": "2026-10-20T15:00" }),
            )
            .unwrap();
        serde_json::from_value(out.data["event"].clone()).unwrap()
    }

    #[test]
    fn nv3_holds_notify_are_undoable_and_attributed() {
        let (dir, ctx) = setup();
        let defs = CalendarProvider::default().actions();
        let def = defs
            .iter()
            .find(|d| d.name.as_str() == "calendar.hold")
            .unwrap();
        assert_eq!(
            def.default_tier,
            Tier::Notify,
            "PRD: create calendar holds is Notify"
        );
        assert_eq!(def.side_effect, SideEffect::LocalReversible);

        let input = json!({ "title": "Prep for Northwind", "start": "2026-10-20T14:00", "end": "2026-10-20T15:00" });
        let preview = CalendarProvider::default()
            .preview(&ctx, &name("calendar.hold"), &input)
            .unwrap();
        assert_eq!(
            preview.summary,
            "Hold Tue Oct 20, 14:00–15:00: Prep for Northwind"
        );
        assert_eq!(
            preview.snapshot_roots,
            vec![dir.path().join("Calendar/Events")]
        );

        let out = CalendarProvider::default()
            .invoke(&ctx, &name("calendar.hold"), &input)
            .unwrap();
        let change = &out.changes[0];
        assert!(change.reversible);
        assert_eq!(
            change.target,
            "Calendar: Tue Oct 20, 14:00–15:00: Prep for Northwind"
        );
        // Preview and invoke agree on what is written.
        assert!(change.paths[0].starts_with(&preview.writes[0]));
        let saved: Event = serde_json::from_slice(&fs::read(&change.paths[0]).unwrap()).unwrap();
        assert_eq!(saved.kind, EventKind::Hold);
        assert_eq!(saved.organizer, "demo");
        // AU-10: "by [agent] for [user]" travels with the event.
        assert_eq!(saved.attribution, "by invoice-demo for demo");
    }

    #[test]
    fn nv3_listing_returns_overlapping_events_in_order() {
        let (dir, ctx) = setup();
        seed_others(dir.path());
        hold(&ctx);
        CalendarProvider::default()
            .invoke(
                &ctx,
                &name("calendar.create"),
                &json!({ "title": "Standup", "start": "2026-10-19T09:00", "end": "2026-10-19T09:15", "attendees": ["sam@corp.example"] }),
            )
            .unwrap();
        CalendarProvider::default()
            .invoke(
                &ctx,
                &name("calendar.create"),
                &json!({ "title": "Next week", "start": "2026-10-27T09:00", "end": "2026-10-27T10:00" }),
            )
            .unwrap();
        // A file that isn't an event is skipped, not fatal.
        fs::write(dir.path().join("Calendar/Events/notes.txt"), "x").unwrap();

        let input = json!({ "from": "2026-10-19", "to": "2026-10-26" });
        let preview = CalendarProvider::default()
            .preview(&ctx, &name("calendar.list"), &input)
            .unwrap();
        assert_eq!(
            preview.summary,
            "Read the calendar, from Mon Oct 19 to Sun Oct 25"
        );
        assert!(preview.writes.is_empty() && preview.snapshot_roots.is_empty());
        let out = CalendarProvider::default()
            .invoke(&ctx, &name("calendar.list"), &input)
            .unwrap();
        let titles: Vec<&str> = out.data["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["title"].as_str().unwrap())
            .collect();
        assert_eq!(
            titles,
            ["Standup", "Prep for Northwind", "Northwind renewal"]
        );
        assert_eq!(out.text, "3 events from Mon Oct 19 to Sun Oct 25");
        assert!(out.changes.is_empty());
    }

    #[test]
    fn nv3_updates_and_deletes_touch_only_the_users_own_events() {
        let (dir, ctx) = setup();
        seed_others(dir.path());
        let mine = hold(&ctx);

        let moved =
            json!({ "id": mine.id, "start": "2026-10-20T16:00", "end": "2026-10-20T17:00" });
        let out = CalendarProvider::default()
            .invoke(&ctx, &name("calendar.update"), &moved)
            .unwrap();
        assert_eq!(out.changes[0].kind, ChangeKind::Modified);
        assert_eq!(out.data["event"]["start"], "2026-10-20T16:00");

        for (action, input) in [
            (
                "calendar.update",
                json!({ "id": "ev-northwind", "title": "Mine now" }),
            ),
            ("calendar.delete", json!({ "id": "ev-northwind" })),
            (
                "calendar.invite",
                json!({ "id": "ev-northwind", "to": ["x@corp.example"] }),
            ),
        ] {
            assert!(
                matches!(
                    CalendarProvider::default().preview(&ctx, &name(action), &input),
                    Err(ActionError::OutOfScope(_))
                ),
                "{action} on someone else's event"
            );
            assert!(matches!(
                CalendarProvider::default().invoke(&ctx, &name(action), &input),
                Err(ActionError::OutOfScope(_))
            ));
        }
        assert!(
            dir.path()
                .join("Calendar/Events/ev-northwind.json")
                .exists()
        );

        let delete = json!({ "id": mine.id });
        let preview = CalendarProvider::default()
            .preview(&ctx, &name("calendar.delete"), &delete)
            .unwrap();
        assert_eq!(
            preview.snapshot_roots,
            vec![dir.path().join("Calendar/Events")]
        );
        let out = CalendarProvider::default()
            .invoke(&ctx, &name("calendar.delete"), &delete)
            .unwrap();
        assert!(out.changes[0].reversible);
        assert!(!out.changes[0].paths[0].exists());
    }

    #[test]
    fn nv3_invitations_leave_the_machine_and_ask_first() {
        let (dir, ctx) = setup();
        let defs = CalendarProvider::default().actions();
        let def = defs
            .iter()
            .find(|d| d.name.as_str() == "calendar.invite")
            .unwrap();
        assert_eq!(def.default_tier, Tier::Ask);
        assert_eq!(def.side_effect, SideEffect::Irreversible);

        let mine = hold(&ctx);
        let input = json!({ "id": mine.id, "to": ["sam@corp.example", "dana@northwind.example"] });
        let preview = CalendarProvider::default()
            .preview(&ctx, &name("calendar.invite"), &input)
            .unwrap();
        // One approval with a review list (DA-12).
        assert_eq!(preview.summary, "Send 2 invitations: Prep for Northwind");
        assert_eq!(preview.items.len(), 2);
        assert!(
            preview.snapshot_roots.is_empty(),
            "invitations can't be undone"
        );

        let out = CalendarProvider::default()
            .invoke(&ctx, &name("calendar.invite"), &input)
            .unwrap();
        assert!(
            out.changes
                .iter()
                .all(|c| !c.reversible && c.kind == ChangeKind::Sent)
        );
        let sent: Vec<_> = fs::read_dir(dir.path().join("Calendar/Sent"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(sent.len(), 1);
        let ics = fs::read_to_string(sent[0].path()).unwrap();
        assert!(ics.contains("ATTENDEE;RSVP=TRUE:mailto:dana@northwind.example\r\n"));
        assert!(ics.contains("X-GLASSHOUSE-ATTRIBUTION:by invoice-demo for demo\r\n"));
    }

    #[test]
    fn rejects_bad_times_ids_text_and_unknown_fields() {
        let (_dir, ctx) = setup();
        let hold_with = |patch: Value| {
            let mut input =
                json!({ "title": "x", "start": "2026-10-20T14:00", "end": "2026-10-20T15:00" });
            for (k, v) in patch.as_object().unwrap() {
                input[k] = v.clone();
            }
            CalendarProvider::default().preview(&ctx, &name("calendar.hold"), &input)
        };
        for patch in [
            json!({ "start": "2026-13-01T10:00" }),
            json!({ "start": "2026-02-30T10:00" }),
            json!({ "start": "2026-10-20 14:00" }),
            json!({ "end": "2026-10-20T13:00" }),
            json!({ "end": "2026-11-20T13:00" }),
            json!({ "title": "Line one\nBcc: everyone" }),
            json!({ "title": "  " }),
            json!({ "location": "Room 1" }),
        ] {
            assert!(
                matches!(hold_with(patch.clone()), Err(ActionError::InvalidInput(_))),
                "{patch} should be refused"
            );
        }
        for id in ["../../etc/passwd", "Ev-1", "-x", ""] {
            assert!(matches!(
                CalendarProvider::default().preview(
                    &ctx,
                    &name("calendar.read"),
                    &json!({ "id": id })
                ),
                Err(ActionError::InvalidInput(_))
            ));
        }
        assert!(
            CalendarProvider::default()
                .preview(
                    &ctx,
                    &name("calendar.list"),
                    &json!({ "from": "2026-01-01", "to": "2026-06-01" })
                )
                .is_err(),
            "ranges are bounded"
        );
        assert!(matches!(
            CalendarProvider::default().preview(
                &ctx,
                &name("calendar.invite"),
                &json!({ "id": "ev-x", "to": ["not an address"] })
            ),
            Err(ActionError::InvalidInput(_))
        ));
    }

    #[test]
    fn minutes_know_their_weekday_and_order() {
        let monday = Minute::parse("start", "2026-10-19T09:00").unwrap();
        assert_eq!(monday.date(), "Mon Oct 19");
        assert_eq!(
            Minute::parse("x", "2028-02-29T00:00").unwrap().date(),
            "Tue Feb 29"
        );
        let later = Minute::parse("end", "2026-10-20T08:00").unwrap();
        assert!(monday < later);
        assert_eq!(later.minutes() - monday.minutes(), 23 * 60);
        assert_eq!(when(monday, later), "Mon Oct 19 09:00 – Tue Oct 20 08:00");
        assert_eq!(add_days(monday, 13).date(), "Sun Nov 1");
    }
}
