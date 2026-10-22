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
    /// Driven by the user's own harness, such as Claude Code, through
    /// `glass-mcp`; glassd starts no program for it (HX-1).
    #[serde(default)]
    pub attended: bool,
    /// For an attended agent, the program the person's own client runs, as
    /// them, to drive it, such as `glass-claude-runner` (ADR 0042). None
    /// when the person connects their harness by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
}

/// A task handed off in plain language (DA-1), with the scope the user set
/// on the handoff card (DA-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRequest {
    pub agent: AgentId,
    pub prompt: String,
    #[serde(default)]
    pub scope: ScopeRequest,
    /// A short name for the task, one line of at most [`TITLE_MAX_CHARS`]
    /// characters. Without one, the OS names it from the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The longest task title, in characters.
pub const TITLE_MAX_CHARS: usize = 60;

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
    /// The user's layer on this agent, if they tailored it (TL-2).
    #[serde(default)]
    pub layer: Option<crate::LayerLine>,
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

/// One of the caller's routines (DA-4): a task saved with its scope and a
/// script, which runs when they start it or on its schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutineView {
    pub routine: String,
    pub name: String,
    pub version: u32,
    /// When it runs by itself, in words ("every weekday at 09:00"), or
    /// "when started".
    pub runs: String,
    /// What it does: the request each run starts with.
    pub request: String,
    pub agent: AgentId,
    /// The tools its script calls.
    pub calls: Vec<String>,
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutineRef {
    pub routine: String,
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
    /// The task's short name, such as "Reconcile the October invoices",
    /// for lists like the command bar's Recent. Empty from an older glassd.
    #[serde(default)]
    pub title: String,
    /// One-line, plain-language caption of the current step.
    pub caption: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// The last three feed lines, oldest first.
    pub feed: Vec<FeedLine>,
    /// The pending approval while the state is `NeedsYou`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<ApprovalView>,
    /// What the task did, once it has ended (Done, Failed or Stopped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub started_at: Timestamp,
    pub updated_at: Timestamp,
    /// For a task the user's own harness drives: the socket to connect it
    /// to, as `glass-mcp <socket>`. Only the task's user can connect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_socket: Option<PathBuf>,
    /// The ended task this one continues, when it was taken over in the
    /// user's harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<TaskId>,
}

/// What an ended task did: the Done tile's receipt, and the line under a
/// Failed or Stopped tile's reason ("Nothing left the machine…").
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Receipt {
    pub actions: u32,
    pub changes: u32,
    /// Changes that can still be undone.
    #[serde(default)]
    pub undoable: u32,
    /// Changes that left the machine, such as sent mail. Can't be undone.
    #[serde(default)]
    pub left_machine: u32,
    /// The providers its model calls went to ("Anthropic"): what it sent
    /// them left the machine too, though nothing there changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Things the user should look at, such as irreversible actions taken.
    pub flagged: Vec<String>,
}

impl Receipt {
    /// One plain sentence on what's safe and what isn't, for a tile that
    /// stopped early: "Nothing left the machine. 26 changes can be undone."
    pub fn outcome(&self) -> String {
        let models = match self.models.as_slice() {
            [] => String::new(),
            [one] => format!("its model calls to {one}"),
            [rest @ .., last] => format!("its model calls to {} and {last}", rest.join(", ")),
        };
        let left = match (self.left_machine, models.is_empty()) {
            (0, true) => "Nothing left the machine.".to_owned(),
            (0, false) => format!("Nothing left the machine but {models}."),
            (1, _) => "1 change left the machine and can't be undone.".to_owned(),
            (n, _) => format!("{n} changes left the machine and can't be undone."),
        };
        let left = match (self.left_machine, models.is_empty()) {
            (1.., false) => format!("{left} So did {models}."),
            _ => left,
        };
        let undoable = match self.undoable {
            0 if self.changes == 0 => "Nothing was changed.".to_owned(),
            0 => String::new(),
            1 => "1 change can be undone.".to_owned(),
            n => format!("{n} changes can be undone."),
        };
        [left, undoable]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
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
    /// What the agent told the person, in words, when it was done (AE-3,
    /// ADR 0041): an answer longer than a caption, in paragraphs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
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
    /// Stable within the task; undo one change with `UndoRequest::changes`.
    #[serde(default)]
    pub id: u32,
    pub at: Timestamp,
    pub kind: ChangeKind,
    /// What changed, as the user would name it ("Drafts: Invoice 0407").
    pub target: String,
    pub action: ActionName,
    pub reversible: bool,
    /// The files it wrote, as `~/…` paths. Undoing the change puts these
    /// back.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Rolled back since (by Undo all, a roll back, or its own Undo).
    #[serde(default)]
    pub undone: bool,
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
    /// Undo only these changes (`ChangeEntry::id`), the per-row Undo in the
    /// Changes view. Empty means every change since `to`. Undoing a change
    /// puts its files back as they were just before it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<u32>,
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
    /// Files created during the task, removed again.
    pub removed: Vec<String>,
    /// What wasn't undone, with the reason: changes that left the machine,
    /// and files left as they are because someone changed them since.
    pub not_undone: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_says_what_is_safe() {
        let receipt = |changes, undoable, left_machine| Receipt {
            actions: 30,
            changes,
            undoable,
            left_machine,
            models: Vec::new(),
            flagged: Vec::new(),
        };
        assert_eq!(
            receipt(26, 26, 0).outcome(),
            "Nothing left the machine. 26 changes can be undone."
        );
        assert_eq!(
            receipt(3, 2, 1).outcome(),
            "1 change left the machine and can't be undone. 2 changes can be undone."
        );
        assert_eq!(
            receipt(0, 0, 0).outcome(),
            "Nothing left the machine. Nothing was changed."
        );
        assert_eq!(
            receipt(2, 0, 2).outcome(),
            "2 changes left the machine and can't be undone."
        );
        // What a task sent its models left the machine too.
        let sent = |left_machine, models: &[&str]| Receipt {
            models: models.iter().map(|m| (*m).to_owned()).collect(),
            ..receipt(left_machine, 0, left_machine)
        };
        assert_eq!(
            sent(0, &["Anthropic"]).outcome(),
            "Nothing left the machine but its model calls to Anthropic. Nothing was changed."
        );
        assert_eq!(
            sent(1, &["Amazon Bedrock", "Anthropic"]).outcome(),
            "1 change left the machine and can't be undone. So did its model calls to \
             Amazon Bedrock and Anthropic."
        );
    }
}
