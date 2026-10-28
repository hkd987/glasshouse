//! What runs an agent's computer (ADR 0065 §8, §10): Linux namespaces
//! where there's no KVM, Firecracker microVMs on AWS.
//!
//! A computer is kept between tasks. Each task runs its agent inside it
//! with the task's socket at [`glass_platform::sandbox::AGENT_SOCKET_PATH`],
//! the agent's only way to the person's world; the task's processes end
//! with the task (PA-2), the computer and its disk don't.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use glass_platform::sandbox::ProcessSpec;

/// One agent's computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerSpec {
    /// `dana-reconciler`: letters, digits and `-`.
    pub id: String,
    /// Its own folder on the host: its disk (a root tree for namespaces, an
    /// image file and snapshots for a VM), its logs, its sockets.
    pub dir: PathBuf,
    pub memory_mib: u32,
    pub vcpus: u32,
}

impl ComputerSpec {
    /// Its terminal: what its tasks and the person print, appended.
    pub fn screen(&self) -> PathBuf {
        self.dir.join("screen.log")
    }
}

/// A process to run in a computer for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRun {
    /// The task's socket on the host: it appears inside at
    /// `AGENT_SOCKET_PATH`.
    pub socket: PathBuf,
    /// The task's own folder inside the computer: `/work/<task>`.
    pub workdir: PathBuf,
    pub process: ProcessSpec,
}

/// What runs computers.
///
/// Methods block; call them from a blocking context.
pub trait Hypervisor: Send + Sync + fmt::Debug {
    /// For people and the record: "Linux namespaces", "Firecracker microVM".
    fn kind(&self) -> &'static str;

    /// Whether a computer is a security boundary: a VM is; namespaces are a
    /// weaker one, said so where it shows.
    fn contained(&self) -> bool;

    /// Makes its disk from the image the first time, starts it if it isn't
    /// running. A computer saved by [`Self::suspend`] resumes where it was.
    fn start(&self, computer: &ComputerSpec) -> Result<(), String>;

    /// Whether it runs.
    fn running(&self, computer: &ComputerSpec) -> bool;

    /// The host command that runs `run` inside the computer: its output
    /// goes to the caller, who also appends it to the screen.
    fn command(&self, computer: &ComputerSpec, run: &TaskRun) -> Result<Command, String>;

    /// A host path for `inside`, a path in the computer (namespaces: its
    /// root tree; a VM: none, files go through the guest).
    fn host_path(&self, computer: &ComputerSpec, inside: &Path) -> Option<PathBuf>;

    /// Stops everything in it, ready to save: a VM pauses and snapshots its
    /// memory into its folder; namespaces stop its processes. Afterwards its
    /// folder holds all of it.
    fn suspend(&self, computer: &ComputerSpec) -> Result<(), String>;

    /// As glassd starts, before any task: stops what an earlier run of
    /// glassd left running in computers under `dir`, and says what.
    fn stop_leftovers(&self, _dir: &Path) -> Vec<String> {
        Vec::new()
    }
}
