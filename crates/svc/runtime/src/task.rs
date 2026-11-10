//! One task's identity (fixed at start) and its mutable record.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, MutexGuard};

use glass_actions::{Grant, TaskContext};
use glass_api::{
    ActivityEntry, ActivityKind, AppManifest, ApprovalView, ChangeEntry, DisplaySafe, FeedLine,
    Progress, Receipt, ScopeRequest, SnapshotMarker, TaskDetail, TaskView, TileState, display_line,
};
use glass_platform::fs::Owner;
use glass_platform::sandbox::SandboxSpec;
use glass_platform::snapshot::SnapshotHandle;
use glass_policy::EffectivePolicy;
use glass_types::{AgentPrincipal, Label, TaskId, Timestamp, UserRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use crate::written::Written;
use crate::{Caller, CatalogEntry};

/// Lines a tile's feed shows (AW-3).
const FEED_LINES: usize = 3;
/// The longest line of a task's activity, in characters: a few lines of a
/// tile's expanded view. Most are a sentence.
pub(crate) const ACTIVITY_LINE: usize = 500;
/// The lines of activity a task keeps: ten times what the expanded view
/// shows (its lists stop at 200).
pub(crate) const ACTIVITY_LINES: usize = 2000;

/// Whether the agent may act right now. Calls wait at the gate, which makes
/// every action boundary a safe point to pause at (AW-20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Run,
    Hold,
    Ended,
}

#[derive(Debug)]
pub(crate) struct Task {
    pub id: TaskId,
    pub user: Caller,
    pub entry: CatalogEntry,
    pub prompt: String,
    /// The scope the user set on the handoff card, kept for Retry.
    pub scope: ScopeRequest,
    pub plan: Vec<String>,
    /// What policy lets this task do. An agent's task keeps the one it
    /// started with (ADR 0021); an app session follows the policy in
    /// force (ADR 0023, amended).
    policy: std::sync::RwLock<Arc<EffectivePolicy>>,
    pub ctx: TaskContext,
    pub sandbox: SandboxSpec,
    pub cant_touch: Vec<String>,
    /// The agent's settings: published defaults, with the user's layer.
    pub settings: BTreeMap<String, Value>,
    /// The layer version this task runs with (TL-2).
    pub layer: Option<u32>,
    /// Where the user's own harness connects, for an attended task.
    pub driver_socket: Option<PathBuf>,
    /// The ended task this one carries on from (`task.continue`).
    pub continues: Option<Continued>,
    /// The routine it runs, if it's one (DA-4).
    pub routine: Option<RoutineRun>,
    /// The schedule that started it, if one did (ADR 0069).
    pub schedule: Option<ScheduleRun>,
    /// For an app session (ADR 0024): the app, as its manifest declares
    /// it. The task's principal is the app, acting for the user.
    pub app: Option<Arc<AppManifest>>,
    pub started_at: Timestamp,
    pub gate: watch::Sender<Gate>,
    /// Fires once to make the supervisor kill the agent.
    pub stop: Mutex<Option<oneshot::Sender<()>>>,
    /// Actions past every check and running now. Sleep waits for these.
    pub in_flight: AtomicUsize,
    /// A write of the task's record is on its way (`Runtime::keep_soon`).
    pub keeping: AtomicBool,
    /// Dismissed or retired: its record is gone and isn't written again.
    pub gone: AtomicBool,
    state: Mutex<TaskState>,
}

#[derive(Debug)]
pub(crate) struct TakenSnapshot {
    pub marker: SnapshotMarker,
    pub handle: SnapshotHandle,
}

#[derive(Debug)]
pub(crate) struct TaskState {
    pub tile: TileState,
    pub caption: String,
    pub progress: Option<Progress>,
    pub feed: VecDeque<FeedLine>,
    pub activity: Vec<ActivityEntry>,
    pub changes: Vec<ChangeEntry>,
    /// What each change touched and how the task left its files (undo).
    pub written: Written,
    pub snapshots: Vec<TakenSnapshot>,
    pub actions: u32,
    pub failure: Option<String>,
    pub updated_at: Timestamp,
    /// Taken at take over, to tell the agent what the user changed (AW-21).
    pub takeover: Option<SnapshotHandle>,
    /// Set while paused for sleep (PM-5): the caption to restore on wake.
    pub sleeping: Option<String>,
    /// For an attended task: the user's harness has connected.
    pub connected: bool,
    /// The task's short name (`TaskView::title`).
    pub title: String,
    /// The title stays as it is: the request gave it, or the agent already
    /// renamed the task.
    pub title_fixed: bool,
    /// The most sensitive label of anything the task has read (DP-11).
    /// Model calls carry it, so it decides where they may go (DP-2).
    pub data_label: Option<Label>,
    /// What the agent told the person when done (`TaskDetail::answer`).
    pub answer: Option<String>,
    /// Where its model calls took what it sent them (`Receipt::models`).
    pub models_sent_to: Vec<String>,
    /// Its tool calls and what they returned, for replay code (ADR 0046).
    pub trace: Trace,
}

/// A task's completed tool calls, in order, with what each returned: what
/// replay code is compiled from (NV-21, ADR 0046). Past its limits it
/// stops, marked full, and can't be replayed. When the task ends it is
/// compiled once and the calls are let go: only the code stays.
#[derive(Debug, Default)]
pub(crate) struct Trace {
    pub calls: Vec<glass_replay::Call>,
    bytes: usize,
    pub full: bool,
    ended: Option<Result<glass_replay::Replay, String>>,
}

/// The most a trace keeps: its calls' inputs and results.
const MAX_TRACE_BYTES: usize = 2 * 1024 * 1024;

impl Trace {
    pub fn push(&mut self, call: glass_replay::Call) {
        let size = call.input.to_string().len() + call.text.len() + call.data.to_string().len();
        if self.full
            || self.calls.len() >= glass_replay::MAX_CALLS
            || self.bytes + size > MAX_TRACE_BYTES
        {
            self.full = true;
            return;
        }
        self.bytes += size;
        self.calls.push(call);
    }

    /// What compiling it takes, copied out so that the compiling happens
    /// with the task's lock let go: a long trace takes a while, and the
    /// task's views wait on that lock.
    pub fn taken(&self) -> Traced {
        if let Some(ended) = &self.ended {
            return Traced::Compiled(ended.clone());
        }
        if self.full {
            return Traced::Compiled(Err(format!(
                "it made more calls, or read more, than a routine replays (at most {} calls)",
                glass_replay::MAX_CALLS
            )));
        }
        Traced::Calls(self.calls.clone())
    }

    /// At the task's end: what its calls compiled to, kept, and the calls
    /// let go.
    pub fn end(&mut self, compiled: Result<glass_replay::Replay, String>) {
        self.ended = Some(compiled);
        self.calls = Vec::new();
        self.bytes = 0;
    }
}

/// A trace as it stood, to compile away from the task's lock.
pub(crate) enum Traced {
    Compiled(Result<glass_replay::Replay, String>),
    Calls(Vec<glass_replay::Call>),
}

impl Traced {
    /// Its calls as replay code, or why they can't be.
    pub fn compile(self) -> Result<glass_replay::Replay, String> {
        match self {
            Self::Compiled(compiled) => compiled,
            Self::Calls(calls) => glass_replay::compile(&calls),
        }
    }
}

/// Everything fixed when a task starts.
#[derive(Debug)]
pub(crate) struct NewTask {
    pub id: TaskId,
    pub user: Caller,
    pub entry: CatalogEntry,
    pub prompt: String,
    pub scope: ScopeRequest,
    pub plan: Vec<String>,
    pub policy: EffectivePolicy,
    pub ctx: TaskContext,
    pub sandbox: SandboxSpec,
    pub cant_touch: Vec<String>,
    pub settings: BTreeMap<String, Value>,
    pub layer: Option<u32>,
    pub driver_socket: Option<PathBuf>,
    pub continues: Option<Continued>,
    pub routine: Option<RoutineRun>,
    pub schedule: Option<ScheduleRun>,
    pub app: Option<Arc<AppManifest>>,
    pub title: String,
    /// The request gave the title, so the agent can't rename the task.
    pub title_fixed: bool,
    pub stop: oneshot::Sender<()>,
}

/// The routine a task runs (DA-4): the agent reads it by id and runs its
/// script first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RoutineRun {
    pub id: String,
    pub version: u32,
    /// Started by glassd on its schedule, not by the person.
    pub scheduled: bool,
}

/// The schedule a task's run is of (ADR 0069): whose, and which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ScheduleRun {
    pub owner: String,
    pub id: String,
}

/// What a continued task tells the harness about the task it carries on
/// from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Continued {
    pub id: TaskId,
    pub agent: String,
    /// How it ended, as a clause: "failed: …", "was stopped".
    pub ended: String,
    /// The steps it completed, from its activity log.
    pub done: Vec<String>,
    /// What it changed, with whether each can be undone.
    pub changes: Vec<String>,
}

impl Task {
    /// Whether this is an app session (ADR 0024) rather than an agent's
    /// task.
    pub fn is_app(&self) -> bool {
        self.app.is_some()
    }

    /// The effective policy in force for this task now.
    pub fn policy(&self) -> Arc<EffectivePolicy> {
        Arc::clone(
            &self
                .policy
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// The context an action runs in: the task's, with the groups of the
    /// policy in force for it now (the catalog decides by them). For an
    /// agent's task that is the policy it started with.
    pub fn call_ctx(&self) -> TaskContext {
        let mut ctx = self.ctx.clone();
        ctx.groups = self.policy().groups().map(str::to_owned).collect();
        ctx.data_label = self.lock().data_label;
        ctx
    }

    /// Puts `policy` in force for this task's next calls.
    pub(crate) fn follow_policy(&self, policy: EffectivePolicy) {
        *self
            .policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(policy);
    }

    pub fn new(new: NewTask) -> Self {
        let now = Timestamp::now();
        Self {
            id: new.id,
            user: new.user,
            entry: new.entry,
            prompt: new.prompt,
            scope: new.scope,
            plan: new.plan,
            policy: std::sync::RwLock::new(Arc::new(new.policy)),
            ctx: new.ctx,
            sandbox: new.sandbox,
            cant_touch: new.cant_touch,
            settings: new.settings,
            layer: new.layer,
            driver_socket: new.driver_socket,
            continues: new.continues,
            routine: new.routine,
            schedule: new.schedule,
            app: new.app,
            started_at: now,
            gate: watch::Sender::new(Gate::Run),
            stop: Mutex::new(Some(new.stop)),
            in_flight: AtomicUsize::new(0),
            keeping: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            state: Mutex::new(TaskState {
                tile: TileState::Starting,
                caption: "Setting up a sandbox".into(),
                progress: None,
                feed: VecDeque::new(),
                activity: Vec::new(),
                changes: Vec::new(),
                written: Written::default(),
                snapshots: Vec::new(),
                actions: 0,
                failure: None,
                updated_at: now,
                takeover: None,
                sleeping: None,
                connected: false,
                title: new.title,
                title_fixed: new.title_fixed,
                data_label: None,
                answer: None,
                models_sent_to: Vec::new(),
                trace: Trace::default(),
            }),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, TaskState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Moves the gate to `to`, unless the task has ended: an ended task
    /// never holds or runs again, whoever was halfway through releasing or
    /// holding it as it ended (the Approvals model, specs/tla).
    pub fn set_gate(&self, to: Gate) {
        self.gate.send_if_modified(|gate| {
            if *gate == Gate::Ended || *gate == to {
                return false;
            }
            *gate = to;
            true
        });
    }

    /// Whether the task has ended: its gate says so before its tile does.
    pub fn ended(&self) -> bool {
        *self.gate.borrow() == Gate::Ended
    }

    /// Waits until the agent may act. `Err` once the task has ended.
    pub async fn wait_runnable(&self) -> Result<(), ()> {
        let mut gate = self.gate.subscribe();
        let state = gate.wait_for(|g| *g != Gate::Hold).await.map_err(|_| ())?;
        if *state == Gate::Ended {
            Err(())
        } else {
            Ok(())
        }
    }

    /// Roots covered by a snapshot taken since the task started.
    pub fn covered(&self, roots: &[PathBuf]) -> bool {
        let state = self.lock();
        roots.iter().all(|root| {
            state.snapshots.iter().any(|s| {
                s.handle
                    .parts
                    .iter()
                    .any(|p| p.roots.iter().any(|r| root.starts_with(r)))
            })
        })
    }

    /// What a tile shows. Every view and detail leaves glassd through here
    /// or [`Task::detail`], so this is where text an agent, an action
    /// argument or a file name wrote becomes one line of display text: a
    /// caption can't clear a terminal, nor a file name reorder a tile.
    pub fn view(&self, needs: Option<ApprovalView>) -> TaskView {
        let mut view = self.raw_view(needs);
        view.make_display_safe();
        view
    }

    fn raw_view(&self, needs: Option<ApprovalView>) -> TaskView {
        let state = self.lock();
        let needs = needs.filter(|_| state.tile.is_live());
        TaskView {
            id: self.id,
            agent: self.entry.id.clone(),
            agent_name: self.entry.name.clone(),
            user: self.user.name.clone(),
            mode: self.entry.mode,
            state: state.tile,
            title: state.title.clone(),
            caption: state.caption.clone(),
            progress: state.progress,
            feed: state.feed.iter().cloned().collect(),
            needs,
            receipt: (!state.tile.is_live()).then(|| state.receipt()),
            failure: state.failure.clone(),
            started_at: self.started_at,
            updated_at: state.updated_at,
            driver_socket: self.driver_socket.clone(),
            continues: self.continues.as_ref().map(|c| c.id),
            computer: self.sandbox.computer.clone(),
            persona: self.sandbox.persona.clone(),
        }
    }

    pub fn detail(&self, needs: Option<ApprovalView>) -> TaskDetail {
        let view = self.raw_view(needs.clone());
        let state = self.lock();
        let mut detail = TaskDetail {
            now: state.now_sentence(needs.as_ref()),
            prompt: self.prompt.clone(),
            plan: self.plan.clone(),
            activity: state.activity.clone(),
            changes: state.changes.clone(),
            cant_touch: self.cant_touch.clone(),
            snapshots: state.snapshots.iter().map(|s| s.marker.clone()).collect(),
            answer: state.answer.clone(),
            view,
        };
        detail.make_display_safe();
        detail
    }
}

/// The version of [`Kept`] this glassd writes and reads.
const KEPT_VERSION: u32 = 1;

/// Why a task that was working when glassd stopped has ended.
pub(crate) const RESTARTED: &str = "the system restarted while it was working";

/// A task as it is kept on disk (ADR 0020, item 3): what was fixed when it
/// started and its record so far, without what only a running task has
/// (its gate, its stop, its socket, the calls a routine would be made of).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Kept {
    version: u32,
    pub id: TaskId,
    pub user: Person,
    pub entry: CatalogEntry,
    prompt: String,
    scope: ScopeRequest,
    plan: Vec<String>,
    grants: Vec<Grant>,
    sandbox: SandboxSpec,
    cant_touch: Vec<String>,
    settings: BTreeMap<String, Value>,
    layer: Option<u32>,
    continues: Option<Continued>,
    routine: Option<RoutineRun>,
    schedule: Option<ScheduleRun>,
    pub app: Option<AppManifest>,
    started_at: Timestamp,
    tile: TileState,
    caption: String,
    progress: Option<Progress>,
    feed: Vec<FeedLine>,
    activity: Vec<ActivityEntry>,
    changes: Vec<ChangeEntry>,
    written: Written,
    snapshots: Vec<KeptSnapshot>,
    actions: u32,
    failure: Option<String>,
    updated_at: Timestamp,
    title: String,
    title_fixed: bool,
    data_label: Option<Label>,
    answer: Option<String>,
    models_sent_to: Vec<String>,
}

/// Whose task it is, as the kernel named them when it started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Person {
    pub name: String,
    pub uid: u32,
    gid: u32,
    home: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeptSnapshot {
    marker: SnapshotMarker,
    handle: SnapshotHandle,
}

impl Kept {
    /// Whether this glassd reads records of this version.
    pub fn readable(&self) -> bool {
        self.version == KEPT_VERSION
    }
}

impl Task {
    /// The task as it stands, to keep on disk.
    pub fn kept(&self) -> Kept {
        let state = self.lock();
        Kept {
            version: KEPT_VERSION,
            id: self.id,
            user: Person {
                name: self.user.name.clone(),
                uid: self.user.uid,
                gid: self.user.gid,
                home: self.user.home.clone(),
            },
            entry: self.entry.clone(),
            prompt: self.prompt.clone(),
            scope: self.scope.clone(),
            plan: self.plan.clone(),
            grants: self.ctx.grants.clone(),
            sandbox: self.sandbox.clone(),
            cant_touch: self.cant_touch.clone(),
            settings: self.settings.clone(),
            layer: self.layer,
            continues: self.continues.clone(),
            routine: self.routine.clone(),
            schedule: self.schedule.clone(),
            app: self.app.as_deref().cloned(),
            started_at: self.started_at,
            tile: state.tile,
            caption: state.caption.clone(),
            progress: state.progress,
            feed: state.feed.iter().cloned().collect(),
            activity: state.activity.clone(),
            changes: state.changes.clone(),
            written: state.written.clone(),
            snapshots: state
                .snapshots
                .iter()
                .map(|s| KeptSnapshot {
                    marker: s.marker.clone(),
                    handle: s.handle.clone(),
                })
                .collect(),
            actions: state.actions,
            failure: state.failure.clone(),
            updated_at: state.updated_at,
            title: state.title.clone(),
            title_fixed: state.title_fixed,
            data_label: state.data_label,
            answer: state.answer.clone(),
            models_sent_to: state.models_sent_to.clone(),
        }
    }

    /// A task read back from its record after glassd started again: ended,
    /// with what it changed and the snapshots to undo it from. `policy` is
    /// the one for an ended task (`Policy::for_ended`). Also whether it was
    /// still working when glassd stopped: it comes back as failed, saying
    /// so.
    pub fn restored(kept: Kept, policy: EffectivePolicy) -> (Self, bool) {
        let user = Caller {
            name: kept.user.name,
            uid: kept.user.uid,
            gid: kept.user.gid,
            home: kept.user.home,
        };
        let ctx = TaskContext {
            principal: AgentPrincipal {
                agent: kept.entry.id.clone(),
                on_behalf_of: UserRef {
                    name: user.name.clone(),
                    uid: user.uid,
                },
                task: kept.id,
            },
            owner: Owner {
                uid: user.uid,
                gid: user.gid,
            },
            home: user.home.clone(),
            grants: kept.grants,
            groups: policy.groups().map(str::to_owned).collect(),
            data_label: None,
        };
        // The calls a routine would be made of were in memory only.
        let mut trace = Trace::default();
        trace.end(Err(
            "the system has restarted since it ran, and its steps aren't kept through that".into(),
        ));
        let interrupted = kept.tile.is_live();
        let mut state = TaskState {
            tile: kept.tile,
            caption: kept.caption,
            progress: kept.progress,
            feed: kept.feed.into(),
            activity: kept.activity,
            changes: kept.changes,
            written: kept.written,
            snapshots: kept
                .snapshots
                .into_iter()
                .map(|s| TakenSnapshot {
                    marker: s.marker,
                    handle: s.handle,
                })
                .collect(),
            actions: kept.actions,
            failure: kept.failure,
            updated_at: kept.updated_at,
            // A take over ended with glassd: what the person did since is
            // theirs, not the task's.
            takeover: None,
            sleeping: None,
            connected: false,
            title: kept.title,
            title_fixed: kept.title_fixed,
            data_label: kept.data_label,
            answer: kept.answer,
            models_sent_to: kept.models_sent_to,
            trace,
        };
        if interrupted {
            state.tile = TileState::Failed;
            state.failure = Some(RESTARTED.into());
            state.progress = None;
            state.note(ActivityKind::State, format!("Failed: {RESTARTED}"));
        }
        let task = Self {
            id: kept.id,
            user,
            entry: kept.entry,
            prompt: kept.prompt,
            scope: kept.scope,
            plan: kept.plan,
            policy: std::sync::RwLock::new(Arc::new(policy)),
            ctx,
            sandbox: kept.sandbox,
            cant_touch: kept.cant_touch,
            settings: kept.settings,
            layer: kept.layer,
            // An attended task's socket went with the glassd that made it.
            driver_socket: None,
            continues: kept.continues,
            routine: kept.routine,
            schedule: kept.schedule,
            app: kept.app.map(Arc::new),
            started_at: kept.started_at,
            gate: watch::Sender::new(Gate::Ended),
            stop: Mutex::new(None),
            in_flight: AtomicUsize::new(0),
            keeping: AtomicBool::new(false),
            gone: AtomicBool::new(false),
            state: Mutex::new(state),
        };
        (task, interrupted)
    }
}

impl TaskState {
    /// Every root the task's snapshots cover.
    pub fn snapshot_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = self
            .snapshots
            .iter()
            .flat_map(|s| s.handle.parts.iter().flat_map(|p| p.roots.iter().cloned()))
            .collect();
        roots.sort();
        roots.dedup();
        roots
    }

    /// Adds a line to the activity log (AW-13). Lines are kept as display
    /// text, since most carry something the agent wrote, and the log also
    /// tells a harness that takes over what the task did.
    ///
    /// The log is in memory for as long as the task is, so it has a size
    /// whatever the agent does: a line is cut to [`ACTIVITY_LINE`]
    /// characters, and past [`ACTIVITY_LINES`] the oldest go (the audit
    /// log has every step, for good).
    pub fn note(&mut self, kind: ActivityKind, text: impl Into<String>) {
        let at = Timestamp::now();
        let text = text.into();
        let mut text = glass_api::clip(&text, ACTIVITY_LINE).into_owned();
        text.make_display_safe();
        self.activity.push(ActivityEntry { at, kind, text });
        // In one go every so often, not a shift of the whole log per line.
        if self.activity.len() > ACTIVITY_LINES + ACTIVITY_LINES / 8 {
            let gone = self.activity.len() - ACTIVITY_LINES;
            self.activity.drain(..gone);
        }
        self.updated_at = at;
    }

    /// Adds a completed action to the feed and the activity log.
    pub fn record_action(&mut self, verb: &str, target: &str, text: &str) {
        let at = Timestamp::now();
        let line = |text: &str| display_line(&glass_api::clip(text, ACTIVITY_LINE)).into_owned();
        self.feed.push_back(FeedLine {
            at,
            text: line(text),
            verb: line(verb),
            target: line(target),
        });
        while self.feed.len() > FEED_LINES {
            self.feed.pop_front();
        }
        self.actions += 1;
        self.note(ActivityKind::Action, text);
    }

    pub fn receipt(&self) -> Receipt {
        let count =
            |keep: fn(&ChangeEntry) -> bool| self.changes.iter().filter(|c| keep(c)).count();
        Receipt {
            actions: self.actions,
            changes: self.changes.len() as u32,
            undoable: count(|c| c.reversible && !c.undone) as u32,
            left_machine: count(|c| !c.reversible) as u32,
            models: self.models_sent_to.clone(),
            flagged: self
                .changes
                .iter()
                .filter(|c| !c.reversible)
                .map(|c| format!("{} (can't be undone)", c.target))
                .collect(),
        }
    }

    /// The "Now" bar: the current step in a full sentence (AW-12).
    fn now_sentence(&self, needs: Option<&ApprovalView>) -> String {
        match self.tile {
            TileState::Starting => "Setting up a sandbox for this task.".into(),
            TileState::Working => format!("{}.", self.caption.trim_end_matches('.')),
            TileState::NeedsYou => match needs {
                Some(ask) => format!("Waiting for you to approve: {}.", ask.summary),
                None => "Waiting for you.".into(),
            },
            TileState::Paused if self.sleeping.is_some() => {
                "Paused for sleep. It carries on when the computer wakes.".into()
            }
            TileState::Paused => "Paused. Resume to let the agent continue.".into(),
            TileState::YouAreDriving => {
                "Paused · you have control. Hand back when you're done.".into()
            }
            TileState::Done => {
                let r = self.receipt();
                let n = |count: u32, noun: &str| {
                    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
                };
                format!(
                    "Finished: {}, {}.",
                    n(r.actions, "action"),
                    n(r.changes, "change")
                )
            }
            TileState::Failed => {
                format!(
                    "Stopped because {}.",
                    self.failure
                        .as_deref()
                        .unwrap_or("the agent could not continue")
                )
            }
            TileState::Stopped => "Stopped by you.".into(),
        }
    }
}
