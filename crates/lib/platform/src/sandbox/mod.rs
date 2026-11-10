//! Where agents run.
//!
//! A sandbox is a private root directory with a fixed layout, a set of
//! mounts, and an account to run as. The runtime builds a [`SandboxSpec`] per
//! task, asks the backend to create it, spawns the agent with
//! [`Sandbox::command`], and destroys it when the task ends (PA-2: grants
//! expire with the task).
//!
//! Layout inside every sandbox:
//!
//! ```text
//! /run/glasshouse/agent.sock   the task's MCP socket, the agent's only way out
//! /work                        scratch space private to the task
//! /tmp                         private temporary files
//! /etc                         a minimal passwd/group naming only the agent account
//! ```
//!
//! The jail backend also puts every task below the person's CPU priority and
//! within [`ResourceLimits`], so a busy agent can't make the desktop stutter.

mod host;
mod jail;
mod tool;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub use host::HostSandbox;
pub use jail::{JailConfig, JailSandbox};
pub use tool::{ToolOutput, ToolRun, UnshareTools};

use crate::Result;

/// Where the task socket appears inside the sandbox.
pub const AGENT_SOCKET_PATH: &str = "/run/glasshouse/agent.sock";
/// The task's private scratch directory inside the sandbox.
pub const WORK_DIR: &str = "/work";

/// A path mounted into the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    /// Host path.
    pub source: PathBuf,
    /// Absolute path inside the sandbox.
    pub target: PathBuf,
    pub writable: bool,
}

/// Everything needed to create, enter and destroy one sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSpec {
    /// Unique name, also the jail name: letters, digits and dashes.
    pub name: String,
    /// Host directory that becomes `/` inside. Created by the backend and
    /// removed on destroy, so it must be dedicated to this sandbox.
    pub root: PathBuf,
    pub mounts: Vec<Mount>,
    /// The account agent processes run as.
    pub user: String,
    pub uid: u32,
    pub gid: u32,
    /// The agent computer the task runs in (ADR 0065, 0066), kept between
    /// its tasks; only the computer backend reads it, and gives a task
    /// without one a computer of its own that goes with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer: Option<String>,
    /// The persona the task is for, on that computer (ADR 0068): it
    /// starts in a folder of its own there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
}

/// What one task's processes may use, so the person's own work stays
/// responsive while agents run: agents start below the person's CPU
/// priority, and each task's jail gets rctl(8) limits on CPU, memory, disk
/// and processes for as long as it exists (PA-2: removed with it). Zero
/// turns a limit off. The host development backend applies none of this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceLimits {
    /// The niceness agents start with: 0 is the person's, 20 the lowest.
    /// An unprivileged agent can raise it but never lower it.
    pub nice: u8,
    /// CPU for all of the task's processes together, in percent of one CPU
    /// (rctl `pcpu`).
    pub cpu_percent: u64,
    /// Memory for the whole task, in bytes: both what it may commit
    /// (`swapuse`, so allocations past it fail at once instead of the
    /// machine swapping) and what may stay resident (`memoryuse`).
    pub memory_bytes: u64,
    /// Disk throughput, in bytes per second (`readbps`, `writebps`).
    pub read_bps: u64,
    pub write_bps: u64,
    /// Disk operations per second (`readiops`, `writeiops`).
    pub read_iops: u64,
    pub write_iops: u64,
    /// Processes at once (`maxproc`), so a runaway agent can't fill the
    /// process table.
    pub max_processes: u64,
}

impl ResourceLimits {
    /// The rctl(8) rules for the jail `jail`, one per limit that is set.
    pub fn rctl_rules(&self, jail: &str) -> Vec<String> {
        [
            ("pcpu", "deny", self.cpu_percent),
            ("swapuse", "deny", self.memory_bytes),
            ("memoryuse", "deny", self.memory_bytes),
            ("readbps", "throttle", self.read_bps),
            ("writebps", "throttle", self.write_bps),
            ("readiops", "throttle", self.read_iops),
            ("writeiops", "throttle", self.write_iops),
            ("maxproc", "deny", self.max_processes),
        ]
        .into_iter()
        .filter(|(_, _, amount)| *amount > 0)
        .map(|(resource, action, amount)| format!("jail:{jail}:{resource}:{action}={amount}"))
        .collect()
    }

    /// The same limits without the ones that need rctl, for a machine
    /// booted without resource accounting (`kern.racct.enable=0`).
    pub fn priority_only(self) -> Self {
        Self {
            nice: self.nice,
            ..Self::default()
        }
    }
}

/// A process to start inside a sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    /// Path to the program as seen inside the sandbox.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The complete environment; nothing is inherited from the daemon.
    pub env: Vec<(String, String)>,
}

/// A sandbox backend.
///
/// Methods block (they run base-system tools); call them from a blocking
/// context.
/// Runs a tool once on files in its folder, as tightly confined as it
/// can: glassd and the admin console bundle the apps people make this way
/// (ADR 0032). Every sandbox is one; so is [`UnshareTools`] on our servers.
pub trait ToolRunner: Send + Sync + fmt::Debug {
    fn run_tool(&self, run: &ToolRun) -> Result<ToolOutput>;

    /// Whether the tool runs as [`ToolRun::user`], so that its folder must
    /// be theirs when the caller is root.
    fn runs_as_user(&self) -> bool {
        true
    }
}

/// What a start's sweep of an earlier run's sandboxes did
/// ([`Sandbox::cleanup_stale`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    /// Removed whole.
    pub removed: Vec<String>,
    /// Still on disk, each with why. Nothing of theirs runs; their files
    /// couldn't be removed.
    pub left: Vec<(String, String)>,
}

pub trait Sandbox: ToolRunner + Send + Sync + fmt::Debug {
    /// A short name for status output and audit records.
    fn kind(&self) -> &'static str;

    /// Whether this backend actually contains an agent. Only a real boundary
    /// may run agents outside development.
    fn is_security_boundary(&self) -> bool;

    fn create(&self, spec: &SandboxSpec) -> Result<()>;

    /// A command that starts `process` inside the sandbox as `spec.user`.
    fn command(&self, spec: &SandboxSpec, process: &ProcessSpec) -> Command;

    /// Tears the sandbox down: stops every process in it, unmounts, deletes
    /// the root. Idempotent.
    fn destroy(&self, spec: &SandboxSpec) -> Result<()>;

    /// How a process inside the sandbox names `inside`.
    fn agent_path(&self, spec: &SandboxSpec, inside: &Path) -> PathBuf;

    /// Where `inside` lives on the host, for the daemon's own use (such as
    /// binding the task socket).
    fn host_path(&self, spec: &SandboxSpec, inside: &Path) -> PathBuf {
        spec.root.join(inside.strip_prefix("/").unwrap_or(inside))
    }

    /// Destroys sandboxes left behind by an earlier daemon run: every one,
    /// whatever happens to the others. One whose files can't be removed is
    /// in [`Swept::left`] and is tried again at the next start. An error is
    /// for what must stop the daemon starting: something of an earlier
    /// run's still running.
    fn cleanup_stale(&self) -> Result<Swept>;

    /// A file the task's output is also appended to, as it comes: an agent
    /// computer's screen, which its person watches (ADR 0065).
    fn screen(&self, _spec: &SandboxSpec) -> Option<PathBuf> {
        None
    }

    /// Whether tasks may name an agent computer to run in
    /// ([`SandboxSpec::computer`]).
    fn keeps_computers(&self) -> bool {
        false
    }

    /// The line people read as the agent starts. It names no backend
    /// (BR-1); the audit record carries [`Self::kind`].
    fn running_note(&self) -> &'static str {
        if self.is_security_boundary() {
            "Agent running in its own sealed sandbox"
        } else {
            "Agent running WITHOUT containment (development mode)"
        }
    }
}

/// Whether `id` may name an agent computer: lowercase letters, digits and
/// `-`, starting with a letter or digit, at most 48 (it names a folder).
pub fn valid_computer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Validates a sandbox name: it doubles as a jail name and a file name.
pub(crate) fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(crate::PlatformError::Unsafe(format!(
            "bad sandbox name {name:?}"
        )))
    }
}

/// Creates the fixed layout every sandbox root has.
pub(crate) fn create_layout(spec: &SandboxSpec) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let make = |rel: &str| {
        let path = spec.root.join(rel);
        std::fs::create_dir_all(&path)
            .map_err(|e| crate::PlatformError::io(format!("creating {}", path.display()), e))
            .map(|()| path)
    };
    make("run/glasshouse")?;
    make("etc")?;
    let tmp = make("tmp")?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o1777))
        .map_err(|e| crate::PlatformError::io("chmod tmp", e))?;
    let work = make("work")?;
    std::os::unix::fs::chown(&work, Some(spec.uid), Some(spec.gid))
        .map_err(|e| crate::PlatformError::io("chown work", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_become_one_rctl_rule_each_and_zero_is_off() {
        let limits = ResourceLimits {
            nice: 10,
            cpu_percent: 100,
            memory_bytes: 1 << 30,
            write_bps: 32 << 20,
            max_processes: 256,
            ..ResourceLimits::default()
        };
        assert_eq!(
            limits.rctl_rules("glass-1"),
            [
                "jail:glass-1:pcpu:deny=100",
                "jail:glass-1:swapuse:deny=1073741824",
                "jail:glass-1:memoryuse:deny=1073741824",
                "jail:glass-1:writebps:throttle=33554432",
                "jail:glass-1:maxproc:deny=256",
            ]
        );
        assert!(ResourceLimits::default().rctl_rules("glass-1").is_empty());
        let quiet = limits.priority_only();
        assert_eq!(quiet.nice, 10);
        assert!(quiet.rctl_rules("glass-1").is_empty());
    }

    #[test]
    fn adr0065_computer_ids_name_one_folder() {
        assert!(valid_computer_id("dana-reconciler"));
        assert!(valid_computer_id("r2"));
        for bad in ["", "-x", "A", "a.b", "a/b", "..", "a b", &"x".repeat(49)] {
            assert!(!valid_computer_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn names_are_restricted() {
        assert!(check_name("glass-0193ab12").is_ok());
        for bad in ["", "a.b", "a/b", "a b", "../x"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }
}
