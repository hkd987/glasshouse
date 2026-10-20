use std::fmt;
use std::path::PathBuf;

use glass_api::{AskLabels, ChangeKind};
use glass_types::{ActionName, Label, SideEffect, Tier};
use serde_json::Value;

use crate::TaskContext;

/// An action as an app declares it (NV-1): name, typed input, side effects
/// and default approval tier.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionDef {
    pub name: ActionName,
    pub title: String,
    pub description: String,
    /// JSON Schema for the input object.
    pub input_schema: Value,
    pub side_effect: SideEffect,
    /// The tier when no policy rule matches.
    pub default_tier: Tier,
    /// How an ask for this action reads: "Review drafts", "Approve and
    /// send". Defaults to "Review" and "Approve".
    pub ask: AskLabels,
}

impl ActionDef {
    /// The same action with its own answer labels.
    pub fn asking(self, review: &str, approve: &str) -> Self {
        Self {
            ask: AskLabels::new(review, approve),
            ..self
        }
    }

    /// The MCP tool name. MCP clients and model APIs restrict tool names to
    /// letters, digits, `_` and `-`, so dots become underscores.
    pub fn mcp_name(&self) -> String {
        self.name.as_str().replace('.', "_")
    }
}

/// What a call would do, worked out before it runs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Preview {
    /// Verb and target in a few words, such as "Write Reports/summary.csv".
    /// Shown in the tile feed (AW-3) and approval prompts.
    pub summary: String,
    /// What the call touches, as the agent named it, for the audit record.
    pub targets: Vec<String>,
    /// The review list for an approval: one line per affected item (DA-12).
    pub items: Vec<String>,
    /// Host paths the call reads. The broker checks their labels.
    pub reads: Vec<PathBuf>,
    /// Host paths the call changes. The broker checks their labels.
    pub writes: Vec<PathBuf>,
    /// The label of what the call reads that isn't a file, such as an item
    /// in the shared space (ADR 0034 §5): checked against what the agent
    /// may open, and raising the task's label, as a file's does.
    pub label: Option<Label>,
    /// Existing directories to snapshot before the call runs, so it can be
    /// undone (AU-20).
    pub snapshot_roots: Vec<PathBuf>,
}

/// One thing a completed call changed, for the Changes tab (AW-13).
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: ChangeKind,
    /// What changed, as the user would name it.
    pub target: String,
    pub reversible: bool,
    /// Host files the call wrote, so this one change can be undone on its
    /// own.
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionOutput {
    /// A short result for the agent (and its model) to read.
    pub text: String,
    pub data: Value,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The call reaches outside what the task was granted. Always audited.
    #[error("outside this task's scope: {0}")]
    OutOfScope(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Failed(String),
}

impl ActionError {
    /// An I/O error, named after what was being done.
    pub fn io(context: impl fmt::Display, e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound(context.to_string()),
            // cap-std reports escapes as permission errors.
            std::io::ErrorKind::PermissionDenied => Self::OutOfScope(context.to_string()),
            _ => Self::Failed(format!("{context}: {e}")),
        }
    }
}

/// An app that declares actions.
///
/// Methods block (file I/O); the runtime calls them from blocking tasks.
pub trait ActionProvider: Send + Sync + fmt::Debug {
    fn actions(&self) -> Vec<ActionDef>;

    /// Prepares per-user app state before a task starts, such as creating the
    /// mail folders, so later snapshots have something to cover.
    fn prepare(&self, _ctx: &TaskContext) -> Result<(), ActionError> {
        Ok(())
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError>;

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError>;
}

/// Deserializes an action's input, mapping errors to `InvalidInput`.
pub fn parse_input<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T, ActionError> {
    let input = if input.is_null() {
        Value::Object(Default::default())
    } else {
        input.clone()
    };
    serde_json::from_value(input).map_err(|e| ActionError::InvalidInput(e.to_string()))
}
