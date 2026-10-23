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
//!
//! Versions are kept root only and never change ([`store`]); which ones
//! the person has is a file in their home.

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

pub use schedule::{Schedule, local_minute};
pub use store::{Routine, Routines};
/// A schedule's days, as [`Schedule::due`] takes them.
pub use time::Weekday;

pub const LIST: &str = "routines.list";
pub const READ: &str = "routines.read";
pub const SAVE: &str = "routines.save";
pub const DELETE: &str = "routines.delete";

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
                 script fails: glassctl routine run {} starts it now.",
                saved.name, saved.version, saved.id
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
        None => format!(
            "Runs when you start it (glassctl routine run {})",
            routine.id
        ),
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
                let text = match mine.as_slice() {
                    [] => "No routines yet.".to_owned(),
                    all => all
                        .iter()
                        .map(|r| {
                            let runs = r
                                .schedule
                                .map_or_else(|| "when started".to_owned(), |s| s.describe());
                            format!("{} ({}, version {}): {runs}", r.name, r.id, r.version)
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                Ok(ActionOutput {
                    text,
                    data: json!({ "routines": mine.iter().map(summary_json).collect::<Vec<_>>() }),
                    changes: Vec::new(),
                })
            }
            READ => {
                let Named { routine } = parse_input(input)?;
                let found = self.mine(ctx, &routine)?;
                let mut data = summary_json(&found);
                data["script"] = json!(found.script);
                data["note"] = json!(found.note);
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
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }
}

#[cfg(test)]
mod tests;
