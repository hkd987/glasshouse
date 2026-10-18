//! `glassd.toml`: where things live, which backends to use, and the agent
//! catalog. Defaults are the production FreeBSD layout.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use glass_runtime::CatalogEntry;
use serde::Deserialize;

pub const DEFAULT_CONFIG: &str = "/usr/local/etc/glasshouse/glassd.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_control_socket")]
    pub control_socket: PathBuf,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    /// Defaults to `<state_dir>/audit/audit.log`.
    #[serde(default)]
    pub audit_log: Option<PathBuf>,
    /// Policy file. When it doesn't exist the built-in default policy is
    /// enforced (it is designed to be safe to ship unchanged).
    #[serde(default = "default_policy")]
    pub policy: PathBuf,
    #[serde(default = "default_max_live")]
    pub max_live_tasks_per_user: usize,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub snapshots: SnapshotConfig,
    #[serde(default, rename = "agent")]
    pub agents: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    #[serde(default)]
    pub backend: SandboxBackend,
    /// Must be true to run the development host backend, which contains
    /// nothing. Never set in a shipped config.
    #[serde(default)]
    pub allow_insecure: bool,
    /// Read-only base-system mounts for jails. Defaults to the platform's.
    #[serde(default)]
    pub base_mounts: Option<Vec<PathBuf>>,
    /// Directories holding agent programs, mounted read-only into sandboxes.
    #[serde(default = "default_agent_dirs")]
    pub agent_dirs: Vec<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            backend: SandboxBackend::default(),
            allow_insecure: false,
            base_mounts: None,
            agent_dirs: default_agent_dirs(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackend {
    #[default]
    FreebsdJail,
    HostInsecure,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotConfig {
    #[serde(default)]
    pub backend: SnapshotBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotBackend {
    #[default]
    Zfs,
    Copy,
}

fn default_control_socket() -> PathBuf {
    glass_api::DEFAULT_CONTROL_SOCKET.into()
}

fn default_state_dir() -> PathBuf {
    "/var/db/glasshouse".into()
}

fn default_policy() -> PathBuf {
    "/usr/local/etc/glasshouse/policy.toml".into()
}

fn default_max_live() -> usize {
    8
}

fn default_agent_dirs() -> Vec<PathBuf> {
    vec!["/usr/local/libexec/glasshouse".into()]
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn audit_log(&self) -> PathBuf {
        self.audit_log
            .clone()
            .unwrap_or_else(|| self.state_dir.join("audit/audit.log"))
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.sandbox.backend == SandboxBackend::HostInsecure && !self.sandbox.allow_insecure {
            bail!(
                "sandbox.backend = \"host-insecure\" runs agents with no containment; \
                 set sandbox.allow_insecure = true to use it for development"
            );
        }
        for agent in &self.agents {
            if !agent.program.is_absolute() {
                bail!("agent {}: program must be an absolute path", agent.id);
            }
            if self.sandbox.backend == SandboxBackend::FreebsdJail
                && !self
                    .sandbox
                    .agent_dirs
                    .iter()
                    .any(|d| agent.program.starts_with(d))
            {
                bail!(
                    "agent {}: {} is not under any sandbox.agent_dirs entry, so it would not exist inside the jail",
                    agent.id,
                    agent.program.display()
                );
            }
        }
        let mut ids: Vec<_> = self.agents.iter().map(|a| &a.id).collect();
        ids.sort();
        if let Some(pair) = ids.windows(2).find(|w| w[0] == w[1]) {
            bail!("agent {} is listed twice", pair[0]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_production_layout() {
        let config: Config = toml::from_str("").unwrap();
        config.validate().unwrap();
        assert_eq!(config.sandbox.backend, SandboxBackend::FreebsdJail);
        assert_eq!(config.snapshots.backend, SnapshotBackend::Zfs);
        assert_eq!(
            config.audit_log(),
            PathBuf::from("/var/db/glasshouse/audit/audit.log")
        );
    }

    #[test]
    fn insecure_backend_needs_explicit_opt_in() {
        let config: Config = toml::from_str("[sandbox]\nbackend = \"host-insecure\"\n").unwrap();
        assert!(config.validate().is_err());
        let config: Config =
            toml::from_str("[sandbox]\nbackend = \"host-insecure\"\nallow_insecure = true\n")
                .unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn jailed_agents_must_live_in_a_mounted_dir() {
        let text = r#"
            [[agent]]
            id = "x"
            name = "X"
            program = "/opt/x/agent"
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn shipped_config_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/freebsd/overlay/usr/local/etc/glasshouse/glassd.toml");
        Config::load(&path).unwrap();
    }
}
