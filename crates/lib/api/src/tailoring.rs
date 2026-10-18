//! Tailoring catalog agents with personal layers (PRD "Tailoring catalog
//! items"). A layer is saved on top of the published agent rather than as a
//! copy, so the publisher's updates keep coming and the change rides along.

use std::collections::BTreeMap;

use glass_types::{AgentId, Tier, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AgentSummary;

/// A setting the publisher lets users change, declared in the agent's
/// catalog entry (TL-1). Everything not declared is locked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomizationPoint {
    pub key: String,
    /// Shown to the user, such as "Flag invoices over this amount".
    pub title: String,
    pub kind: PointKind,
    /// For [`PointKind::Choice`]: the allowed values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// The value when no layer sets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointKind {
    /// One line of text.
    Text,
    Number,
    /// On or off.
    Flag,
    /// One of `choices`.
    Choice,
    /// Extra instructions in the user's words, added to every task.
    Instructions,
}

/// `layer.get`, and the agent for `layer.revert`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerRef {
    pub agent: AgentId,
}

/// `layer.set`: the whole content of a new version of the caller's layer
/// (TL-2). Fails, saving nothing, if any check fails.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerChange {
    pub agent: AgentId,
    /// Values for customization points, by key. Keys left out use the
    /// published default.
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    /// Extra limits on every task with this agent, as on the handoff card.
    /// Can only make tiers stricter (TL-10).
    #[serde(default)]
    pub restrict: Vec<(String, Tier)>,
    /// What the user asked for, in their words.
    #[serde(default)]
    pub note: String,
}

/// `layer.revert`: switch to an earlier version, or turn the layer off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerRevert {
    pub agent: AgentId,
    /// `None` runs the published agent with no layer.
    #[serde(default)]
    pub to: Option<u32>,
}

/// The Tailor panel for one agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerView {
    pub agent: AgentSummary,
    /// What can be changed (TL-1).
    pub points: Vec<CustomizationPoint>,
    pub status: LayerStatus,
    /// The version in use, if any.
    #[serde(default)]
    pub active: Option<u32>,
    /// Every version, newest first (TL-2).
    pub history: Vec<LayerVersion>,
    /// What the active version passed before it turned on.
    #[serde(default)]
    pub checks: Vec<LayerCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LayerStatus {
    /// No layer: tasks run the published agent.
    Off,
    On,
    /// The agent changed and the layer no longer fits it (TL-13). Tasks run
    /// the published agent until the user changes or reverts the layer.
    Paused {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerVersion {
    pub version: u32,
    pub at: Timestamp,
    #[serde(default)]
    pub note: String,
    pub settings: BTreeMap<String, Value>,
    #[serde(default)]
    pub restrict: Vec<(String, Tier)>,
    /// Plain-language lines, such as "Flag invoices over: 1000".
    #[serde(default)]
    pub summary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerCheck {
    pub name: String,
    pub result: CheckResult,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    Passed,
    Failed,
    /// The check doesn't exist for this agent yet.
    NotAvailable,
}

/// The layer line on a handoff card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerLine {
    pub version: u32,
    pub summary: Vec<String>,
    /// Set when the layer is paused and the task runs the published agent.
    #[serde(default)]
    pub paused: Option<String>,
}
