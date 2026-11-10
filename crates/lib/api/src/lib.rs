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
mod computer;
mod hardware;
mod schedule;
mod session;
pub mod settings;
mod tailoring;
mod task;
mod text;
mod update;

use serde::{Deserialize, Serialize};

pub use approval::{
    AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalState, ApprovalView, AskLabels,
};
pub use apps::{
    APP_SOCKET_ENV, APP_STATUS_MAX_WAIT_SECS, AppAction, AppCall, AppCallResult, AppFiles, AppLive,
    AppLiveResult, AppManifest, AppOpen, AppPermission, AppSession, AppSessionRef, AppStatus,
    AppStatusRequest, AppWaiting, DEFAULT_APP_SOCKET, LiveOp, ProfileApp, ProfileAppKind,
    app_bridge_port, app_methods,
};
pub use computer::{
    ComputerNew, ComputerRef, ComputerState, ComputerView, DISPLAY_INPUT_MAX, DisplayInput,
    DisplayInputRequest, FrameRequest, FrameView, INPUT_MAX_BYTES, PERSON_COMPUTER, PERSONAS_MAX,
    PersonaNew, PersonaRef, PersonaView, PointerButton, ScreenInput, ScreenRequest, ScreenView,
    StoredView,
};
pub use hardware::{PatchApply, PatchStatus, PatchTrial, TrialState};
pub use schedule::{
    SCHEDULE_FAILURES_MAX, SCHEDULE_PROMPT_MAX_CHARS, SCHEDULES_MAX, ScheduleEnable, ScheduleNew,
    ScheduleRef, ScheduleRunView, ScheduleView,
};
pub use session::{
    CODE_PREFIX, IssuedToken, LoginCode, PersonView, SessionAs, TOKEN_PREFIX, TokenExchange,
};
pub use tailoring::{
    CheckResult, CustomizationPoint, LayerChange, LayerCheck, LayerLine, LayerRef, LayerRevert,
    LayerStatus, LayerVersion, LayerView, PointKind,
};
pub use task::{
    ActivityEntry, ActivityKind, AgentMode, AgentSummary, ChangeEntry, ChangeKind, FeedLine,
    HandoffCard, PROMPT_MAX_CHARS, Progress, Receipt, RoutineDraft, RoutineFromTask, RoutineRef,
    RoutineView, ScopeLine, ScopeRequest, SnapshotMarker, TITLE_MAX_CHARS, TaskDetail, TaskRef,
    TaskRequest, TaskView, TileState, UndoReport, UndoRequest,
};
pub use text::{DisplaySafe, STAND_IN, clip, display_char, display_line, invisible};
pub use update::{
    OsUpdate, RestartWhen, UpdateApply, UpdateAvailable, UpdateRestart, UpdateStatus,
};

/// Where `glassd` listens unless configured otherwise.
pub const DEFAULT_CONTROL_SOCKET: &str = "/var/run/glasshouse/control.sock";

/// The general desktop agent (AE-3): what the command bar and "Hey Glass"
/// start.
pub const DESKTOP_AGENT: &str = "desktop-agent";
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
    /// `()` → `Vec<`[`crate::ComputerView`]`>`: the caller's agent
    /// computers (ADR 0065, 0066).
    pub const COMPUTER_LIST: &str = "computer.list";
    /// [`crate::ComputerNew`] → [`crate::ComputerView`]
    pub const COMPUTER_MAKE: &str = "computer.make";
    /// [`crate::SessionAs`] → [`crate::PersonView`]: this connection acts
    /// for the person the proof names, until it closes. The front's
    /// account only, once per connection (ADR 0067).
    pub const SESSION_AS: &str = "session.as";
    /// `()` → [`crate::PersonView`]: who this connection acts for.
    pub const SESSION_WHO: &str = "session.who";
    /// `()` → [`crate::LoginCode`]: a one-time code for `glass login`, for
    /// a person signed in at the load balancer.
    pub const TOKEN_CODE: &str = "token.code";
    /// [`crate::TokenExchange`] → [`crate::IssuedToken`]: the front trades a
    /// code `glass` brought, once, for a token for its person.
    pub const TOKEN_EXCHANGE: &str = "token.exchange";
    /// `()` → `u32`: every token of the person's goes; `glass` signs in
    /// again.
    pub const TOKEN_REVOKE: &str = "token.revoke";
    /// [`crate::PersonaNew`] → [`crate::ComputerView`]: a new agent on the
    /// caller's computer, made first if they have none (ADR 0068).
    pub const PERSONA_MAKE: &str = "persona.make";
    /// [`crate::PersonaRef`] → [`crate::ComputerView`]: the agent goes; its
    /// files on the computer stay.
    pub const PERSONA_REMOVE: &str = "persona.remove";
    /// `()` → `Vec<`[`crate::ScheduleView`]`>`: the caller's schedules
    /// (ADR 0069).
    pub const SCHEDULE_LIST: &str = "schedule.list";
    /// [`crate::ScheduleNew`] → [`crate::ScheduleView`]: one of the caller's
    /// agents, directed on a schedule, on their computer.
    pub const SCHEDULE_MAKE: &str = "schedule.make";
    /// [`crate::ScheduleRef`] → `()`
    pub const SCHEDULE_REMOVE: &str = "schedule.remove";
    /// [`crate::ScheduleEnable`] → [`crate::ScheduleView`]
    pub const SCHEDULE_ENABLE: &str = "schedule.enable";
    /// [`crate::ScreenRequest`] → [`crate::ScreenView`]
    pub const COMPUTER_SCREEN: &str = "computer.screen";
    /// [`crate::ComputerRef`] → [`crate::ComputerView`]: its person has the
    /// keyboard; its tasks wait.
    pub const COMPUTER_TAKE_OVER: &str = "computer.take_over";
    /// [`crate::ComputerRef`] → [`crate::ComputerView`]
    pub const COMPUTER_HAND_BACK: &str = "computer.hand_back";
    /// [`crate::ScreenInput`] → `()`: runs a line its person typed; what it
    /// prints shows on the screen. Only while taken over.
    pub const COMPUTER_INPUT: &str = "computer.input";
    /// [`crate::FrameRequest`] → [`crate::FrameView`]: what its display
    /// shows, for a computer with one (ADR 0072).
    pub const COMPUTER_FRAME: &str = "computer.frame";
    /// [`crate::DisplayInputRequest`] → `()`: its person's pointer and keys
    /// on its display. Only while taken over.
    pub const COMPUTER_DISPLAY: &str = "computer.display";
    /// [`crate::ComputerRef`] → [`crate::ComputerView`]: saved to the store
    /// and gone from this host (ADR 0065 §11).
    pub const COMPUTER_SLEEP: &str = "computer.sleep";
    /// [`crate::ComputerRef`] → [`crate::ComputerView`]
    pub const COMPUTER_WAKE: &str = "computer.wake";
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
    /// `()` → `Vec<`[`crate::RoutineView`]`>`: the caller's routines (DA-4).
    pub const ROUTINE_LIST: &str = "routine.list";
    /// [`crate::RoutineRef`] → [`crate::TaskView`]: starts one of the
    /// caller's routines now, as a task with the scope it was saved with.
    pub const ROUTINE_RUN: &str = "routine.run";
    /// [`crate::TaskRef`] → [`crate::RoutineDraft`]: one of the caller's
    /// tasks as replay code, written by this computer (ADR 0046).
    pub const ROUTINE_DRAFT: &str = "routine.draft";
    /// [`crate::RoutineFromTask`] → [`crate::RoutineView`]: saves one of the
    /// caller's tasks as a routine with that code.
    pub const ROUTINE_SAVE: &str = "routine.save";
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
    /// [`crate::PatchApply`] → [`crate::PatchStatus`]. Root only. Checks a
    /// kernel patch's bundle and sets it up in a boot environment of its
    /// own, which the next start tries once (ADR 0057).
    pub const HARDWARE_PATCH_APPLY: &str = "hardware.patch.apply";
    /// `()` → [`crate::PatchStatus`]. Root only.
    pub const HARDWARE_PATCH_STATUS: &str = "hardware.patch.status";
    /// `()` → [`crate::PatchStatus`]. Root only. Takes back a patch set up
    /// but not yet tried.
    pub const HARDWARE_PATCH_CANCEL: &str = "hardware.patch.cancel";
    /// `()` → [`crate::PatchStatus`]. Root only. Takes out the patch the
    /// computer runs, on probation or kept: the boot environment it was
    /// made from is the one the next start uses.
    pub const HARDWARE_PATCH_UNDO: &str = "hardware.patch.undo";
    /// `()` → `()`. Root only, from rc.d as the computer shuts down: a
    /// kernel patch on probation is started again by the next start, once
    /// (ADR 0057). A crash never runs this, so it goes back instead.
    pub const SYSTEM_SHUTDOWN: &str = "system.shutdown";
    /// `()` → [`crate::UpdateStatus`] with what's available: the newest
    /// release on this computer's channel, checked (ADR 0061). Root only.
    pub const SYSTEM_UPDATE_CHECK: &str = "system.update.check";
    /// [`crate::UpdateApply`] → [`crate::OsUpdate`]: installs the newest
    /// release in a boot environment of its own, which the next start uses
    /// once. Root only.
    pub const SYSTEM_UPDATE_APPLY: &str = "system.update.apply";
    /// `()` → [`crate::UpdateStatus`]. Root only.
    pub const SYSTEM_UPDATE_STATUS: &str = "system.update.status";
    /// `()` → [`crate::OsUpdate`]: takes back an update not yet started.
    pub const SYSTEM_UPDATE_CANCEL: &str = "system.update.cancel";
    /// [`crate::UpdateRestart`] → [`crate::UpdateStatus`]: restarts into
    /// the update installed, now or tonight, or not tonight after all.
    /// Root only.
    pub const SYSTEM_UPDATE_RESTART: &str = "system.update.restart";
    /// `()` → [`crate::OsUpdate`]: back to the version before, from the
    /// next start.
    pub const SYSTEM_UPDATE_UNDO: &str = "system.update.undo";
    /// `()` → [`crate::settings::SettingsView`]: the caller's device
    /// settings (UX-1), as their desktop applies them.
    pub const SETTINGS_GET: &str = "settings.get";
    /// [`crate::settings::SettingChoice`] → [`crate::settings::SettingsView`]:
    /// the caller changes one of their own settings, as the Settings app's
    /// `settings.set` does, from their desktop. Audited.
    pub const SETTINGS_SET: &str = "settings.set";
    /// [`crate::settings::BrightnessChange`] →
    /// [`crate::settings::Brightness`]: the screen's brightness (ADR 0060),
    /// from the desktop at the screen (its brightness keys) or root.
    pub const DISPLAY_BRIGHTNESS: &str = "display.brightness";
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
    /// glassd's check of the system's files at its start (ADR 0064).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_files: Option<SystemCheck>,
}

/// glassd's check, at every start, of the system's files against the list
/// the OS key signed (ADR 0064, EP-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemCheck {
    pub state: SystemCheckState,
    /// The image the list is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Files checked.
    #[serde(default)]
    pub files: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<glass_types::Timestamp>,
    #[serde(default)]
    pub took_ms: u64,
    /// Not checked: why. Changed: what, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What differs from the list: the first few, `/usr/bin/yes (changed)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub differences: Vec<String>,
    /// How many differ.
    #[serde(default)]
    pub different: u64,
    /// Files glassd made read-only again, having found them writable.
    #[serde(default)]
    pub sealed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemCheckState {
    /// Nothing to check against (a development build), or nothing to
    /// check it with: agents run.
    Unchecked,
    /// Under way: agents start once it's done.
    Checking,
    /// The system is the image's, as the OS key signed it.
    Verified,
    /// Something differs, or the list doesn't verify: no agent starts.
    Changed,
}

impl SystemCheck {
    pub fn unchecked(why: impl Into<String>) -> Self {
        Self {
            state: SystemCheckState::Unchecked,
            version: None,
            files: 0,
            checked_at: None,
            took_ms: 0,
            note: Some(why.into()),
            differences: Vec::new(),
            different: 0,
            sealed: 0,
        }
    }
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
    /// Bytes after the last whole record: one being written, or one a
    /// crash or a full disk cut short. Not corruption (AU-2, ADR 0053).
    #[serde(default)]
    pub unfinished: u64,
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
