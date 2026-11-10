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
//!
//! This crate is the contract only. Each app's actions live in their own
//! crate under `crates/apps/` (`glass-app-files`, `glass-app-mail`, …),
//! which depends on this one and on lib crates, never on another app, so
//! apps build in parallel and teams can own them separately.

mod context;
mod def;
mod live;
mod registry;
pub mod shape;
pub mod testing;

use std::path::Path;

pub use context::{Grant, MAX_PATH_BYTES, MAX_PATH_DEPTH, Resolved, TaskContext};
pub use def::{ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, parse_input};
pub use live::{Live, Pulled};
pub use registry::{Registry, RegistryError};

/// The key in an [`ActionOutput`]'s `data` whose value the runtime moves
/// into the call's audit record, under the app's name (`detail.mail`),
/// instead of handing it to the caller: what the record must keep and the
/// result needn't carry, such as a sent message's SHA-256 and the server's
/// reply (AU-1, ADR 0037). Never content, never a credential.
pub const AUDIT_DETAIL: &str = "_audit";

/// Tags a file the agent changed with "by [agent] for [user]" (AU-10).
/// Best effort: a filesystem without extended attributes still gets the
/// change, and the audit log still attributes it.
pub fn attribute(ctx: &TaskContext, path: &Path) {
    if let Err(e) = glass_platform::xattr::write_attribution(path, &ctx.principal.attribution()) {
        tracing::debug!(path = %path.display(), error = %e, "could not record attribution");
    }
}
