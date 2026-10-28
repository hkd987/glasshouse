//! Firecracker's API: the calls an agent computer needs, as Firecracker
//! v1.17.0 describes them (`src/firecracker/swagger/firecracker.yaml`, and
//! `docs/snapshotting/snapshot-support.md` for snapshots).
//!
//! Firecracker serves HTTP/1.1 on a Unix socket; each call here is one
//! request on a connection of its own. Paths in bodies are as Firecracker
//! sees them: under the jailer, from its jail's root.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::http;

/// The Firecracker release this client follows.
pub const FIRECRACKER_RELEASE: &str = "v1.17.0";

/// How long a call may take.
const TIMEOUT: Duration = Duration::from_secs(30);
/// A full snapshot writes all of the guest's memory out.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(600);
/// The most of a refusal's text put in an error.
const MAX_REASON: usize = 500;

/// `PUT /machine-config` (`MachineConfiguration`; the rest left at its
/// defaults: no SMT, no CPU template, no huge pages).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineConfig {
    /// 1 to 32.
    pub vcpu_count: u32,
    pub mem_size_mib: u32,
}

/// `PUT /boot-source` (`BootSource`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootSource {
    /// An uncompressed kernel (`vmlinux`).
    pub kernel_image_path: String,
    pub boot_args: String,
}

/// `PUT /drives/{drive_id}` (`Drive`): a virtio block device on a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drive {
    pub drive_id: String,
    pub path_on_host: String,
    /// The guest's root: Firecracker adds `root=/dev/vda` and `ro` or `rw`
    /// to the kernel's command line.
    pub is_root_device: bool,
    pub is_read_only: bool,
}

/// `PUT /vsock` (`Vsock`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vsock {
    /// 3 or more.
    pub guest_cid: u32,
    /// Where Firecracker listens for the host's connections (`CONNECT
    /// <port>\n`); the guest's go to `<uds_path>_<port>`.
    pub uds_path: String,
}

/// `PATCH /vm` (`Vm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VmState {
    Paused,
    Resumed,
}

/// `PUT /snapshot/create` (`SnapshotCreateParams`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCreate {
    pub snapshot_type: SnapshotType,
    /// The device state; created, or truncated and written.
    pub snapshot_path: String,
    /// The guest's memory; created, or truncated and written.
    pub mem_file_path: String,
}

/// Full snapshots only: diff snapshots are a developer preview (ADR 0065
/// §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotType {
    Full,
}

/// `PUT /snapshot/load` (`SnapshotLoadParams`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotLoad {
    pub snapshot_path: String,
    pub mem_backend: MemoryBackend,
    /// Resumes it once loaded; otherwise it stays paused.
    pub resume_vm: bool,
}

/// `MemoryBackend`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryBackend {
    pub backend_type: MemoryBackendType,
    /// The memory file. It backs the guest's memory from then on, so it must
    /// not change while the VM runs.
    pub backend_path: String,
}

/// The kernel pages the memory file in (not a user-space fault handler).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryBackendType {
    File,
}

/// `GET /` (`InstanceInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub app_name: String,
    pub id: String,
    /// `Not started`, `Running` or `Paused`.
    pub state: String,
    /// What a snapshot is bound to.
    pub vmm_version: String,
}

/// `InstanceActionInfo`.
#[derive(Serialize)]
struct Action {
    action_type: &'static str,
}

#[derive(Serialize)]
struct Vm {
    state: VmState,
}

/// `Error`.
#[derive(Deserialize)]
struct Fault {
    fault_message: String,
}

/// One Firecracker's API, at its socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `GET /`: its id, its state and its version.
    pub fn instance_info(&self) -> Result<InstanceInfo, String> {
        let body = self.call("GET", "/", None, TIMEOUT)?;
        serde_json::from_slice(&body).map_err(|e| {
            format!("Firecracker's answer to GET / doesn't describe an instance: {e}.")
        })
    }

    /// `PUT /machine-config`, before it starts.
    pub fn put_machine_config(&self, config: &MachineConfig) -> Result<(), String> {
        self.put("/machine-config", config, TIMEOUT)
    }

    /// `PUT /boot-source`, before it starts.
    pub fn put_boot_source(&self, source: &BootSource) -> Result<(), String> {
        self.put("/boot-source", source, TIMEOUT)
    }

    /// `PUT /drives/{drive_id}`, before it starts.
    pub fn put_drive(&self, drive: &Drive) -> Result<(), String> {
        let id = &drive.drive_id;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!(
                "A drive's id is letters, digits and underscores: {id:?} isn't."
            ));
        }
        self.put(&format!("/drives/{id}"), drive, TIMEOUT)
    }

    /// `PUT /vsock`, before it starts.
    pub fn put_vsock(&self, vsock: &Vsock) -> Result<(), String> {
        self.put("/vsock", vsock, TIMEOUT)
    }

    /// `PUT /actions` with `InstanceStart`: boots what was configured.
    pub fn start_instance(&self) -> Result<(), String> {
        self.put(
            "/actions",
            &Action {
                action_type: "InstanceStart",
            },
            TIMEOUT,
        )
    }

    /// `PATCH /vm`.
    pub fn set_state(&self, state: VmState) -> Result<(), String> {
        let body = serde_json::to_vec(&Vm { state }).map_err(|e| e.to_string())?;
        self.call("PATCH", "/vm", Some(body), TIMEOUT).map(drop)
    }

    pub fn pause(&self) -> Result<(), String> {
        self.set_state(VmState::Paused)
    }

    pub fn resume(&self) -> Result<(), String> {
        self.set_state(VmState::Resumed)
    }

    /// `PUT /snapshot/create`, while it's paused.
    pub fn create_snapshot(&self, snapshot: &SnapshotCreate) -> Result<(), String> {
        self.put("/snapshot/create", snapshot, SNAPSHOT_TIMEOUT)
    }

    /// `PUT /snapshot/load`, on a Firecracker nothing was configured on yet.
    /// If it fails, that Firecracker ends.
    pub fn load_snapshot(&self, snapshot: &SnapshotLoad) -> Result<(), String> {
        self.put("/snapshot/load", snapshot, SNAPSHOT_TIMEOUT)
    }

    fn put(&self, path: &str, body: &impl Serialize, timeout: Duration) -> Result<(), String> {
        let body = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        self.call("PUT", path, Some(body), timeout).map(drop)
    }

    /// Makes one call; a reply outside 2xx is an error saying why.
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        let reply =
            http::exchange(&self.socket, method, path, body.as_deref(), timeout).map_err(|e| {
                format!(
                    "Couldn't reach Firecracker's API at {} for {method} {path}: {e}.",
                    self.socket.display()
                )
            })?;
        if (200..300).contains(&reply.status) {
            return Ok(reply.body);
        }
        Err(format!(
            "Firecracker refused {method} {path} (HTTP {}): {}.",
            reply.status,
            reason(&reply.body)
        ))
    }
}

/// What a refusal says: its `fault_message`, else its text.
fn reason(body: &[u8]) -> String {
    let said = match serde_json::from_slice::<Fault>(body) {
        Ok(fault) => fault.fault_message,
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    };
    let said = said.trim().trim_end_matches('.');
    if said.is_empty() {
        return "it gave no reason".into();
    }
    match said.char_indices().nth(MAX_REASON) {
        Some((at, _)) => format!("{}...", &said[..at]),
        None => said.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0065_a_refusal_reads_as_what_firecracker_said() {
        let said = |body: &str| reason(body.as_bytes());
        assert_eq!(
            said(
                r#"{"fault_message":"The requested operation is not supported after starting the microVM."}"#
            ),
            "The requested operation is not supported after starting the microVM"
        );
        assert_eq!(said("Bad request\n"), "Bad request");
        assert_eq!(said(r#"{"fault_message":""}"#), "it gave no reason");
        assert_eq!(said(""), "it gave no reason");
        assert_eq!(said(&"x".repeat(2000)).chars().count(), MAX_REASON + 3);
    }
}
