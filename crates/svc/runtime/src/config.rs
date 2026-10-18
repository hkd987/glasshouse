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
    /// The model the agent works with, shown under "Data and model" on the
    /// handoff card. Empty for agents that use none.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_mode")]
    pub mode: AgentMode,
    /// The agent program, as a host path that is also visible inside the
    /// sandbox (its directory must be in [`RuntimeConfig::agent_dirs`]).
    pub program: PathBuf,
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
}
