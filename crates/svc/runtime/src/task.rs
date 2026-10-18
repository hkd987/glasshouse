//! One task's identity (fixed at start) and its mutable record.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use glass_actions::TaskContext;
use glass_api::{
    ActivityEntry, ActivityKind, ApprovalView, ChangeEntry, FeedLine, Progress, Receipt,
    SnapshotMarker, TaskDetail, TaskView, TileState,
};
use glass_platform::sandbox::SandboxSpec;
use glass_platform::snapshot::SnapshotHandle;
use glass_policy::EffectivePolicy;
use glass_types::{TaskId, Timestamp};
use tokio::sync::{oneshot, watch};

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
    pub plan: Vec<String>,
    pub policy: EffectivePolicy,
    pub ctx: TaskContext,
    pub sandbox: SandboxSpec,
    pub cant_touch: Vec<String>,
    pub started_at: Timestamp,
    pub gate: watch::Sender<Gate>,
    /// Fires once to make the supervisor kill the agent.
    pub stop: Mutex<Option<oneshot::Sender<()>>>,
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
    pub snapshots: Vec<TakenSnapshot>,
    pub actions: u32,
    pub failure: Option<String>,
    pub updated_at: Timestamp,
    /// Taken at take over, to tell the agent what the user changed (AW-21).
    pub takeover: Option<SnapshotHandle>,
}

impl Task {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TaskId,
        user: Caller,
        entry: CatalogEntry,
        prompt: String,
        plan: Vec<String>,
        policy: EffectivePolicy,
        ctx: TaskContext,
        sandbox: SandboxSpec,
        cant_touch: Vec<String>,
        stop: oneshot::Sender<()>,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            id,
            user,
            entry,
            prompt,
            plan,
            policy,
            ctx,
            sandbox,
            cant_touch,
            started_at: now,
            gate: watch::Sender::new(Gate::Run),
            stop: Mutex::new(Some(stop)),
            state: Mutex::new(TaskState {
                tile: TileState::Starting,
                caption: "Setting up a sandbox".into(),
                progress: None,
                feed: VecDeque::new(),
                activity: Vec::new(),
                changes: Vec::new(),
                snapshots: Vec::new(),
                actions: 0,
                failure: None,
                updated_at: now,
                takeover: None,
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

    pub fn view(&self, needs: Option<ApprovalView>) -> TaskView {
        let state = self.lock();
        let needs = needs.filter(|_| state.tile.is_live());
        TaskView {
            id: self.id,
            agent: self.entry.id.clone(),
            agent_name: self.entry.name.clone(),
            user: self.user.name.clone(),
            mode: self.entry.mode,
            state: state.tile,
            caption: state.caption.clone(),
            progress: state.progress,
            feed: state.feed.iter().cloned().collect(),
            needs,
            receipt: (state.tile == TileState::Done).then(|| state.receipt()),
            failure: state.failure.clone(),
            started_at: self.started_at,
            updated_at: state.updated_at,
        }
    }

    pub fn detail(&self, needs: Option<ApprovalView>) -> TaskDetail {
        let view = self.view(needs.clone());
        let state = self.lock();
        TaskDetail {
            now: state.now_sentence(needs.as_ref()),
            prompt: self.prompt.clone(),
            plan: self.plan.clone(),
            activity: state.activity.clone(),
            changes: state.changes.clone(),
            cant_touch: self.cant_touch.clone(),
            snapshots: state.snapshots.iter().map(|s| s.marker.clone()).collect(),
            view,
        }
    }
}

impl TaskState {
    /// Adds a line to the activity log (AW-13).
    pub fn note(&mut self, kind: ActivityKind, text: impl Into<String>) {
        let at = Timestamp::now();
        self.activity.push(ActivityEntry {
            at,
            kind,
            text: text.into(),
        });
        self.updated_at = at;
    }

    /// Adds a completed action to the feed and the activity log.
    pub fn record_action(&mut self, text: &str) {
        let at = Timestamp::now();
        self.feed.push_back(FeedLine {
            at,
            text: text.to_owned(),
        });
        while self.feed.len() > FEED_LINES {
            self.feed.pop_front();
        }
        self.actions += 1;
        self.note(ActivityKind::Action, text);
    }

    pub fn receipt(&self) -> Receipt {
        Receipt {
            actions: self.actions,
            changes: self.changes.len() as u32,
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
            TileState::Paused => "Paused. Resume to let the agent continue.".into(),
            TileState::YouAreDriving => {
                "Paused · you have control. Hand back when you're done.".into()
            }
            TileState::Done => {
                let r = self.receipt();
                format!("Finished: {} actions, {} changes.", r.actions, r.changes)
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
