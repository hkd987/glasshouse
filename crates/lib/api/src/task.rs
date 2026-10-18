use std::path::PathBuf;

use glass_types::{ActionName, AgentId, Label, SnapshotId, TaskId, Tier, Timestamp};
use serde::{Deserialize, Serialize};

use crate::ApprovalView;

/// How an agent works, which decides what its tile shows (AW-2, AW-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    /// Calls app actions; the tile shows a three-line activity feed.
    Actions,
    /// Drives apps on its own virtual display; the tile shows a thumbnail.
    Screen,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id: AgentId,
    pub name: String,
    /// The published version, such as "2.3".
    #[serde(default)]
    pub version: String,
    pub description: String,
    pub mode: AgentMode,
}

/// A task handed off in plain language (DA-1), with the scope the user set
/// on the handoff card (DA-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRequest {
    pub agent: AgentId,
    pub prompt: String,
    #[serde(default)]
    pub scope: ScopeRequest,
}

/// What the task may touch. Paths are absolute paths on this machine and must
/// be ones the requesting user can access.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ScopeRequest {
    /// Folders or files the agent may read.
    #[serde(default)]
    pub read: Vec<PathBuf>,
    /// Folders the agent may change. Changes go through brokered actions so
    /// every write is tiered, snapshotted and logged.
    #[serde(default)]
    pub write: Vec<PathBuf>,
    /// Extra limits from the user, such as `mail.send = never` ("block
    /// sends"). Can only make tiers stricter (DA-10).
    #[serde(default)]
    pub restrict: Vec<(String, Tier)>,
}

/// The card shown before anything runs (DA-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffCard {
    pub agent: AgentSummary,
    pub prompt: String,
    /// The plan, in 3 to 7 steps.
    pub plan: Vec<String>,
    /// Apps and data the task will touch.
    pub touches: Vec<ScopeLine>,
    /// What it will ask before doing.
    pub asks_before: Vec<String>,
    /// What runs, then shows a quiet line in the tile feed.
    pub notifies: Vec<String>,
    /// What it can't do. Policy limits appear locked (DA-3).
    pub cant_touch: Vec<String>,
    /// The most sensitive data the task may open ("Data and model").
    #[serde(default)]
    pub max_label: Option<Label>,
    /// Which model does the work, such as "Claude via Amazon Bedrock".
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeLine {
    pub path: PathBuf,
    pub access: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRef {
    pub task: TaskId,
}

/// A tile's state (PRD "Tile states"). "Collapsed" is how the shell shows
/// tiles, not a task state, so it is not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TileState {
    /// Setting up the sandbox.
    Starting,
    /// Calm, blue dot.
    Working,
    /// Orange ring, the only state that pulses.
    NeedsYou,
    /// Paused by the user, not driving.
    Paused,
    /// Blue ring, "paused · you have control".
    YouAreDriving,
    /// Receipt: counts, items flagged.
    Done,
    /// Red ring, reason in one line.
    Failed,
    /// Stopped by the user before finishing.
    Stopped,
}

impl TileState {
    /// Whether the task can still act.
    pub fn is_live(self) -> bool {
        !matches!(
            self,
            TileState::Done | TileState::Failed | TileState::Stopped
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            TileState::Starting => "Starting",
            TileState::Working => "Working",
            TileState::NeedsYou => "Needs you",
            TileState::Paused => "Paused",
            TileState::YouAreDriving => "You're driving",
            TileState::Done => "Done",
            TileState::Failed => "Failed",
            TileState::Stopped => "Stopped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
}

/// One line of a tile's activity feed: verb + target (AW-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedLine {
    pub at: Timestamp,
    /// The whole line, such as "Write Reports/summary.csv".
    pub text: String,
    /// The verb alone ("write"), for tiles that set it in its own column.
    #[serde(default)]
    pub verb: String,
    /// What it acted on ("Reports/summary.csv").
    #[serde(default)]
    pub target: String,
}

/// Everything a tile renders (AW-1 to AW-4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskView {
    pub id: TaskId,
    pub agent: AgentId,
    pub agent_name: String,
    pub user: String,
    pub mode: AgentMode,
    pub state: TileState,
    /// One-line, plain-language caption of the current step.
    pub caption: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// The last three feed lines, oldest first.
    pub feed: Vec<FeedLine>,
    /// The pending approval while the state is `NeedsYou`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<ApprovalView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub started_at: Timestamp,
    pub updated_at: Timestamp,
}

/// What a finished task did (the Done tile).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Receipt {
    pub actions: u32,
    pub changes: u32,
    /// Things the user should look at, such as irreversible actions taken.
    pub flagged: Vec<String>,
}

/// The expanded view (AW-10 to AW-16).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub view: TaskView,
    pub prompt: String,
    pub plan: Vec<String>,
    /// The "Now" bar: the current step in a full sentence (AW-12).
    pub now: String,
    /// Timestamped log including snapshots (AW-13, Activity tab).
    pub activity: Vec<ActivityEntry>,
    /// Files, records and messages the agent changed (AW-13, Changes tab).
    pub changes: Vec<ChangeEntry>,
    /// What policy blocks for this task (AW-14).
    pub cant_touch: Vec<String>,
    /// Timeline markers to roll back to (AW-15).
    pub snapshots: Vec<SnapshotMarker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Action,
    Notice,
    Approval,
    Snapshot,
    Blocked,
    State,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub at: Timestamp,
    pub kind: ActivityKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
    Moved,
    /// Left the machine, such as sent mail. Not reversible.
    Sent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeEntry {
    pub at: Timestamp,
    pub kind: ChangeKind,
    pub target: String,
    pub action: ActionName,
    pub reversible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMarker {
    pub id: SnapshotId,
    pub at: Timestamp,
    pub reason: String,
}

/// Roll a task back: to a snapshot marker, or all the way ("Undo all").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoRequest {
    pub task: TaskId,
    /// Roll back to this marker. `None` undoes everything (AU-21).
    #[serde(default)]
    pub to: Option<SnapshotId>,
    /// Report what would change without changing anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// What a rollback did, or would do (AU-21: lists anything it couldn't undo).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UndoReport {
    pub dry_run: bool,
    /// Files put back to their earlier content.
    pub restored: Vec<String>,
    /// Files the agent created, removed again.
    pub removed: Vec<String>,
    /// Changes that can't be undone, with the reason.
    pub not_undone: Vec<String>,
}
