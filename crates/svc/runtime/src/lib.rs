//! The agent runtime: everything between "the user handed off a task" and
//! "the task is done, logged and undoable".
//!
//! - [`Runtime`] owns tasks: planning (the handoff card), starting, the tile
//!   state machine, pause / take over / stop, undo, and approvals.
//! - Each task runs one agent in its own sandbox. The agent's only way out is
//!   the task's MCP socket ([`agent_api`]); every tool call goes through the
//!   action pipeline in `execute`: hold while paused → preview → decide →
//!   ask → snapshot → invoke → record.
//! - [`ControlApi`] serves the `glass-api` control socket for `glassctl` and
//!   the desktop shell.
//!
//! Blocking platform work (sandboxes, snapshots, file I/O) runs on the
//! blocking pool; nothing here holds a lock across an `.await`.

mod agent_api;
mod caller;
mod config;
mod continuation;
mod control_api;
mod error;
mod execute;
mod labels;
mod runtime;
mod supervise;
mod tailor;
mod task;
mod undo;

pub use caller::Caller;
pub use config::{CatalogEntry, RuntimeConfig};
pub use control_api::ControlApi;
pub use error::RuntimeError;
pub use runtime::{Runtime, RuntimeParts};

/// The built-in tool every agent has for keeping its tile caption and
/// progress current (AW-4). It has no side effects and needs no policy.
pub const REPORT_PROGRESS_TOOL: &str = "report_progress";
