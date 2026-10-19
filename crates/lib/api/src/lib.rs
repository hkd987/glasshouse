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
//!
//! Text in a view that an agent, an action argument or a file name could
//! have written is one line of display text ([`display_line`]): glassd makes
//! views [`DisplaySafe`] as it builds them, and clients pass what they print
//! through the same function.

mod approval;
mod apps;
pub mod settings;
mod tailoring;
mod task;
mod text;

use serde::{Deserialize, Serialize};

pub use approval::{
    AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalState, ApprovalView, AskLabels,
};
pub use apps::{
    APP_SOCKET_ENV, APP_STATUS_MAX_WAIT_SECS, AppAction, AppCall, AppCallResult, AppFiles,
    AppManifest, AppOpen, AppPermission, AppSession, AppSessionRef, AppStatus, AppStatusRequest,
    AppWaiting, DEFAULT_APP_SOCKET, ProfileApp, ProfileAppKind, app_bridge_port, app_methods,
};
pub use tailoring::{
    CheckResult, CustomizationPoint, LayerChange, LayerCheck, LayerLine, LayerRef, LayerRevert,
    LayerStatus, LayerVersion, LayerView, PointKind,
};
pub use task::{
    ActivityEntry, ActivityKind, AgentMode, AgentSummary, ChangeEntry, ChangeKind, FeedLine,
    HandoffCard, Progress, Receipt, ScopeLine, ScopeRequest, SnapshotMarker, TITLE_MAX_CHARS,
    TaskDetail, TaskRef, TaskRequest, TaskView, TileState, UndoReport, UndoRequest,
};
pub use text::{DisplaySafe, STAND_IN, display_char, display_line, invisible};

/// Where `glassd` listens unless configured otherwise.
pub const DEFAULT_CONTROL_SOCKET: &str = "/var/run/glasshouse/control.sock";
/// Environment variable clients read to find the control socket.
pub const CONTROL_SOCKET_ENV: &str = "GLASS_CONTROL_SOCKET";

/// Who is at the screen, from the sign-in screen (ADR 0031).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeatBegin {
    pub user: String,
    /// The `su` running their desktop: its one child is the desktop.
    pub session_pid: u32,
}

/// Method names. Params → result types are noted on each.
pub mod methods {
    /// `()` → [`crate::SystemStatus`]
    pub const SYSTEM_STATUS: &str = "system.status";
    /// `()` → `Vec<`[`crate::AgentSummary`]`>`: agents the caller may run.
    pub const AGENTS_LIST: &str = "agents.list";
    /// `()` → `Vec<`[`crate::ProfileApp`]`>`: the apps in the caller's
    /// profile, which is all the launcher offers them.
    pub const APPS_LIST: &str = "apps.list";
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
    /// [`crate::TaskRef`] → [`crate::TaskView`]: Take over on an ended task
    /// (AW-22 for actions agents, which have no screen to hand over).
    /// Starts a new task with the same request and scope, driven by the
    /// user's own harness through `glass-mcp <driver_socket>`, with what the
    /// first task did and why it stopped in the harness's instructions.
    pub const TASK_CONTINUE: &str = "task.continue";
    /// [`crate::TaskRef`] → [`crate::TaskView`]: the Failed tile's Retry.
    /// Starts the ended task's request again as a new task, under today's
    /// policy. What the first run saved stays (and stays undoable there).
    pub const TASK_RETRY: &str = "task.retry";
    /// [`crate::LayerRef`] → [`crate::LayerView`]: the caller's layer on an
    /// agent and what can be changed (TL-1, TL-2).
    pub const LAYER_GET: &str = "layer.get";
    /// [`crate::LayerChange`] → [`crate::LayerView`]: saves and turns on a
    /// new version, after its checks pass (TL-2, TL-10).
    pub const LAYER_SET: &str = "layer.set";
    /// [`crate::LayerRevert`] → [`crate::LayerView`]: switches to an earlier
    /// version, or turns the layer off.
    pub const LAYER_REVERT: &str = "layer.revert";
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
    /// `()` → [`crate::settings::SettingsView`]: the caller's device
    /// settings (UX-1), as their desktop applies them.
    pub const SETTINGS_GET: &str = "settings.get";
    /// [`crate::SeatBegin`] → `()`. Root only: the sign-in screen says who
    /// is at the screen and which `su` runs their desktop (ADR 0031).
    pub const SEAT_BEGIN: &str = "seat.begin";
    /// `()` → `()`. Root only: their desktop ended.
    pub const SEAT_END: &str = "seat.end";
    /// `()` → `()`. From the desktop at the screen only: the person is
    /// using it. At most every 15 seconds while there is input.
    pub const SEAT_ACTIVE: &str = "seat.active";
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
    TaskUpdated {
        task: Box<TaskView>,
    },
    TaskRemoved {
        task: glass_types::TaskId,
    },
    /// The subscriber's device settings changed: a new policy, or their
    /// own choice. Ask `settings.get` again (UX-1).
    SettingsChanged,
}
