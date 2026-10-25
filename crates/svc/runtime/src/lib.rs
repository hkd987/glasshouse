//! The agent runtime: everything between "the user handed off a task" and
//! "the task is done, logged and undoable".
//!
//! - [`Runtime`] owns tasks: planning (the handoff card), starting, the tile
//!   state machine, pause / take over / stop, undo, and approvals.
//! - Each task runs one agent in its own sandbox. The agent's only way out is
//!   the task's MCP socket ([`agent_api`]); every tool call goes through the
//!   action pipeline in `execute`: hold while paused → preview → decide →
//!   ask → snapshot → invoke → record. Model calls (`model.complete`) take
//!   the same path, routed by the labels of what the task has read and sent
//!   through the model gateway (ADR 0018).
//! - [`ControlApi`] serves the `glass-api` control socket for `glassctl` and
//!   the desktop shell.
//! - [`AppApi`] serves the app socket: an installed app's session acts for
//!   the user who opened it, calling only the actions its manifest declares,
//!   through the same pipeline (ADR 0024).
//!
//! Blocking platform work (sandboxes, snapshots, file I/O) runs on the
//! blocking pool; nothing here holds a lock across an `.await`.

mod agent_api;
mod app_api;
mod apps;
mod caller;
mod config;
mod continuation;
mod control_api;
mod error;
mod execute;
mod flows;
pub mod hwpatch;
mod labels;
mod model;
pub mod osupdate;
mod routines;
mod runtime;
mod seat;
mod studio;
mod supervise;
pub mod system;
mod tailor;
mod task;
#[cfg(test)]
mod testenvs;
mod title;
mod undo;
mod written;

pub use app_api::AppApi;
pub use apps::MAX_OPEN_APPS_PER_USER;
pub use caller::Caller;
pub use config::{
    CatalogEntry, CatalogGate, NoOrgApps, NoPersonalApps, OpenCatalog, OrgApps, PersonalApp,
    PersonalApps, RuntimeConfig,
};
pub use control_api::ControlApi;
pub use error::RuntimeError;
pub use runtime::{RunningAgent, Runtime, RuntimeParts, SettingsHandle};
pub use seat::WAKE_WAIT;
pub use studio::StudioSurroundings;

/// The built-in tool every agent has for keeping its tile caption and
/// progress current (AW-4). It has no side effects and needs no policy.
pub const REPORT_PROGRESS_TOOL: &str = "report_progress";
/// The longest answer an agent may leave the person (ADR 0041).
pub const ANSWER_MAX_CHARS: usize = 4000;
