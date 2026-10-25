//! `glassd.toml`: where things live, which backends to use, and the agent
//! catalog. Defaults are the production FreeBSD layout.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use glass_actions::Registry;
use glass_api::AppManifest;
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
    /// Where app runtimes reach glassd (UI-5, ADR 0024). Authorized by uid,
    /// like the control socket, and carrying only the app methods.
    #[serde(default = "default_app_socket")]
    pub app_socket: PathBuf,
    /// Installed apps, one folder each, with the `manifest.json` glassd
    /// enforces (DV-3). Read-only system files: an app never writes its
    /// own manifest.
    #[serde(default = "default_apps_dir")]
    pub apps_dir: PathBuf,
    /// Development only, with the host backend: every user's app data
    /// (`~/Mail`, `~/Calendar`) lives in this folder instead of their home.
    #[serde(default)]
    pub dev_home: Option<PathBuf>,
    /// The browser engine's policy folder, where glassd writes the
    /// browser's settings from policy as `managed/org.json` and
    /// `recommended/org.json` (UX-1, ADR 0029). Unset writes none.
    #[serde(default)]
    pub browser_policies: Option<PathBuf>,
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
    /// The org's admin console, once the device is enrolled (ADR 0023).
    /// Unset: the device isn't managed and enforces its policy file only.
    #[serde(default)]
    pub managed: Option<ManagedConfig>,
    #[serde(default)]
    pub studio: StudioConfig,
    /// Which kernel patches this computer takes (ADR 0057).
    #[serde(default)]
    pub hardware: HardwareConfig,
    /// Where wpa_supplicant listens, and the organization's Wi-Fi
    /// passwords (CF-12, ADR 0059).
    #[serde(default)]
    pub wifi: WifiConfig,
    /// The screen's backlight (ADR 0060).
    #[serde(default)]
    pub display: DisplayConfig,
    /// The OS updates this computer takes (ADR 0061).
    #[serde(default)]
    pub update: UpdateConfig,
    #[serde(default, rename = "agent")]
    pub agents: Vec<CatalogEntry>,
}

/// `[hardware]`: who may sign the kernel patches this computer takes,
/// whether it takes ones nobody reviewed, and how long a patch is on
/// probation (ADR 0057). With no keys, it takes none.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareConfig {
    /// Reviewers' keys: they say whether a patch was reviewed.
    #[serde(default)]
    pub keys: Vec<glass_managed::PublicKey>,
    /// Keys that sign only patches nobody reviewed, such as the build
    /// service's (ADR 0057).
    #[serde(default)]
    pub community_keys: Vec<glass_managed::PublicKey>,
    #[serde(default)]
    pub community: bool,
    /// Minutes of use on a patch before it's kept: until then a crash goes
    /// back to the boot environment before. A working day by default.
    #[serde(default = "default_probation")]
    pub probation_minutes: u64,
    /// Wakes from sleep on a patch before it's kept, as well, on a
    /// computer that sleeps: any kernel patch can break waking.
    #[serde(default = "default_resumes")]
    pub probation_resumes: u64,
    /// The build service agents' patches are built by (slice 2), and the
    /// file with its token, root's, mode 600. Unset: agents build none.
    #[serde(default)]
    pub build_service: Option<String>,
    #[serde(default = "default_build_token")]
    pub build_token: PathBuf,
    /// Plain HTTP to this machine, for development (a tunnel to the
    /// host's service).
    #[serde(default)]
    pub build_allow_insecure_http: bool,
}

fn default_build_token() -> PathBuf {
    "/usr/local/etc/glasshouse/kbuild-token".into()
}

impl Default for HardwareConfig {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            community_keys: Vec::new(),
            community: false,
            probation_minutes: default_probation(),
            probation_resumes: default_resumes(),
            build_service: None,
            build_token: default_build_token(),
            build_allow_insecure_http: false,
        }
    }
}

fn default_resumes() -> u64 {
    1
}

fn default_probation() -> u64 {
    8 * 60
}

/// `[update]`: the root keys the image trusts for OS updates, the channel
/// the computer follows, and the folder releases are found in (ADR 0061).
/// With no root keys it takes no update.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateConfig {
    #[serde(default)]
    pub roots: Vec<glass_managed::PublicKey>,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default)]
    pub source: Option<PathBuf>,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            channel: default_channel(),
            source: None,
        }
    }
}

fn default_channel() -> String {
    "broad".into()
}

/// `[display]`: `backlight`, a file holding a percent that stands in for
/// the screen's backlight, on a computer with none to dim (development,
/// the apps' tests). Unset: the computer's own, if it has one.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    #[serde(default)]
    pub backlight: Option<PathBuf>,
}

/// `[wifi]`: the radio's wpa_supplicant control socket, and the
/// organization's Wi-Fi passwords: root-owned, mode 600, or glassd refuses
/// them, as it does model secrets. Not shipped; IT or its tooling writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiConfig {
    #[serde(default = "default_wifi_control")]
    pub control_dir: PathBuf,
    #[serde(default = "default_wifi_interface")]
    pub interface: String,
    #[serde(default = "default_wifi_secrets")]
    pub secrets: PathBuf,
}

impl Default for WifiConfig {
    fn default() -> Self {
        Self {
            control_dir: default_wifi_control(),
            interface: default_wifi_interface(),
            secrets: default_wifi_secrets(),
        }
    }
}

fn default_wifi_control() -> PathBuf {
    glass_platform::wifi::CONTROL_DIR.into()
}

fn default_wifi_interface() -> String {
    glass_platform::wifi::INTERFACE.into()
}

fn default_wifi_secrets() -> PathBuf {
    "/usr/local/etc/glasshouse/wifi-secrets.toml".into()
}

/// `[studio]`: what remixing and making apps needs (ADR 0032). glassd
/// keeps the apps people make in `<state_dir>/apps`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudioConfig {
    /// esbuild: one statically linked program, alone in its folder, which
    /// a jail mounts read-only to bundle in.
    #[serde(default = "default_bundler")]
    pub bundler: PathBuf,
    /// glass-app-try, which tries each version saved (ADR 0049): statically
    /// linked and alone in its folder, run in the bundler's jail. Missing,
    /// and nothing is tried.
    #[serde(default = "default_tryout")]
    pub tryout: PathBuf,
    /// Our apps' source, which people remix: `apps/<app>/src` and `sdk/`.
    /// A folder of its own: glassd removes it when IT turns remixing off
    /// (TL-6), and the next update brings it back.
    #[serde(default = "default_source")]
    pub source: PathBuf,
}

impl Default for StudioConfig {
    fn default() -> Self {
        Self {
            bundler: default_bundler(),
            tryout: default_tryout(),
            source: default_source(),
        }
    }
}

fn default_bundler() -> PathBuf {
    "/usr/local/lib/glasshouse/bundler/esbuild".into()
}

fn default_tryout() -> PathBuf {
    "/usr/local/lib/glasshouse/app-try/glass-app-try".into()
}

fn default_source() -> PathBuf {
    "/usr/local/share/glasshouse/source".into()
}

/// `[managed]`: where this device gets its org's policy and catalog, and
/// whose signatures it accepts (ADR 0023). Written at enrollment.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedConfig {
    /// The console's address, `https://...`.
    pub url: String,
    /// The org's id; a release for another org is refused.
    pub org: String,
    /// The org's public keys (`ed25519:<hex>`). Releases signed by any
    /// other key are refused; several allow a key rotation (EP-12).
    pub org_keys: Vec<glass_managed::PublicKey>,
    /// The enrollment token, in a file only glassd's user can read.
    pub token_file: PathBuf,
    /// This device's id in the fleet. Defaults to the machine's name.
    #[serde(default)]
    pub device: Option<String>,
    /// Plain HTTP to a loopback console, for development and tests only.
    #[serde(default)]
    pub allow_insecure_http: bool,
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

fn default_app_socket() -> PathBuf {
    glass_api::DEFAULT_APP_SOCKET.into()
}

fn default_apps_dir() -> PathBuf {
    "/usr/local/share/glasshouse/apps".into()
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

    /// The installed apps whose manifests check out (UI-5, DV-3), and why
    /// any other wasn't installed. One bad app never stops glassd: it just
    /// isn't available.
    pub fn load_apps(&self, registry: &Registry) -> (Vec<AppManifest>, Vec<String>) {
        let mut apps = Vec::new();
        let mut skipped = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.apps_dir) else {
            return (apps, skipped);
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("manifest.json").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            match self.load_app(&dir, registry) {
                Ok(manifest) if apps.iter().any(|a: &AppManifest| a.id == manifest.id) => {
                    skipped.push(format!(
                        "{}: app {} is installed twice",
                        dir.display(),
                        manifest.id
                    ));
                }
                Ok(manifest) => apps.push(manifest),
                Err(e) => skipped.push(format!("{}: {e:#}", dir.display())),
            }
        }
        (apps, skipped)
    }

    fn load_app(&self, dir: &Path, registry: &Registry) -> anyhow::Result<AppManifest> {
        let path = dir.join("manifest.json");
        let meta = std::fs::metadata(&path)?;
        if meta.len() > 64 * 1024 {
            bail!("manifest.json is larger than 64 KiB");
        }
        let manifest: AppManifest = serde_json::from_str(&std::fs::read_to_string(&path)?)
            .context("manifest.json doesn't parse")?;
        manifest.check().map_err(anyhow::Error::msg)?;
        if dir.file_name().and_then(|n| n.to_str()) != Some(manifest.id.as_str()) {
            bail!(
                "the app's folder must be named after its id, {}",
                manifest.id
            );
        }
        if manifest.id.is_personal() {
            bail!(
                "{}: ids that start {} are people's own apps, kept by glassd, never installed",
                manifest.id,
                glass_types::PERSONAL_PREFIX
            );
        }
        // Apps and agents share one namespace: both act "by [id] for [user]".
        if self.agents.iter().any(|a| a.id == manifest.id) {
            bail!("{} is already an agent's id", manifest.id);
        }
        if let Some(unknown) = manifest.actions.iter().find(|a| registry.get(a).is_none()) {
            bail!("declares {unknown}, which no installed provider offers");
        }
        Ok(manifest)
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
        if self.dev_home.is_some() && self.sandbox.backend != SandboxBackend::HostInsecure {
            bail!(
                "dev_home moves every user's app data to one folder; it is for the \
                 development host backend only"
            );
        }
        if self.app_socket == self.control_socket {
            bail!("app_socket and control_socket must be different sockets");
        }
        // A folder name under the update source, nothing else (ADR 0061).
        if self.update.channel.is_empty()
            || !self
                .update
                .channel
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            bail!(
                "update.channel {:?}: lowercase letters, digits and -",
                self.update.channel
            );
        }
        if let Some(managed) = &self.managed {
            if managed.org_keys.is_empty() {
                bail!("managed.org_keys: a managed device needs its org's public key");
            }
            if managed.org.trim().is_empty() {
                bail!("managed.org: name the org this device is enrolled in");
            }
            if managed.allow_insecure_http && self.sandbox.backend != SandboxBackend::HostInsecure {
                bail!("managed.allow_insecure_http is for the development host backend only");
            }
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
    fn dev_home_is_for_the_development_backend_only() {
        let config: Config = toml::from_str("dev_home = \"/tmp/home\"\n").unwrap();
        assert!(config.validate().is_err(), "not with jails");
        let config: Config = toml::from_str(
            "dev_home = \"/tmp/home\"\n[sandbox]\nbackend = \"host-insecure\"\nallow_insecure = true\n",
        )
        .unwrap();
        config.validate().unwrap();
    }

    fn registry() -> Registry {
        glass_first_party::registry(glass_first_party::Setup::offline()).unwrap()
    }

    #[test]
    fn dv3_apps_install_only_with_a_manifest_that_checks_out() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |dir: &str, manifest: &str| {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
            std::fs::write(tmp.path().join(dir).join("manifest.json"), manifest).unwrap();
        };
        let mail = r#"{"id": "mail", "name": "Mail", "version": "0.1.0",
            "actions": ["mail.list", "mail.send"], "labels": ["internal"]}"#;
        write("mail", mail);
        write("misnamed", mail);
        write(
            "calendar",
            r#"{"id": "calendar", "name": "Calendar", "version": "1",
            "actions": ["calendar.list", "payments.send"], "labels": ["internal"]}"#,
        );
        write(
            "notes",
            r#"{"id": "notes", "name": "Notes", "version": "1",
            "actions": ["files.read"], "labels": ["restricted"]}"#,
        );
        write(
            "invoice-demo",
            r#"{"id": "invoice-demo", "name": "Invoices", "version": "1",
            "actions": ["files.read"], "labels": ["internal"]}"#,
        );
        write("broken", "{");
        let mut config: Config = toml::from_str(
            "[[agent]]\nid = \"invoice-demo\"\nname = \"Invoice demo\"\nattended = true\n",
        )
        .unwrap();
        config.apps_dir = tmp.path().to_owned();
        let (apps, skipped) = config.load_apps(&registry());
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["mail"]);
        let why = skipped.join("\n");
        for reason in [
            "no installed provider offers",
            "named after its id",
            "restricted",
            "already an agent's id",
            "doesn't parse",
        ] {
            assert!(why.contains(reason), "{reason}: {why}");
        }
        // No apps folder, no apps, and glassd still starts.
        config.apps_dir = tmp.path().join("missing");
        assert!(config.load_apps(&registry()).0.is_empty());
    }

    #[test]
    fn the_first_party_apps_manifests_install() {
        // apps/<id>/manifest.json, as the image installs them.
        let mut config: Config = toml::from_str("").unwrap();
        config.apps_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
        let (apps, skipped) = config.load_apps(&registry());
        assert!(skipped.is_empty(), "{skipped:?}");
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "artifacts",
                "calendar",
                "chat",
                "docs",
                "mail",
                "meet",
                "routines",
                "settings",
                "store"
            ]
        );
    }

    #[test]
    fn shipped_config_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/freebsd/overlay/usr/local/etc/glasshouse/glassd.toml");
        Config::load(&path).unwrap();
    }
}
