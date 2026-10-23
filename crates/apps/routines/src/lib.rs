//! Routines (DA-4, NV-21, ADR 0044): a task saved by name with the scope
//! it ran with and a script that does it with no model, run when the
//! person asks or on a schedule.
//!
//! The agent writes the script against the task's own tools, tries it
//! with its script runner, and saves it here; the person approves what
//! will run, seeing the script and every tool it calls. Each run is a task
//! like any other, in the same jail, under the same tiers, audit and undo:
//! the agent runs the script, and if it fails, does the request itself.
//!
//! - `routines.list`, `routines.read` (Auto): the person's routines; one,
//!   with its script.
//! - `routines.save` (Ask): a new routine, or a new version of one.
//! - `routines.delete` (Notify): takes one off their list; undo puts it
//!   back.
//! - `routines.pause` (Notify), `routines.schedule` (Ask): pauses a
//!   routine's schedule or resumes it; changes when it runs, as its next
//!   version (ADR 0047).
//! - `routines.run`, `routines.repeats`, `routines.keep`,
//!   `routines.dismiss`: the person's own, in the Routines app, never an
//!   agent's (ADR 0047): starting one now, and the requests they did the
//!   same way more than once, kept as routines or not offered again.
//!
//! Versions are kept root only and never change ([`store`]); which ones
//! the person has is a file in their home.

pub mod host;
pub mod schedule;
pub mod store;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, parse_input,
};
use glass_api::ChangeKind;
use glass_types::{SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};

pub use host::{Host, Repeat};
pub use schedule::{Schedule, local_minute};
pub use store::{Outcome, Routine, Routines, Run};
/// A schedule's days, as [`Schedule::due`] takes them.
pub use time::Weekday;

pub const LIST: &str = "routines.list";
pub const READ: &str = "routines.read";
pub const SAVE: &str = "routines.save";
pub const DELETE: &str = "routines.delete";
pub const PAUSE: &str = "routines.pause";
pub const SCHEDULE: &str = "routines.schedule";
pub const RUN: &str = "routines.run";
pub const REPEATS: &str = "routines.repeats";
pub const KEEP: &str = "routines.keep";
pub const DISMISS: &str = "routines.dismiss";

/// The longest script kept: a routine is a short program.
pub const MAX_SCRIPT_BYTES: usize = 32 * 1024;
const MAX_NAME_CHARS: usize = 60;
const MAX_REQUEST_CHARS: usize = 2_000;
const MAX_NOTE_CHARS: usize = 300;
/// The script lines an approval shows before saying how many more.
const SHOWN_LINES: usize = 40;

fn invalid(why: impl Into<String>) -> ActionError {
    ActionError::InvalidInput(why.into())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveInput {
    /// One of theirs: the save is its next version.
    #[serde(default)]
    routine: Option<String>,
    #[serde(default)]
    name: Option<String>,
    request: String,
    #[serde(default)]
    script: Option<String>,
    /// The runtime writes the script from this task's calls (ADR 0046).
    #[serde(default)]
    from_this_task: bool,
    #[serde(default)]
    schedule: Option<String>,
    note: String,
}

/// A routine, or its next version, as someone asks for it.
#[derive(Debug, Clone)]
pub struct Draft {
    /// One of theirs: this is its next version.
    pub routine: Option<String>,
    /// Its name; the routine's own for a next version.
    pub name: Option<String>,
    pub request: String,
    pub script: String,
    pub schedule: Option<String>,
    pub note: String,
}

/// Who runs a routine and with what: the task it's saved from.
#[derive(Debug, Clone)]
pub struct Scope {
    pub agent: glass_types::AgentId,
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub task: glass_types::TaskId,
}

impl Routines {
    /// `draft` as `login`'s routine, checked, with `scope`: what keeping it
    /// will keep, its version numbered then.
    pub fn plan(
        &self,
        login: &str,
        home: &Path,
        draft: Draft,
        scope: Scope,
    ) -> Result<Routine, String> {
        let current = match &draft.routine {
            Some(id) => Some(
                store::chosen(home)
                    .get(id)
                    .and_then(|v| self.record(login, id, *v))
                    .ok_or_else(|| format!("{id} isn't a routine of yours"))?,
            ),
            None => None,
        };
        let name = match (draft.name, &current) {
            (Some(name), _) => name.trim().to_owned(),
            (None, Some(current)) => current.name.clone(),
            (None, None) => return Err("name the routine".into()),
        };
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS || name.contains('\n') {
            return Err(format!(
                "name it in a line of at most {MAX_NAME_CHARS} characters"
            ));
        }
        let request = draft.request.trim().to_owned();
        if request.is_empty() || request.chars().count() > MAX_REQUEST_CHARS {
            return Err(format!(
                "say what it does, in at most {MAX_REQUEST_CHARS} characters: each run starts with it"
            ));
        }
        if draft.script.trim().is_empty() || draft.script.len() > MAX_SCRIPT_BYTES {
            return Err(format!(
                "the script is the routine: at most {} KB of JavaScript",
                MAX_SCRIPT_BYTES / 1024
            ));
        }
        let note = draft.note.trim().to_owned();
        if note.is_empty() || note.chars().count() > MAX_NOTE_CHARS {
            return Err("say in a sentence what this version is".into());
        }
        let schedule = match draft.schedule.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => Some(text.parse::<Schedule>()?),
        };
        let id = match current {
            Some(current) => current.id,
            None => new_id(&name, &self.ids(login))?,
        };
        Ok(Routine {
            id,
            name,
            version: 0,
            owner: login.to_owned(),
            agent: scope.agent,
            request,
            calls: calls(&draft.script),
            script: draft.script,
            schedule,
            read: scope.read,
            write: scope.write,
            note,
            saved_at: Timestamp::now(),
            saved_in: scope.task,
        })
    }

    /// Keeps `routine` as its next version and makes it the one the person
    /// has, writing their list as them.
    pub fn keep(
        &self,
        routine: Routine,
        home: &Path,
        owner: glass_platform::fs::Owner,
    ) -> std::io::Result<Routine> {
        let saved = self.save(routine)?;
        let mut chosen = store::chosen(home);
        chosen.insert(saved.id.clone(), saved.version);
        store::choose(home, owner, chosen)?;
        Ok(saved)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    routine: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseInput {
    routine: String,
    paused: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleInput {
    routine: String,
    /// None: it runs only when started.
    schedule: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeepInput {
    task: glass_types::TaskId,
    name: String,
    #[serde(default)]
    schedule: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskInput {
    task: glass_types::TaskId,
}

/// The actions over [`Routines`].
#[derive(Debug, Clone)]
pub struct RoutinesProvider {
    routines: Routines,
}

impl RoutinesProvider {
    pub fn new(routines: Routines) -> Self {
        Self { routines }
    }

    /// What a save will keep, worked out from its input and the task alone.
    fn plan(&self, ctx: &TaskContext, input: SaveInput) -> Result<Routine, ActionError> {
        if input.from_this_task {
            // The runtime writes the script from the task's calls and takes
            // the flag off; reaching here, it couldn't.
            return Err(invalid(
                "this computer couldn't write the script from this task",
            ));
        }
        let script = input
            .script
            .ok_or_else(|| invalid("send the script, or from_this_task: true for this computer to write it from what this task did"))?;
        // The scope of the task that saves it (DA-4): what each run may
        // read and change. Never from the input.
        let (read, write) = ctx.grants.iter().partition::<Vec<_>, _>(|g| !g.writable);
        self.routines
            .plan(
                &ctx.principal.on_behalf_of.name,
                &ctx.home,
                Draft {
                    routine: input.routine,
                    name: input.name,
                    request: input.request,
                    script,
                    schedule: input.schedule,
                    note: input.note,
                },
                Scope {
                    agent: ctx.principal.agent.clone(),
                    read: read.iter().map(|g| g.root.clone()).collect(),
                    write: write.iter().map(|g| g.root.clone()).collect(),
                    task: ctx.principal.task,
                },
            )
            .map_err(invalid)
    }

    fn save(&self, ctx: &TaskContext, routine: Routine) -> Result<ActionOutput, ActionError> {
        let saved = self
            .routines
            .keep(routine, &ctx.home, ctx.owner)
            .map_err(|e| ActionError::io("the routine", e))?;
        let when = match &saved.schedule {
            Some(schedule) => format!("It runs {}", schedule.describe()),
            None => "It runs when you start it".to_owned(),
        };
        Ok(ActionOutput {
            text: format!(
                "Saved the routine {} (version {}). {when}, with no model unless its \
                 script fails. It's in Routines, where you can run it now.",
                saved.name, saved.version
            ),
            data: summary_json(&saved),
            changes: vec![Change {
                kind: if saved.version == 1 {
                    ChangeKind::Created
                } else {
                    ChangeKind::Modified
                },
                target: format!("Routine: {} version {}", saved.name, saved.version),
                reversible: true,
                paths: vec![ctx.home.join(store::MINE)],
            }],
        })
    }

    fn host(&self) -> Result<std::sync::Arc<dyn Host>, ActionError> {
        self.routines.host().ok_or_else(|| {
            ActionError::Failed("routines can't be started from here on this computer".into())
        })
    }

    /// `found`'s next version, running on `schedule` (none: when started),
    /// with the same script, request and scope.
    fn rescheduled(
        &self,
        ctx: &TaskContext,
        found: &Routine,
        schedule: Option<String>,
    ) -> Result<Routine, ActionError> {
        let parsed = match schedule.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => Some(text.parse::<Schedule>().map_err(invalid)?),
        };
        if parsed == found.schedule {
            return Err(invalid(format!(
                "{} already runs {}",
                found.name,
                runs_text(found.schedule)
            )));
        }
        self.routines
            .plan(
                &found.owner,
                &ctx.home,
                Draft {
                    routine: Some(found.id.clone()),
                    name: None,
                    request: found.request.clone(),
                    script: found.script.clone(),
                    schedule: parsed.map(|s| s.to_string()),
                    note: format!("Runs {} now", runs_text(parsed)),
                },
                Scope {
                    agent: found.agent.clone(),
                    read: found.read.clone(),
                    write: found.write.clone(),
                    task: ctx.principal.task,
                },
            )
            .map_err(invalid)
    }

    /// The repeat `task` did, kept as the routine `name` (ADR 0046, 0047).
    fn kept(&self, ctx: &TaskContext, input: KeepInput) -> Result<Routine, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let repeat = self
            .host()?
            .repeats(login, ctx.owner.uid)
            .into_iter()
            .find(|r| r.task == input.task)
            .ok_or_else(|| {
                invalid(format!(
                    "task {} isn't a request you did the same way twice; routines.repeats lists them",
                    input.task.short()
                ))
            })?;
        self.routines
            .plan(
                login,
                &ctx.home,
                Draft {
                    routine: None,
                    name: Some(input.name),
                    request: repeat.request,
                    script: repeat.script,
                    schedule: input.schedule,
                    note: format!(
                        "Written by this computer from task {}, done the same way {} times",
                        repeat.task.short(),
                        repeat.times
                    ),
                },
                Scope {
                    agent: repeat.agent,
                    read: repeat.read,
                    write: repeat.write,
                    task: repeat.task,
                },
            )
            .map_err(invalid)
    }

    /// A routine as the Routines app lists it: with whether its schedule is
    /// paused, when it next runs and how it last ran.
    fn listed(&self, routine: &Routine, paused: bool, now: Now) -> Value {
        let mut data = summary_json(routine);
        let runs = self.routines.runs(&routine.owner, &routine.id);
        data["paused"] = json!(paused);
        data["next_run_at"] = json!(if paused {
            None
        } else {
            now.next(routine.schedule)
        });
        data["last_run"] = json!(runs.first());
        data["note"] = json!(routine.note);
        data["saved_at"] = json!(routine.saved_at);
        data
    }

    fn mine(&self, ctx: &TaskContext, id: &str) -> Result<Routine, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        store::chosen(&ctx.home)
            .get(id)
            .and_then(|v| self.routines.record(login, id, *v))
            .ok_or_else(|| {
                invalid(format!(
                    "{id} isn't a routine of yours; routines.list lists them"
                ))
            })
    }
}

/// A name as an id: lowercase letters and digits joined by `-`, 40 at
/// most, the next free one.
fn new_id(name: &str, taken: &[String]) -> Result<String, String> {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let mut stem = String::new();
    for word in words {
        if stem.len() + word.len() + 1 > 40 {
            break;
        }
        if !stem.is_empty() {
            stem.push('-');
        }
        stem.push_str(&word);
    }
    if stem.is_empty() {
        stem = "routine".into();
    }
    (1..100)
        .map(|n| match n {
            1 => stem.clone(),
            n => format!("{stem}-{n}"),
        })
        .find(|id| !taken.contains(id))
        .ok_or_else(|| "name it something more its own".to_owned())
}

/// The tools a script calls, by name, in the order it first calls them:
/// `call("files_list", …)`.
pub fn calls(script: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for (at, _) in script.match_indices("call(") {
        // Not part of another name, such as `recall(`.
        if script[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            continue;
        }
        let rest = script[at + 5..].trim_start();
        let Some(quote) = rest
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\'' | '`'))
        else {
            continue;
        };
        let name: String = rest[1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && rest[1 + name.len()..].starts_with(quote) && !found.contains(&name) {
            found.push(name);
        }
    }
    found
}

/// When a routine runs, in words: "every weekday at 09:00", or "when you
/// start it".
fn runs_text(schedule: Option<Schedule>) -> String {
    schedule.map_or_else(|| "when you start it".to_owned(), |s| s.describe())
}

/// This minute, in the computer's local time, for when routines next run.
#[derive(Debug, Clone, Copy)]
struct Now {
    unix: i64,
    offset: i32,
}

impl Now {
    fn here() -> Self {
        Self {
            unix: Timestamp::now().unix_seconds(),
            offset: glass_platform::system::utc_offset_secs().unwrap_or(0),
        }
    }

    /// When `schedule` next runs; never, without one.
    fn next(self, schedule: Option<Schedule>) -> Option<Timestamp> {
        let (day, hour, minute, _) = local_minute(self.unix, self.offset);
        let ahead = schedule?.minutes_until(day, hour, minute)?;
        Timestamp::from_unix_seconds(self.unix - self.unix.rem_euclid(60) + i64::from(ahead) * 60)
    }
}

fn paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn summary_json(routine: &Routine) -> Value {
    json!({
        "routine": routine.id,
        "name": routine.name,
        "version": routine.version,
        "request": routine.request,
        "schedule": routine.schedule.map(|s| s.to_string()),
        "runs": routine.schedule.map_or_else(|| "when started".to_owned(), |s| s.describe()),
        "calls": routine.calls,
        "read": routine.read,
        "write": routine.write,
    })
}

/// What the approval shows: everything that will run, and with what.
fn review(routine: &Routine) -> Vec<String> {
    let mut items = vec![format!("Does: {}", routine.request)];
    items.push(match &routine.schedule {
        Some(schedule) => format!("Runs {}, in this computer's time", schedule.describe()),
        None => "Runs when you start it, from Routines".to_owned(),
    });
    if !routine.read.is_empty() {
        items.push(format!("Reads: {}", paths(&routine.read)));
    }
    if !routine.write.is_empty() {
        items.push(format!("Changes: {}", paths(&routine.write)));
    }
    items.push(match routine.calls.as_slice() {
        [] => "Calls no tools".to_owned(),
        calls => format!(
            "Calls: {}, each at its tier, as {} would",
            calls.join(", "),
            routine.agent
        ),
    });
    let lines: Vec<&str> = routine.script.lines().collect();
    items.push(format!("The script, {} lines:", lines.len()));
    items.extend(lines.iter().take(SHOWN_LINES).map(|l| format!("  {l}")));
    if lines.len() > SHOWN_LINES {
        items.push(format!("  … and {} more lines", lines.len() - SHOWN_LINES));
    }
    items
}

fn def(
    name: &str,
    title: &str,
    description: &str,
    side_effect: SideEffect,
    tier: Tier,
    schema: Value,
) -> ActionDef {
    ActionDef {
        name: name.parse().expect("static action name"),
        title: title.into(),
        description: description.into(),
        input_schema: schema,
        side_effect,
        default_tier: tier,
        ask: Default::default(),
    }
}

/// A call that changes which routines the person has: their file is
/// snapshotted first, so undo puts it back.
fn choice_preview(ctx: &TaskContext, summary: String, items: Vec<String>) -> Preview {
    let file = ctx.home.join(store::MINE);
    Preview {
        summary,
        targets: vec!["Your routines".into()],
        items,
        texts: Vec::new(),
        reads: Vec::new(),
        snapshot_roots: file.parent().map(Path::to_path_buf).into_iter().collect(),
        writes: vec![file],
        label: None,
    }
}

impl ActionProvider for RoutinesProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let routine = json!({ "type": "string", "description": "A routine's id, as routines.list gives it." });
        vec![
            def(
                LIST,
                "Your routines",
                "The routines this person has: what each does, when it runs and what it calls.",
                SideEffect::None,
                Tier::Auto,
                json!({ "type": "object", "additionalProperties": false }),
            ),
            def(
                READ,
                "A routine",
                "One routine, with its script.",
                SideEffect::None,
                Tier::Auto,
                json!({
                    "type": "object",
                    "properties": { "routine": routine },
                    "required": ["routine"],
                    "additionalProperties": false
                }),
            ),
            def(
                SAVE,
                "Save a routine",
                "Saves a task the person wants done again, by name, with a script that does it \
                 and no model. For actions, do it once with the tools, then save with \
                 from_this_task: this computer writes the script from what you did. For an \
                 answer worked out from what the tools return, write the script yourself: \
                 JavaScript for run_script, whose call(tool, input) makes this task's tool \
                 calls; try it first. Each run reads and changes what this task may, and the \
                 agent does the request itself if the script fails. The person sees the script \
                 before it's saved.",
                SideEffect::LocalReversible,
                Tier::Ask,
                json!({
                    "type": "object",
                    "properties": {
                        "routine": { "type": "string", "description": "One of theirs, to save its next version." },
                        "name": { "type": "string", "description": "A short name, such as \"Overdue invoices\"." },
                        "request": { "type": "string", "description": "What it does, as the person would ask for it: each run starts with it." },
                        "from_this_task": { "type": "boolean", "description": "True: this computer writes the script from the calls this task made, the same way every time, with loops where a call was made for each item of a list. Use it for actions: moving, drafting, filing." },
                        "script": { "type": "string", "description": "Or a script you wrote and tried with run_script, for an answer worked out from what the tools return. Its result, a string, is the run's answer." },
                        "schedule": { "type": "string", "description": format!("When it runs by itself: {}. Leave out to run it only when asked.", schedule::FORMS) },
                        "note": { "type": "string", "description": "What this version is, in a sentence." }
                    },
                    "required": ["request", "note"],
                    "additionalProperties": false
                }),
            )
            .asking("Review the routine", "Save it"),
            def(
                DELETE,
                "Remove a routine",
                "Takes a routine off the person's list, so it no longer runs. Undo puts it back.",
                SideEffect::LocalReversible,
                Tier::Notify,
                json!({
                    "type": "object",
                    "properties": { "routine": routine },
                    "required": ["routine"],
                    "additionalProperties": false
                }),
            ),
            def(
                PAUSE,
                "Pause a routine",
                "Pauses a routine's schedule, or resumes it: paused, it runs only when started. \
                 Undo puts it back.",
                SideEffect::LocalReversible,
                Tier::Notify,
                json!({
                    "type": "object",
                    "properties": { "routine": routine, "paused": { "type": "boolean" } },
                    "required": ["routine", "paused"],
                    "additionalProperties": false
                }),
            ),
            def(
                SCHEDULE,
                "Change when a routine runs",
                "Saves a routine's next version with a new schedule, its script and scope as \
                 they are. The person sees the change before it's saved.",
                SideEffect::LocalReversible,
                Tier::Ask,
                json!({
                    "type": "object",
                    "properties": {
                        "routine": routine,
                        "schedule": {
                            "type": ["string", "null"],
                            "description": format!("When it runs by itself: {}; null to run it only when started.", schedule::FORMS)
                        }
                    },
                    "required": ["routine", "schedule"],
                    "additionalProperties": false
                }),
            )
            .asking("Review the change", "Change it"),
            // The person's own, in Routines (ADR 0047): policy makes them
            // Never for every agent.
            def(
                RUN,
                "Run a routine now",
                "Starts one of the person's routines now, as a task of its own.",
                SideEffect::None,
                Tier::Notify,
                json!({
                    "type": "object",
                    "properties": { "routine": routine },
                    "required": ["routine"],
                    "additionalProperties": false
                }),
            ),
            def(
                REPEATS,
                "Requests done the same way twice",
                "The person's requests done more than once with the same steps, with the code this \
                 computer wrote from them (ADR 0046), not yet kept as routines.",
                SideEffect::None,
                Tier::Auto,
                json!({ "type": "object", "additionalProperties": false }),
            ),
            def(
                KEEP,
                "Keep a repeat as a routine",
                "Keeps a request the person did the same way more than once as a routine, with \
                 the code this computer wrote from it.",
                SideEffect::LocalReversible,
                Tier::Notify,
                json!({
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "The repeat's task, as routines.repeats gives it." },
                        "name": { "type": "string" },
                        "schedule": { "type": "string", "description": format!("When it runs by itself: {}.", schedule::FORMS) }
                    },
                    "required": ["task", "name"],
                    "additionalProperties": false
                }),
            ),
            def(
                DISMISS,
                "Stop offering a repeat",
                "Stops offering a request done the same way twice, until it's done that way \
                 twice again.",
                SideEffect::None,
                Tier::Auto,
                json!({
                    "type": "object",
                    "properties": { "task": { "type": "string" } },
                    "required": ["task"],
                    "additionalProperties": false
                }),
            ),
        ]
    }

    fn prepare(&self, ctx: &TaskContext) -> Result<(), ActionError> {
        // The choices' folder exists before a call changes it, so the
        // snapshot before that call has something to cover.
        let home = glass_platform::fs::ConfinedDir::open(&ctx.home)
            .map_err(|e| ActionError::io("home folder", e))?;
        if let Some(folder) = Path::new(store::MINE).parent()
            && !home.exists(folder)
        {
            home.create_dir_all_as(folder, ctx.owner)
                .map_err(|e| ActionError::io("your routines' folder", e))?;
        }
        Ok(())
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &glass_types::ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        match action.as_str() {
            LIST => {
                let Empty {} = parse_input(input)?;
                Ok(Preview {
                    summary: "List your routines".into(),
                    targets: vec!["Your routines".into()],
                    ..Preview::default()
                })
            }
            READ => {
                let Named { routine } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                Ok(Preview {
                    summary: format!("Read the routine {}", found.name),
                    targets: vec![found.name],
                    ..Preview::default()
                })
            }
            SAVE => {
                let routine = self.plan(ctx, parse_input(input)?)?;
                let summary = match self.routines.versions(&routine.owner, &routine.id).last() {
                    Some(latest) => format!(
                        "Save version {} of the routine {}",
                        latest + 1,
                        routine.name
                    ),
                    None => format!("Save the routine {}", routine.name),
                };
                Ok(choice_preview(ctx, summary, review(&routine)))
            }
            DELETE => {
                let Named { routine } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                Ok(choice_preview(
                    ctx,
                    format!("Remove the routine {}", found.name),
                    Vec::new(),
                ))
            }
            PAUSE => {
                let PauseInput { routine, paused } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                let summary = if paused {
                    format!("Pause the routine {}", found.name)
                } else {
                    format!("Resume the routine {}", found.name)
                };
                Ok(choice_preview(ctx, summary, Vec::new()))
            }
            SCHEDULE => {
                let ScheduleInput { routine, schedule } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                let next = self.rescheduled(ctx, &found, schedule)?;
                Ok(choice_preview(
                    ctx,
                    format!(
                        "Run the routine {} {}",
                        found.name,
                        runs_text(next.schedule)
                    ),
                    vec![
                        format!("Was: runs {}", runs_text(found.schedule)),
                        format!(
                            "Now: runs {}, in this computer's time",
                            runs_text(next.schedule)
                        ),
                    ],
                ))
            }
            RUN => {
                let Named { routine } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                Ok(Preview {
                    summary: format!("Run the routine {} now", found.name),
                    targets: vec![found.name],
                    ..Preview::default()
                })
            }
            REPEATS => {
                let Empty {} = parse_input(input)?;
                Ok(Preview {
                    summary: "List requests done the same way twice".into(),
                    targets: vec!["Your routines".into()],
                    ..Preview::default()
                })
            }
            KEEP => {
                let routine = self.kept(ctx, parse_input(input)?)?;
                Ok(choice_preview(
                    ctx,
                    format!("Keep the routine {}", routine.name),
                    review(&routine),
                ))
            }
            DISMISS => {
                let TaskInput { task } = parse_input(input)?;
                Ok(Preview {
                    summary: format!("Stop offering task {}'s request as a routine", task.short()),
                    targets: vec!["Your routines".into()],
                    ..Preview::default()
                })
            }
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &glass_types::ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        match action.as_str() {
            LIST => {
                let Empty {} = parse_input(input)?;
                let mine = self.routines.current(login, &ctx.home);
                let paused = store::paused(&ctx.home);
                let text = match mine.as_slice() {
                    [] => "No routines yet.".to_owned(),
                    all => all
                        .iter()
                        .map(|r| {
                            let runs = if paused.contains(&r.id) {
                                "paused".to_owned()
                            } else {
                                r.schedule
                                    .map_or_else(|| "when started".to_owned(), |s| s.describe())
                            };
                            format!("{} ({}, version {}): {runs}", r.name, r.id, r.version)
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                let now = Now::here();
                Ok(ActionOutput {
                    text,
                    data: json!({ "routines": mine
                        .iter()
                        .map(|r| self.listed(r, paused.contains(&r.id), now))
                        .collect::<Vec<_>>() }),
                    changes: Vec::new(),
                })
            }
            READ => {
                let Named { routine } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                let paused = store::paused(&ctx.home).contains(&found.id);
                let mut data = self.listed(&found, paused, Now::here());
                data["script"] = json!(found.script);
                data["runs_kept"] = json!(self.routines.runs(login, &found.id));
                data["versions"] = json!(self.routines.versions(login, &found.id).len());
                Ok(ActionOutput {
                    text: format!(
                        "{} (version {}) runs {}: {}",
                        found.name,
                        found.version,
                        found
                            .schedule
                            .map_or_else(|| "when started".to_owned(), |s| s.describe()),
                        found.request
                    ),
                    data,
                    changes: Vec::new(),
                })
            }
            SAVE => {
                let routine = self.plan(ctx, parse_input(input)?)?;
                self.save(ctx, routine)
            }
            DELETE => {
                let Named { routine } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                let mut chosen: BTreeMap<String, u32> = store::chosen(&ctx.home);
                chosen.remove(&found.id);
                store::choose(&ctx.home, ctx.owner, chosen)
                    .map_err(|e| ActionError::io("your routines", e))?;
                Ok(ActionOutput {
                    text: format!(
                        "Removed the routine {}; it won't run again. Undo puts it back.",
                        found.name
                    ),
                    data: json!({ "routine": found.id }),
                    changes: vec![Change {
                        kind: ChangeKind::Deleted,
                        target: format!("Routine: {}", found.name),
                        reversible: true,
                        paths: vec![ctx.home.join(store::MINE)],
                    }],
                })
            }
            PAUSE => {
                let PauseInput { routine, paused } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                store::pause(&ctx.home, ctx.owner, &found.id, paused)
                    .map_err(|e| ActionError::io("your routines", e))?;
                let text = if paused {
                    format!(
                        "Paused {}: it runs only when you start it, until you resume it.",
                        found.name
                    )
                } else {
                    format!(
                        "Resumed {}: it runs {} again.",
                        found.name,
                        runs_text(found.schedule)
                    )
                };
                Ok(ActionOutput {
                    text,
                    data: json!({ "routine": found.id, "paused": paused }),
                    changes: vec![Change {
                        kind: ChangeKind::Modified,
                        target: format!(
                            "Routine: {} {}",
                            found.name,
                            if paused { "paused" } else { "resumed" }
                        ),
                        reversible: true,
                        paths: vec![ctx.home.join(store::MINE)],
                    }],
                })
            }
            SCHEDULE => {
                let ScheduleInput { routine, schedule } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                let next = self.rescheduled(ctx, &found, schedule)?;
                self.save(ctx, next)
            }
            RUN => {
                let Named { routine } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                let task = self
                    .host()?
                    .run(login, ctx.owner.uid, &found.id)
                    .map_err(ActionError::Failed)?;
                Ok(ActionOutput {
                    text: format!(
                        "Started {}: it runs as task {}, on your desktop.",
                        found.name,
                        task.short()
                    ),
                    data: json!({ "routine": found.id, "task": task }),
                    changes: Vec::new(),
                })
            }
            REPEATS => {
                let Empty {} = parse_input(input)?;
                let repeats = self.host()?.repeats(login, ctx.owner.uid);
                let text = match repeats.as_slice() {
                    [] => "Nothing done the same way twice yet.".to_owned(),
                    all => all
                        .iter()
                        .map(|r| format!("{} times: {}", r.times, r.request))
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                Ok(ActionOutput {
                    text,
                    data: json!({ "repeats": repeats }),
                    changes: Vec::new(),
                })
            }
            KEEP => {
                let routine = self.kept(ctx, parse_input(input)?)?;
                self.save(ctx, routine)
            }
            DISMISS => {
                let TaskInput { task } = parse_input(input)?;
                self.host()?
                    .dismiss(login, ctx.owner.uid, task)
                    .map_err(ActionError::Failed)?;
                Ok(ActionOutput {
                    text: "It won't be offered again unless it's done the same way twice more."
                        .into(),
                    data: json!({ "task": task }),
                    changes: Vec::new(),
                })
            }
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }
}

#[cfg(test)]
mod tests;
