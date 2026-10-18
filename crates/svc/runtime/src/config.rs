use std::path::PathBuf;

use glass_api::{AgentMode, AgentSummary, CustomizationPoint};
use glass_types::AgentId;
use serde::Deserialize;

/// One agent IT published to the fleet (AD-5): how to run it and what to say
/// about it. What it may do is policy's business, not the catalog's.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub id: AgentId,
    pub name: String,
    /// Published version, shown on handoff cards and in the catalog.
    #[serde(default)]
    pub version: String,
    /// The policy model id this agent prefers (ADR 0018). The handoff card
    /// shows where policy routes the task's data, preferring this one when
    /// allowed. For an agent that can't call the gateway, such as a harness
    /// with its own provider (HX-7), the text is shown as is. Empty for
    /// agents that use none.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_mode")]
    pub mode: AgentMode,
    /// The agent program, as a host path that is also visible inside the
    /// sandbox (its directory must be in [`RuntimeConfig::agent_dirs`]).
    /// Unused for attended agents.
    #[serde(default)]
    pub program: PathBuf,
    /// The user's own harness drives this agent through `glass-mcp`;
    /// glassd starts no program and no sandbox, only the task socket
    /// (HX-1). Every call still goes through the action pipeline.
    #[serde(default)]
    pub attended: bool,
    #[serde(default)]
    pub args: Vec<String>,
    /// The plan shown on the handoff card, until model-backed planning
    /// produces one per task.
    #[serde(default)]
    pub plan: Vec<String>,
    /// What users may change with a layer (TL-1); everything else is locked.
    #[serde(default, rename = "customize")]
    pub customize: Vec<CustomizationPoint>,
}

fn default_mode() -> AgentMode {
    AgentMode::Actions
}

impl CatalogEntry {
    /// Checks what the entry declares, for config validation.
    pub fn check(&self) -> Result<(), String> {
        glass_tailoring::check_declared(&self.customize)
    }

    pub fn summary(&self) -> AgentSummary {
        AgentSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            mode: self.mode,
            attended: self.attended,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Task logs, sandbox roots and development snapshots live here.
    pub state_dir: PathBuf,
    pub catalog: Vec<CatalogEntry>,
    /// Host directories mounted read-only at the same path in every sandbox,
    /// so agent programs can run there.
    pub agent_dirs: Vec<PathBuf>,
    /// Live tasks one user may have at once.
    pub max_live_tasks_per_user: usize,
    /// The attended catalog agent that Take over on an ended task continues
    /// in (`task.continue`), such as Claude Code. `None` turns it off.
    pub harness: Option<AgentId>,
    /// Where attended tasks' sockets live: a root-owned directory users can
    /// traverse; each socket is its user's alone.
    pub attended_dir: PathBuf,
}
