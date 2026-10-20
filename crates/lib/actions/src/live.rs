//! Live editing (ADR 0035 §3): what an app session pushes to and pulls
//! from a shared item it opened with an action, outside the pipeline, so
//! typing isn't an audited call per keystroke. The session's close is
//! audited once, with what it changed.

use glass_types::TaskId;
use serde::Serialize;
use serde_json::Value;

use crate::{ActionError, TaskContext};

/// New updates for an item, after a cursor.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Pulled {
    /// Each `{ update, by, by_name }`.
    pub updates: Vec<Value>,
    /// Ask again with this to hear of the next.
    pub cursor: u64,
}

/// A provider of live edits: one per app kind (Docs, Sheets).
pub trait Live: Send + Sync + std::fmt::Debug {
    /// The app whose sessions edit live, whose wakes a pull waits for.
    fn app(&self) -> &'static str;
    /// Seals and posts `update` (base64) to `item`, which the session in
    /// `ctx` opened and may edit. The new cursor.
    fn push(&self, ctx: &TaskContext, item: &str, update: &str) -> Result<u64, ActionError>;
    /// What others changed in `item` after `since`. Doesn't wait.
    fn pull(&self, ctx: &TaskContext, item: &str, since: u64) -> Result<Pulled, ActionError>;
    /// Session `session` ended: one line per item it changed, such as
    /// "Q3 plan: 340 changes in 14 minutes", for its audit record.
    fn closed(&self, session: &TaskId) -> Vec<String>;
}
