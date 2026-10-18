//! App actions: the typed operations apps declare to the OS (NV-1), and the
//! registry the broker dispatches them through.
//!
//! An action runs in two steps. [`ActionProvider::preview`] works out, with no
//! side effects, what a call would touch; the broker uses that to decide the
//! tier, check labels, snapshot and record the audit entry. Only then does
//! [`ActionProvider::invoke`] run it. Providers never decide permissions.
//!
//! Agents address files as `<grant>/<path>`, such as `Invoices/inv-001.txt`,
//! where the grant is a folder the task was given. Host paths never reach the
//! agent.

mod context;
mod def;
pub mod providers;
mod registry;

pub use context::{Grant, Resolved, TaskContext};
pub use def::{ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview};
pub use registry::{Registry, RegistryError};
