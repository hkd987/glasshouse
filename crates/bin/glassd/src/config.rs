//! `glassd.toml`: where things live, which backends to use, and the agent
//! catalog. Defaults are the production FreeBSD layout.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use glass_platform::sandbox::ResourceLimits;
use glass_runtime::CatalogEntry;
use glass_types::AgentId;
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
    /// The attended agent that Take over on an ended task continues in,
    /// such as `claude-code` (HX-1). Unset turns that off.
    #[serde(default)]
    pub harness: Option<AgentId>,
    /// Credentials for policy's model providers: the stand-in for the
    /// secrets broker (MP-4, ADR 0018). Read only if it is a regular file
    /// owned by glassd's user that nobody else can read. Keep it outside
    /// every directory mounted into a jail.
    #[serde(default = "default_model_secrets")]
    pub model_secrets: PathBuf,
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
    /// What each task may use, so the person's own work comes first.
    #[serde(default)]
    pub limits: LimitsConfig,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            backend: SandboxBackend::default(),
            allow_insecure: false,
            base_mounts: None,
            agent_dirs: default_agent_dirs(),
            limits: LimitsConfig::default(),
        }
    }
}

/// `[sandbox.limits]`: agents run below the person's CPU priority, and each
/// task's jail gets these limits for as long as it exists (PA-2). Shares
/// are of this machine, so one config fits every model; 0 turns a limit
/// off. See `glass_platform::sandbox::ResourceLimits` and
/// docs/dev/performance.md ("Under load").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    /// Niceness agents start with: 0 is the person's, 20 the lowest.
    pub nice: u8,
    /// CPU per task, in percent of the whole machine.
    pub cpu_percent: u64,
    /// Memory per task, in percent of physical memory.
    pub memory_percent: u64,
    /// Disk throughput per task, in MiB per second.
    pub read_mb_per_sec: u64,
    pub write_mb_per_sec: u64,
    /// Disk operations per task, per second.
    pub read_iops: u64,
    pub write_iops: u64,
    /// Processes per task at once.
    pub max_processes: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            nice: 10,
            cpu_percent: 50,
            memory_percent: 25,
            read_mb_per_sec: 64,
            write_mb_per_sec: 32,
            read_iops: 0,
            write_iops: 0,
            max_processes: 256,
        }
    }
}

impl LimitsConfig {
    /// The limits for a machine with `cpus` CPUs and `memory` bytes of
    /// physical memory (0 when unknown, which leaves memory unlimited).
    pub fn resolve(&self, cpus: u64, memory: u64) -> ResourceLimits {
        const MIB: u64 = 1 << 20;
        ResourceLimits {
            nice: self.nice,
            // rctl's pcpu counts one CPU as 100.
            cpu_percent: self.cpu_percent * cpus,
            memory_bytes: memory.saturating_mul(self.memory_percent) / 100,
            read_bps: self.read_mb_per_sec.saturating_mul(MIB),
            write_bps: self.write_mb_per_sec.saturating_mul(MIB),
            read_iops: self.read_iops,
            write_iops: self.write_iops,
            max_processes: self.max_processes,
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.nice > 20 {
            bail!("sandbox.limits.nice runs from 0 (the person's priority) to 20");
        }
        for (name, value) in [
            ("cpu_percent", self.cpu_percent),
            ("memory_percent", self.memory_percent),
        ] {
            if value > 100 {
                bail!("sandbox.limits.{name} is a share of this machine: 0 (off) to 100");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackend {
    /// One jail per task: the only real boundary.
    #[default]
    #[serde(alias = "freebsd-jail")]
    Jail,
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

fn default_model_secrets() -> PathBuf {
    "/usr/local/etc/glasshouse/model-secrets.toml".into()
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
        self.sandbox.limits.validate()?;
        for agent in &self.agents {
            agent
                .check()
                .map_err(|e| anyhow::anyhow!("agent {}: {e}", agent.id))?;
            if agent.attended {
                // The user's own harness drives it; there's no program.
                continue;
            }
            if !agent.program.is_absolute() {
                bail!("agent {}: program must be an absolute path", agent.id);
            }
            if self.sandbox.backend == SandboxBackend::Jail
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
        // Every jail sees the base mounts and the agent directories.
        let base = self
            .sandbox
            .base_mounts
            .clone()
            .unwrap_or_else(|| glass_platform::sandbox::JailConfig::default().base_mounts);
        if base
            .iter()
            .chain(&self.sandbox.agent_dirs)
            .any(|dir| self.model_secrets.starts_with(dir))
        {
            bail!(
                "model_secrets {} is inside a directory mounted into every jail",
                self.model_secrets.display()
            );
        }
        if let Some(harness) = &self.harness
            && !self.agents.iter().any(|a| &a.id == harness && a.attended)
        {
            bail!("harness {harness} must be an attended agent in the catalog");
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
        assert_eq!(config.sandbox.backend, SandboxBackend::Jail);
        assert_eq!(config.snapshots.backend, SnapshotBackend::Zfs);
        assert_eq!(
            config.audit_log(),
            PathBuf::from("/var/db/glasshouse/audit/audit.log")
        );
        assert_eq!(
            config.model_secrets,
            PathBuf::from("/usr/local/etc/glasshouse/model-secrets.toml")
        );
    }

    #[test]
    fn mp4_model_secrets_stay_out_of_jails() {
        for inside in [
            "/usr/local/libexec/glasshouse/keys.toml",
            "/usr/share/keys.toml",
        ] {
            let config: Config =
                toml::from_str(&format!("model_secrets = \"{inside}\"\n")).unwrap();
            assert!(config.validate().is_err(), "{inside}");
        }
    }

    #[test]
    fn the_old_backend_name_still_parses() {
        let config: Config = toml::from_str("[sandbox]\nbackend = \"freebsd-jail\"\n").unwrap();
        assert_eq!(config.sandbox.backend, SandboxBackend::Jail);
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
    fn the_harness_must_be_an_attended_catalog_agent() {
        let text = r#"
            harness = "claude-code"
            [[agent]]
            id = "claude-code"
            name = "Claude Code"
        "#;
        let config: Config = toml::from_str(text).unwrap();
        assert!(config.validate().is_err(), "not attended");
        let config: Config = toml::from_str(&text.replace(
            "name = \"Claude Code\"",
            "name = \"Claude Code\"\nattended = true",
        ))
        .unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn agents_get_a_share_of_this_machine() {
        let limits = LimitsConfig::default().resolve(4, 16 << 30);
        assert_eq!(limits.nice, 10);
        assert_eq!(limits.cpu_percent, 200, "half of four CPUs");
        assert_eq!(limits.memory_bytes, 4 << 30, "a quarter of 16 GiB");
        assert_eq!(limits.write_bps, 32 << 20);
        assert_eq!(limits.max_processes, 256);
        assert_eq!(
            LimitsConfig::default().resolve(4, 0).memory_bytes,
            0,
            "unknown memory: no memory limit rather than a wrong one"
        );
    }

    #[test]
    fn limits_must_make_sense() {
        let bad = |text: &str| {
            let config: Config = toml::from_str(text).unwrap();
            assert!(config.validate().is_err(), "{text}");
        };
        bad("[sandbox.limits]\nnice = 21\n");
        bad("[sandbox.limits]\ncpu_percent = 150\n");
        bad("[sandbox.limits]\nmemory_percent = 101\n");
        let off: Config = toml::from_str("[sandbox.limits]\nnice = 0\ncpu_percent = 0\nmemory_percent = 0\nread_mb_per_sec = 0\nwrite_mb_per_sec = 0\nmax_processes = 0\n").unwrap();
        off.validate().unwrap();
        assert_eq!(
            off.sandbox.limits.resolve(4, 1 << 30),
            ResourceLimits::default()
        );
    }

    #[test]
    fn shipped_config_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/freebsd/overlay/usr/local/etc/glasshouse/glassd.toml");
        Config::load(&path).unwrap();
    }
}
