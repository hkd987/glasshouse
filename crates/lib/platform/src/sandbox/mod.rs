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

mod host;
mod jail;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub use host::HostSandbox;
pub use jail::{JailConfig, JailSandbox};

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
pub trait Sandbox: Send + Sync + fmt::Debug {
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

    /// Destroys sandboxes left behind by an earlier daemon run.
    fn cleanup_stale(&self) -> Result<Vec<String>>;
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
    fn names_are_restricted() {
        assert!(check_name("glass-0193ab12").is_ok());
        for bad in ["", "a.b", "a/b", "a b", "../x"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }
}
