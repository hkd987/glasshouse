//! The policy document IT writes, as stored on disk (`policy.toml`).
//!
//! These types mirror the file one to one. Validation and evaluation live in
//! [`crate::Policy`]; nothing here has behavior beyond parsing.

use std::collections::BTreeMap;

use glass_types::{AgentId, Label, Tier};
use serde::{Deserialize, Serialize};

use crate::ActionPattern;

/// The current (and only) policy schema version.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    pub version: u32,
    #[serde(default)]
    pub org: OrgDefaults,
    #[serde(default, rename = "group")]
    pub groups: Vec<GroupPolicy>,
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentPolicy>,
    /// Folder rules that label data by location (DP-10).
    #[serde(default, rename = "label")]
    pub labels: Vec<FolderLabel>,
}

/// The org-wide baseline every group and agent rule overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgDefaults {
    /// Highest data label any agent may open unless a rule lowers it.
    #[serde(default = "default_max_label")]
    pub max_label: Label,
    /// How long an unanswered Ask holds the agent before the task pauses
    /// (DA-13).
    #[serde(default = "default_approval_timeout_secs")]
    pub approval_timeout_secs: u64,
    /// Baseline tier per action pattern. Actions with no matching rule use
    /// the default tier their app declared.
    #[serde(default)]
    pub tiers: BTreeMap<ActionPattern, Tier>,
}

impl Default for OrgDefaults {
    fn default() -> Self {
        Self {
            max_label: default_max_label(),
            approval_timeout_secs: default_approval_timeout_secs(),
            tiers: BTreeMap::new(),
        }
    }
}

fn default_max_label() -> Label {
    Label::Internal
}

fn default_approval_timeout_secs() -> u64 {
    15 * 60
}

/// Rules for a directory group. In managed fleets groups and memberships are
/// synced from Entra ID or Okta via SCIM (AD-1); `members` is the local
/// fallback used before enrollment and in tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupPolicy {
    pub name: String,
    /// Login names. The special group name `everyone` matches all users.
    #[serde(default)]
    pub members: Vec<String>,
    /// Agents members of this group may run.
    #[serde(default)]
    pub agents: Vec<AgentId>,
    #[serde(default)]
    pub max_label: Option<Label>,
    #[serde(default)]
    pub tiers: BTreeMap<ActionPattern, Tier>,
}

/// Rules for one catalog agent (AD-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicy {
    pub id: AgentId,
    /// Actions this agent may call at all. Anything else is Never and shows
    /// under "Can't touch".
    pub allowed_actions: Vec<ActionPattern>,
    #[serde(default)]
    pub tiers: BTreeMap<ActionPattern, Tier>,
    #[serde(default)]
    pub max_label: Option<Label>,
    /// Network destinations (`host:port`) the agent's sandbox may reach.
    /// Empty means no network at all.
    #[serde(default)]
    pub egress: Vec<String>,
}

/// Labels everything under a path (DP-10). `*` matches exactly one path
/// segment, so `/home/*/Finance` covers every user's Finance folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderLabel {
    pub path: String,
    pub label: Label,
}
