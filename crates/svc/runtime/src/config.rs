use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glass_api::{AgentMode, AgentSummary, AppManifest, CustomizationPoint};
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
    /// For an attended agent, the program the person's client (`glassctl
    /// run --drive`, the command bar) runs as them to drive it, with the
    /// task socket in `GLASS_AGENT_SOCKET` (ADR 0042). glassd never starts
    /// it.
    #[serde(default)]
    pub driver: Option<PathBuf>,
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
        if self.driver.is_some() && !self.attended {
            return Err(format!(
                "agent {}: a driver is for attended agents only",
                self.id
            ));
        }
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
            driver: self.driver.as_ref().map(|d| d.display().to_string()),
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
    /// The apps installed on this machine, as their manifests declare them
    /// (UI-5, ADR 0024). Checked by glassd before they get here.
    pub apps: Vec<AppManifest>,
    /// The admin console this device is managed from (ADR 0023), which
    /// `console` in a profile opens; `None` when it isn't managed.
    pub console_url: Option<String>,
    /// Development only (the insecure host backend): every caller's home,
    /// where first-party apps keep their data, is this folder instead of
    /// the one their account names.
    pub dev_home: Option<PathBuf>,
    /// What the org's catalog lets reach each person (ADR 0023, DV-11):
    /// an item it makes *available* only once they install it. glassd's is
    /// the catalog app's; [`OpenCatalog`] lets everything through.
    pub org_catalog: Arc<dyn CatalogGate>,
    /// The apps people made for themselves (ADR 0032); glassd's are the
    /// studio's.
    pub personal: Arc<dyn PersonalApps>,
    /// Apps people made that IT approved, fetched and checked (ADR 0032);
    /// they reach people by the catalog's rules (DV-11).
    pub org_apps: Arc<dyn OrgApps>,
    /// Which kernel patches this computer takes (ADR 0057).
    pub hardware: crate::hwpatch::HardwarePatches,
}

/// Whether the org's catalog lets an agent or app reach a person, asked
/// before policy's answer is used. It only ever narrows: an id it doesn't
/// know reaches them.
pub trait CatalogGate: Send + Sync + std::fmt::Debug {
    /// For someone in `groups` whose home is `home`.
    fn reaches(&self, groups: &[String], home: &Path, id: &AgentId) -> bool;
    /// The ids the catalog lists: an org app it doesn't list reaches
    /// nobody, whatever `reaches` says of ids it doesn't know.
    fn lists(&self) -> std::collections::BTreeSet<AgentId> {
        std::collections::BTreeSet::new()
    }
}

/// The apps people make for themselves (ADR 0032), which glassd keeps:
/// each person sees and opens only their own.
pub trait PersonalApps: Send + Sync + std::fmt::Debug {
    /// `login`'s own apps at the versions they chose (in `home`).
    fn apps(&self, login: &str, home: &Path) -> Vec<PersonalApp>;
    /// What `login`'s `app` serves now: its version and files by name.
    fn files(
        &self,
        login: &str,
        home: &Path,
        app: &AgentId,
    ) -> Option<(u32, BTreeMap<String, String>)>;
}

/// One of a person's own apps, as they use it now.
#[derive(Debug, Clone)]
pub struct PersonalApp {
    /// Checked as it was saved; its id starts `my-`.
    pub manifest: AppManifest,
    /// The installed app it remixes. It opens only while that one is in
    /// the person's profile.
    pub based_on: Option<AgentId>,
}

/// Nobody has made any apps here.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPersonalApps;

impl PersonalApps for NoPersonalApps {
    fn apps(&self, _login: &str, _home: &Path) -> Vec<PersonalApp> {
        Vec::new()
    }

    fn files(
        &self,
        _login: &str,
        _home: &Path,
        _app: &AgentId,
    ) -> Option<(u32, BTreeMap<String, String>)> {
        None
    }
}

/// Apps people made that IT approved into the org's catalog (ADR 0032,
/// R2), as this computer fetched and checked them: glassd serves them from
/// here, never from a folder anyone can change.
pub trait OrgApps: Send + Sync + std::fmt::Debug {
    /// Every org app this computer holds a checked bundle of.
    fn apps(&self) -> Vec<AppManifest>;
    /// What `app` serves: its version and files by name.
    fn files(&self, app: &AgentId) -> Option<(String, BTreeMap<String, String>)>;
}

/// No org apps here.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOrgApps;

impl OrgApps for NoOrgApps {
    fn apps(&self) -> Vec<AppManifest> {
        Vec::new()
    }

    fn files(&self, _app: &AgentId) -> Option<(String, BTreeMap<String, String>)> {
        None
    }
}

/// No catalog: policy alone decides.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenCatalog;

impl CatalogGate for OpenCatalog {
    fn reaches(&self, _groups: &[String], _home: &Path, _id: &AgentId) -> bool {
        true
    }
}
