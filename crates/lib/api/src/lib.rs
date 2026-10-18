//! The control API: what `glassd` offers on its control socket to `glassctl`,
//! the desktop shell and (later) the admin agent.
//!
//! Every method is JSON-RPC 2.0 over `glass-rpc`. Method names live in
//! [`methods`]; each method's params and result types are named next to it.
//! The daemon authorizes every call by the caller's kernel-reported uid: users
//! see and act on their own tasks; root sees everything.
//!
//! This crate is a contract. Changing a type here changes the wire format for
//! every client, so add fields with `#[serde(default)]` and never repurpose one.

mod approval;
mod task;

use serde::{Deserialize, Serialize};

pub use approval::{AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalState, ApprovalView};
pub use task::{
    ActivityEntry, ActivityKind, AgentMode, AgentSummary, ChangeEntry, ChangeKind, FeedLine,
    HandoffCard, Progress, Receipt, ScopeLine, ScopeRequest, SnapshotMarker, TaskDetail, TaskRef,
    TaskRequest, TaskView, TileState, UndoReport, UndoRequest,
};

/// Where `glassd` listens unless configured otherwise.
pub const DEFAULT_CONTROL_SOCKET: &str = "/var/run/glasshouse/control.sock";
/// Environment variable clients read to find the control socket.
pub const CONTROL_SOCKET_ENV: &str = "GLASS_CONTROL_SOCKET";

/// Method names. Params → result types are noted on each.
pub mod methods {
    /// `()` → [`crate::SystemStatus`]
    pub const SYSTEM_STATUS: &str = "system.status";
    /// `()` → `Vec<`[`crate::AgentSummary`]`>`: agents the caller may run.
    pub const AGENTS_LIST: &str = "agents.list";
    /// [`crate::TaskRequest`] → [`crate::HandoffCard`]. No side effects (DA-2).
    pub const TASK_PLAN: &str = "task.plan";
    /// [`crate::TaskRequest`] → [`crate::TaskView`]. Starts the task (DA-3).
    pub const TASK_START: &str = "task.start";
    /// `()` → `Vec<`[`crate::TaskView`]`>`: one per tile.
    pub const TASK_LIST: &str = "task.list";
    /// [`crate::TaskRef`] → [`crate::TaskDetail`]: the expanded view.
    pub const TASK_GET: &str = "task.get";
    /// [`crate::TaskRef`] → [`crate::TaskView`]
    pub const TASK_PAUSE: &str = "task.pause";
    /// [`crate::TaskRef`] → [`crate::TaskView`]
    pub const TASK_RESUME: &str = "task.resume";
    /// [`crate::TaskRef`] → [`crate::TaskView`]
    pub const TASK_STOP: &str = "task.stop";
    /// [`crate::TaskRef`] → [`crate::TaskView`] (AW-20)
    pub const TASK_TAKE_OVER: &str = "task.take_over";
    /// [`crate::TaskRef`] → [`crate::TaskView`] (AW-21)
    pub const TASK_HAND_BACK: &str = "task.hand_back";
    /// [`crate::TaskRef`] → `()`: removes a finished tile.
    pub const TASK_DISMISS: &str = "task.dismiss";
    /// [`crate::UndoRequest`] → [`crate::UndoReport`] (AW-15, AU-21)
    pub const TASK_UNDO: &str = "task.undo";
    /// [`crate::TaskRef`] → [`crate::TaskView`]: the Failed tile's Retry.
    /// Starts the ended task's request again as a new task, under today's
    /// policy. What the first run saved stays (and stays undoable there).
    pub const TASK_RETRY: &str = "task.retry";
    /// `()` → `Vec<`[`crate::ApprovalView`]`>`: pending approvals.
    pub const APPROVAL_LIST: &str = "approval.list";
    /// [`crate::AnswerApproval`] → [`crate::ApprovalView`] (DA-11)
    pub const APPROVAL_ANSWER: &str = "approval.answer";
    /// [`crate::TaskRef`] → `Vec<audit record>`: rebuild a task end to end (AU-3).
    pub const AUDIT_TASK: &str = "audit.task";
    /// `()` → [`crate::AuditVerifyResult`]. Root only.
    pub const AUDIT_VERIFY: &str = "audit.verify";
    /// `()` → [`crate::PowerResult`]. Root only. The machine is about to
    /// sleep: every running agent pauses at its next safe point (PM-5).
    pub const SYSTEM_SUSPEND: &str = "system.suspend";
    /// `()` → [`crate::PowerResult`]. Root only. The machine woke: agents
    /// paused for sleep carry on.
    pub const SYSTEM_RESUME: &str = "system.resume";
    /// `()` → `()`, then [`EVENT`] notifications until disconnect.
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
    /// Server notification carrying an [`crate::Event`].
    pub const EVENT: &str = "event";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemStatus {
    pub version: String,
    /// The sandbox backend in use, for support and audit.
    pub sandbox: String,
    /// Whether that backend contains agents. Only development builds run
    /// without containment.
    #[serde(default)]
    pub contained: bool,
    /// Where the enforced policy came from.
    pub policy: String,
    pub tasks_running: usize,
    pub approvals_pending: usize,
}

/// How many tasks a suspend paused, or a resume let continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerResult {
    pub tasks: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditVerifyResult {
    pub records: u64,
    pub last_hash: String,
}

/// What subscribers are told. The shell redraws a tile per `TaskUpdated`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    TaskUpdated { task: Box<TaskView> },
    TaskRemoved { task: glass_types::TaskId },
}
