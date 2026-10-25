//! Core domain vocabulary shared by every Glasshouse component.
//!
//! This crate sits at the bottom of the dependency graph. It holds plain
//! types with validation and no I/O, so every daemon, tool and the shell agree
//! on what a task, an agent, a tier or a label is.

mod action;
mod digest;
mod ids;
mod label;
mod principal;
mod tier;
mod timestamp;

pub use action::{ActionName, InvalidActionName, SideEffect};
pub use digest::FileDigest;
pub use ids::{
    AgentId, ApprovalId, InvalidAgentId, InvalidId, PERSONAL_PREFIX, SnapshotId, TaskId,
};
pub use label::Label;
pub use principal::{AgentPrincipal, UserRef};
pub use tier::Tier;
pub use timestamp::Timestamp;
