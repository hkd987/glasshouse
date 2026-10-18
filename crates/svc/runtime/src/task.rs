//! One task's identity (fixed at start) and its mutable record.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, MutexGuard};

use glass_actions::TaskContext;
use glass_api::{
    ActivityEntry, ActivityKind, ApprovalView, ChangeEntry, DisplaySafe, FeedLine, Progress,
    Receipt, ScopeRequest, SnapshotMarker, TaskDetail, TaskView, TileState, display_line,
};
use glass_platform::sandbox::SandboxSpec;
use glass_platform::snapshot::SnapshotHandle;
use glass_policy::EffectivePolicy;
use glass_types::{TaskId, Timestamp};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use crate::written::Written;
use crate::{Caller, CatalogEntry};

/// Lines a tile's feed shows (AW-3).
const FEED_LINES: usize = 3;

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
    pub policy: EffectivePolicy,
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
    pub started_at: Timestamp,
    pub gate: watch::Sender<Gate>,
    /// Fires once to make the supervisor kill the agent.
    pub stop: Mutex<Option<oneshot::Sender<()>>>,
    /// Actions past every check and running now. Sleep waits for these.
    pub in_flight: AtomicUsize,
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
    pub title: String,
    /// The request gave the title, so the agent can't rename the task.
    pub title_fixed: bool,
    pub stop: oneshot::Sender<()>,
}

/// What a continued task tells the harness about the task it carries on
/// from.
#[derive(Debug, Clone)]
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
    pub fn new(new: NewTask) -> Self {
        let now = Timestamp::now();
        Self {
            id: new.id,
            user: new.user,
            entry: new.entry,
            prompt: new.prompt,
            scope: new.scope,
            plan: new.plan,
            policy: new.policy,
            ctx: new.ctx,
            sandbox: new.sandbox,
            cant_touch: new.cant_touch,
            settings: new.settings,
            layer: new.layer,
            driver_socket: new.driver_socket,
            continues: new.continues,
            started_at: now,
            gate: watch::Sender::new(Gate::Run),
            stop: Mutex::new(Some(new.stop)),
            in_flight: AtomicUsize::new(0),
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
            }),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, TaskState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
            view,
        };
        detail.make_display_safe();
        detail
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
    pub fn note(&mut self, kind: ActivityKind, text: impl Into<String>) {
        let at = Timestamp::now();
        let mut text = text.into();
        text.make_display_safe();
        self.activity.push(ActivityEntry { at, kind, text });
        self.updated_at = at;
    }

    /// Adds a completed action to the feed and the activity log.
    pub fn record_action(&mut self, verb: &str, target: &str, text: &str) {
        let at = Timestamp::now();
        self.feed.push_back(FeedLine {
            at,
            text: display_line(text).into_owned(),
            verb: display_line(verb).into_owned(),
            target: display_line(target).into_owned(),
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
