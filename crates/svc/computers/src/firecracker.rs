//! Firecracker microVMs as agents' computers on AWS (ADR 0065 §10, §11):
//! one microVM per computer, run by Firecracker's jailer on a Linux agent
//! host with KVM.
//!
//! Written against Firecracker v1.17.0: its API ([`api`], from the
//! release's swagger file), its jailer ([`jailer`]), its snapshots and its
//! vsock. Nothing here can boot a VM without KVM; the tests drive a
//! stand-in for the API, and a host with KVM is where it's proven.
//!
//! A computer's folder ([`ComputerSpec::dir`]) holds:
//! - `data.img`: its data disk, kept. The first start makes it an empty
//!   sparse file of [`FirecrackerConfig::data_disk_bytes`]; the guest image
//!   formats a blank data disk as it boots (it's `/dev/vdb`, the shared
//!   read-only base disk `/dev/vda`). "Start this computer over" is a blank
//!   file again.
//! - `snapshot/`, while it's suspended: its memory (`vm.mem`) and device
//!   state (`vm.snap`) from a full snapshot, and the Firecracker version
//!   that made them (`firecracker-version`). Gone once it resumes.
//! - While it runs: `firecracker.json`, Firecracker's pid and sockets, so a
//!   restarted host finds it; and `jail/`, the jailer's chroot, into which
//!   the kernel, the base disk and the data disk are hard-linked, and where
//!   Firecracker's API and vsock sockets are.
//! - `screen.log`.
//!
//! So a suspended computer's folder is all of it, and a stopped one's is
//! its disk; neither holds anything of the host's.
//!
//! The guest has no network device: tasks reach it over vsock ([`exec`]).

pub mod api;
pub mod exec;
mod http;
pub mod jailer;
mod process;
#[cfg(test)]
mod tests;
pub mod vmexec;
pub mod vmguest;

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, chown};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::hypervisor::{ComputerSpec, Hypervisor, TaskRun};
use api::{
    BootSource, Client, Drive, InstanceInfo, MachineConfig, MemoryBackend, MemoryBackendType,
    SnapshotCreate, SnapshotLoad, SnapshotType, Vsock,
};

/// Firecracker's getting-started command line; the root disk's `root=` and
/// `ro` Firecracker adds itself.
pub const DEFAULT_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1";
/// A new computer's data disk: 16 GiB, taking only what's written.
pub const DEFAULT_DATA_DISK_BYTES: u64 = 16 << 30;
/// Memory a computer's cgroup allows beyond its guest's, for Firecracker.
pub const DEFAULT_OVERHEAD_MIB: u32 = 64;
/// How long a new Firecracker may take to answer on its API.
pub const DEFAULT_API_TIMEOUT: Duration = Duration::from_secs(5);
/// Every guest's vsock CID. Firecracker's vsock goes through Unix sockets
/// in each computer's own jail, not the host kernel's vsock, so computers
/// on a host needn't differ.
pub const GUEST_CID: u32 = 3;

/// In a computer's folder.
const DATA_DISK: &str = "data.img";
const SNAPSHOT: &str = "snapshot";
const SNAPSHOT_NEW: &str = "snapshot.new";
const STATE: &str = "firecracker.json";
/// The jailer's chroot base.
pub(crate) const JAIL: &str = "jail";
/// In `snapshot/`, and in the jail as Firecracker writes them.
const SNAP_STATE: &str = "vm.snap";
const SNAP_MEMORY: &str = "vm.mem";
const SNAP_VERSION: &str = "firecracker-version";
/// In the jail.
const KERNEL: &str = "vmlinux";
const ROOT_IMAGE: &str = "rootfs.img";
const VSOCK: &str = "vsock.sock";
/// A snapshot being resumed from: its memory backs the guest's from then
/// on, so the next snapshot is written beside it, never over it.
const RESUME_STATE: &str = "resume.snap";
const RESUME_MEMORY: &str = "resume.mem";

/// How an agent host runs Firecracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirecrackerConfig {
    /// The `firecracker` binary: static, the same release as the jailer.
    pub firecracker: PathBuf,
    pub jailer: PathBuf,
    /// The host's side of the guest's exec service ([`exec`]).
    pub vmexec: PathBuf,
    /// Our signed guest kernel, uncompressed (`vmlinux`).
    pub kernel: PathBuf,
    /// The read-only base disk every computer shares: the signed image,
    /// readable by everyone. It and the kernel are hard-linked into each
    /// computer's jail, so they must be on the computers' filesystem.
    pub root_image: PathBuf,
    /// The size a new computer's data disk gets, in bytes.
    pub data_disk_bytes: u64,
    /// The guest kernel's command line.
    pub boot_args: String,
    /// The unprivileged user and group Firecracker runs as.
    pub uid: u32,
    pub gid: u32,
    /// Memory a computer's cgroup allows beyond its guest's, in MiB.
    pub overhead_mib: u32,
    /// The cgroup computers' cgroups go in, with the memory and cpu
    /// controllers enabled for its children; by default the jailer's (the
    /// binary's name).
    pub parent_cgroup: Option<String>,
    /// A network namespace for Firecracker, one with nothing in it.
    pub netns: Option<PathBuf>,
    /// How long a new Firecracker may take to answer on its API.
    pub api_timeout: Duration,
}

/// How [`FirecrackerHypervisor::start_computer`] started a computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    AlreadyRunning,
    /// From its disk: it had no snapshot.
    Booted,
    /// From its snapshot, its programs as they were.
    Resumed,
    /// From its disk, because another Firecracker made its snapshot and
    /// this one can't restore it (ADR 0065 §11): its open programs are
    /// lost, never its files.
    BootedFromDisk {
        saved_by: String,
        host: String,
    },
    /// From its disk, because its snapshot, made by this same Firecracker,
    /// didn't load (`why`): the same loss, and the same files.
    BootedUnresumed {
        why: String,
    },
}

/// Why a start didn't happen.
enum Failed {
    /// Firecracker was given its snapshot and didn't load it.
    Snapshot(String),
    Other(String),
}

impl Failed {
    fn into_text(self) -> String {
        match self {
            Self::Snapshot(why) | Self::Other(why) => why,
        }
    }
}

impl From<String> for Failed {
    fn from(why: String) -> Self {
        Self::Other(why)
    }
}

impl fmt::Display for Started {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => f.write_str("It was running."),
            Self::Booted => f.write_str("It started."),
            Self::Resumed => f.write_str("It resumed where it was."),
            Self::BootedFromDisk { saved_by, host } => write!(
                f,
                "It started again from its disk: it was put away by Firecracker {saved_by}, \
                 and this host's Firecracker {host} can't resume that. The programs it had \
                 open were closed; its files are all here."
            ),
            Self::BootedUnresumed { why } => write!(
                f,
                "It started again from its disk: it couldn't be resumed as it was put away. \
                 {why} The programs it had open were closed; its files are all here."
            ),
        }
    }
}

/// What a running computer's folder records about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Running {
    pid: u32,
    api_socket: PathBuf,
    vsock: PathBuf,
    jail: PathBuf,
    firecracker_version: String,
}

/// Agents' computers as Firecracker microVMs.
#[derive(Debug)]
pub struct FirecrackerHypervisor {
    config: FirecrackerConfig,
    /// The next task's forwarding port, from [`exec::FORWARD_PORTS`].
    next_port: AtomicU32,
}

impl FirecrackerHypervisor {
    pub fn new(config: FirecrackerConfig) -> Self {
        Self {
            config,
            next_port: AtomicU32::new(0),
        }
    }

    pub fn config(&self) -> &FirecrackerConfig {
        &self.config
    }

    /// [`Hypervisor::start`], saying how it started.
    pub fn start_computer(&self, computer: &ComputerSpec) -> Result<Started, String> {
        check_id(computer)?;
        if self.running(computer) {
            return Ok(Started::AlreadyRunning);
        }
        let jail = self.jail(computer);
        self.clear(computer)?;
        fs::create_dir_all(&computer.dir)
            .map_err(|e| format!("Couldn't make {}: {e}.", computer.dir.display()))?;
        self.make_data_disk(computer)?;
        let started = match self.start_once(computer, &jail) {
            // Its snapshot didn't load. Kept, it was tried again at the
            // next start and the one after, and the computer never
            // started again. It starts from its disk (ADR 0065: open
            // programs are lost, files never are), in a new Firecracker:
            // one that failed a load isn't asked for anything else.
            Err(Failed::Snapshot(why)) => self
                .clear(computer)
                .and_then(|()| remove_all(&computer.dir.join(SNAPSHOT)))
                .and_then(|()| self.start_once(computer, &jail).map_err(Failed::into_text))
                .map(|_| Started::BootedUnresumed { why }),
            other => other.map_err(Failed::into_text),
        };
        match started {
            Ok(started) => {
                if matches!(
                    started,
                    Started::BootedFromDisk { .. } | Started::BootedUnresumed { .. }
                ) {
                    let _ = note(computer, &started.to_string());
                }
                Ok(started)
            }
            Err(e) => {
                let _ = self.clear(computer);
                Err(format!("Couldn't start {}: {e}", computer.id))
            }
        }
    }

    /// One Firecracker in a new jail, resumed or booted.
    fn start_once(&self, computer: &ComputerSpec, jail: &Path) -> Result<Started, Failed> {
        self.make_jail(computer, jail)?;
        let pid = self.launch(computer, jail)?;
        let client = Client::new(jail.join(jailer::API_SOCKET.trim_start_matches('/')));
        let info = self.wait_for_api(&client, pid, &computer.host_id())?;
        let started = self.boot_or_resume(computer, jail, &client, &info.vmm_version)?;
        write_state(
            computer,
            &Running {
                pid,
                api_socket: client.socket().to_owned(),
                vsock: jail.join(VSOCK),
                jail: jail.to_owned(),
                firecracker_version: info.vmm_version,
            },
        )?;
        Ok(started)
    }

    /// The jailer's command for `computer`.
    pub fn jailer_command(&self, computer: &ComputerSpec) -> Command {
        jailer::command(&self.config, computer)
    }

    /// Its jail: `<dir>/jail/<firecracker's file name>/<its name on this
    /// host>/root`, where the jailer makes it.
    fn jail(&self, computer: &ComputerSpec) -> PathBuf {
        computer
            .dir
            .join(JAIL)
            .join(self.exec_name())
            .join(computer.host_id())
            .join("root")
    }

    /// Firecracker's file name, which names its folder and pid file in the
    /// jail.
    fn exec_name(&self) -> &std::ffi::OsStr {
        self.config
            .firecracker
            .file_name()
            .unwrap_or("firecracker".as_ref())
    }

    /// Its Firecracker, from its folder's record, if that runs.
    fn find(&self, computer: &ComputerSpec) -> Option<Running> {
        let ours = computer.host_id();
        read_state(computer).filter(|running| process::alive(running.pid, &ours))
    }

    /// Stops what's left of an earlier run (a Firecracker the host lost
    /// track of) and removes its jail and record. Its disk and snapshot stay.
    fn clear(&self, computer: &ComputerSpec) -> Result<(), String> {
        let pid_file = |jail: &Path| {
            let mut name = self.exec_name().to_owned();
            name.push(".pid");
            process::read_pid(&jail.join(name))
        };
        let mut pids = vec![];
        if let Some(running) = read_state(computer) {
            pids.push(running.pid);
            pids.extend(pid_file(&running.jail));
        }
        pids.extend(pid_file(&self.jail(computer)));
        let ours = computer.host_id();
        for pid in pids {
            process::stop(pid, &ours)?;
        }
        remove_all(&computer.dir.join(JAIL))?;
        remove(&computer.dir.join(STATE))
    }

    /// Makes its data disk the first time: an empty sparse file.
    fn make_data_disk(&self, computer: &ComputerSpec) -> Result<(), String> {
        let disk = computer.dir.join(DATA_DISK);
        if fs::symlink_metadata(&disk).is_ok_and(|m| !m.is_file()) {
            return Err(format!("{} isn't a disk image.", disk.display()));
        }
        let made = File::options().write(true).create_new(true).open(&disk);
        match made {
            Ok(file) => {
                if let Err(e) = file.set_len(self.config.data_disk_bytes) {
                    let _ = fs::remove_file(&disk);
                    return Err(format!("Couldn't make {}: {e}.", disk.display()));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("Couldn't make {}: {e}.", disk.display())),
        }
        chown(&disk, Some(self.config.uid), Some(self.config.gid)).map_err(|e| {
            format!(
                "Couldn't give {} to Firecracker's user: {e}.",
                disk.display()
            )
        })
    }

    /// Makes its jail with what it boots from: the jailer adds Firecracker.
    fn make_jail(&self, computer: &ComputerSpec, jail: &Path) -> Result<(), String> {
        fs::create_dir_all(jail).map_err(|e| format!("Couldn't make {}: {e}.", jail.display()))?;
        link(&self.config.kernel, &jail.join(KERNEL), "the guest kernel")?;
        link(
            &self.config.root_image,
            &jail.join(ROOT_IMAGE),
            "the base disk",
        )?;
        link(
            &computer.dir.join(DATA_DISK),
            &jail.join(DATA_DISK),
            "its data disk",
        )
    }

    /// Runs the jailer, which leaves Firecracker running; its pid.
    fn launch(&self, computer: &ComputerSpec, jail: &Path) -> Result<u32, String> {
        let output = self
            .jailer_command(computer)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                format!(
                    "couldn't run the jailer ({}): {e}.",
                    self.config.jailer.display()
                )
            })?;
        if !output.status.success() {
            let said = String::from_utf8_lossy(&output.stderr);
            let said = said.trim();
            return Err(if said.is_empty() {
                format!("the jailer failed ({}).", output.status)
            } else {
                format!("the jailer failed ({}): {said}", output.status)
            });
        }
        let mut name = self.exec_name().to_owned();
        name.push(".pid");
        let pid_file = jail.join(name);
        process::read_pid(&pid_file)
            .ok_or_else(|| format!("the jailer left no pid in {}.", pid_file.display()))
    }

    /// Waits for a new Firecracker to answer on its API.
    fn wait_for_api(&self, client: &Client, pid: u32, id: &str) -> Result<InstanceInfo, String> {
        let deadline = Instant::now() + self.config.api_timeout;
        loop {
            match client.instance_info() {
                Ok(info) => return Ok(info),
                Err(e) if !process::alive(pid, id) => {
                    return Err(format!("Firecracker ended as it started. {e}"));
                }
                Err(e) if Instant::now() >= deadline => {
                    return Err(format!(
                        "Firecracker didn't answer within {:?}. {e}",
                        self.config.api_timeout
                    ));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    /// Resumes it from its snapshot if this Firecracker can, or boots it.
    fn boot_or_resume(
        &self,
        computer: &ComputerSpec,
        jail: &Path,
        client: &Client,
        version: &str,
    ) -> Result<Started, Failed> {
        match snapshot_version(computer) {
            Some(saved) if saved == version => {
                resume(computer, jail, client, self.config.uid, self.config.gid)?;
                Ok(Started::Resumed)
            }
            Some(saved) => {
                remove_all(&computer.dir.join(SNAPSHOT))?;
                self.boot(computer, client)?;
                Ok(Started::BootedFromDisk {
                    saved_by: saved,
                    host: version.to_owned(),
                })
            }
            None => {
                // An incomplete snapshot can't be resumed from.
                remove_all(&computer.dir.join(SNAPSHOT))?;
                self.boot(computer, client)?;
                Ok(Started::Booted)
            }
        }
    }

    /// Configures a new Firecracker and starts the guest.
    fn boot(&self, computer: &ComputerSpec, client: &Client) -> Result<(), String> {
        client.put_machine_config(&MachineConfig {
            vcpu_count: computer.vcpus,
            mem_size_mib: computer.memory_mib,
        })?;
        client.put_boot_source(&BootSource {
            kernel_image_path: inside(KERNEL),
            boot_args: self.config.boot_args.clone(),
        })?;
        client.put_drive(&Drive {
            drive_id: "rootfs".into(),
            path_on_host: inside(ROOT_IMAGE),
            is_root_device: true,
            is_read_only: true,
        })?;
        client.put_drive(&Drive {
            drive_id: "data".into(),
            path_on_host: inside(DATA_DISK),
            is_root_device: false,
            is_read_only: false,
        })?;
        client.put_vsock(&Vsock {
            guest_cid: GUEST_CID,
            uds_path: inside(VSOCK),
        })?;
        client.start_instance()
    }

    /// Moves its snapshot out of its jail into its folder, with the version
    /// that made it, all at once.
    fn keep_snapshot(
        &self,
        computer: &ComputerSpec,
        jail: &Path,
        version: &str,
    ) -> Result<(), String> {
        let new = computer.dir.join(SNAPSHOT_NEW);
        remove_all(&new)?;
        fs::create_dir(&new).map_err(|e| format!("Couldn't make {}: {e}.", new.display()))?;
        for name in [SNAP_STATE, SNAP_MEMORY] {
            let from = jail.join(name);
            // Firecracker could have left anything there: only a plain file.
            if !fs::symlink_metadata(&from).is_ok_and(|m| m.is_file()) {
                return Err(format!(
                    "Firecracker left no snapshot file at {}.",
                    from.display()
                ));
            }
            fs::rename(&from, new.join(name))
                .map_err(|e| format!("Couldn't keep {}: {e}.", from.display()))?;
        }
        fs::write(new.join(SNAP_VERSION), format!("{version}\n"))
            .map_err(|e| format!("Couldn't record the snapshot's version: {e}."))?;
        let kept = computer.dir.join(SNAPSHOT);
        remove_all(&kept)?;
        fs::rename(&new, &kept).map_err(|e| format!("Couldn't keep {}: {e}.", kept.display()))
    }

    /// The next task's forwarding port.
    fn forward_port(&self) -> u32 {
        let ports = exec::FORWARD_PORTS;
        let n = self.next_port.fetch_add(1, Ordering::Relaxed);
        ports.start + n % (ports.end - ports.start)
    }
}

impl Hypervisor for FirecrackerHypervisor {
    fn kind(&self) -> &'static str {
        "Firecracker microVM"
    }

    fn contained(&self) -> bool {
        true
    }

    fn start(&self, computer: &ComputerSpec) -> Result<(), String> {
        self.start_computer(computer).map(drop)
    }

    fn running(&self, computer: &ComputerSpec) -> bool {
        self.find(computer).is_some()
    }

    /// `glass-vmexec` ([`vmexec`]), which runs `run` through the guest's
    /// exec service, `glass-vmguest` ([`vmguest`]); [`exec`] has how.
    fn command(&self, computer: &ComputerSpec, run: &TaskRun) -> Result<Command, String> {
        let running = self.find(computer).ok_or_else(|| {
            format!(
                "{} isn't running: start it before running a task in it.",
                computer.id
            )
        })?;
        let request = exec::ExecRequest::new(run, self.forward_port())?;
        exec::helper_command(&self.config.vmexec, &running.vsock, &run.socket, &request)
    }

    fn host_path(&self, _computer: &ComputerSpec, _inside: &Path) -> Option<PathBuf> {
        None
    }

    /// Pauses it, takes a full snapshot into its folder and stops its
    /// Firecracker. If the snapshot fails it resumes, and keeps running.
    fn suspend(&self, computer: &ComputerSpec) -> Result<(), String> {
        check_id(computer)?;
        let Some(running) = self.find(computer) else {
            // Not running: its folder holds its disk, and its snapshot if
            // it has one, already.
            return self.clear(computer);
        };
        let id = &computer.id;
        // A guest has run, and Firecracker owns its jail: what glassd
        // connects to there must be a socket, not a link to another one.
        let socket = fs::symlink_metadata(&running.api_socket);
        if !socket.is_ok_and(|m| m.file_type().is_socket()) {
            return Err(format!(
                "Couldn't suspend {id}: {} isn't Firecracker's socket.",
                running.api_socket.display()
            ));
        }
        let client = Client::new(&running.api_socket);
        let info = client
            .instance_info()
            .map_err(|e| format!("Couldn't suspend {id}: {e}"))?;
        client
            .pause()
            .map_err(|e| format!("Couldn't suspend {id}: {e}"))?;
        let snapshot = SnapshotCreate {
            snapshot_type: SnapshotType::Full,
            snapshot_path: inside(SNAP_STATE),
            mem_file_path: inside(SNAP_MEMORY),
        };
        if let Err(e) = client.create_snapshot(&snapshot) {
            let resumed = match client.resume() {
                Ok(()) => "it keeps running".to_owned(),
                Err(again) => format!("and it didn't resume either: {again}"),
            };
            return Err(format!("Couldn't snapshot {id}, so {resumed}. {e}"));
        }
        process::stop(running.pid, &computer.host_id())?;
        self.keep_snapshot(computer, &running.jail, &info.vmm_version)?;
        self.clear(computer)
    }
}

/// Moves its snapshot into the jail and has Firecracker resume from it. If
/// that fails the snapshot goes back to its folder, for another try.
fn resume(
    computer: &ComputerSpec,
    jail: &Path,
    client: &Client,
    uid: u32,
    gid: u32,
) -> Result<(), Failed> {
    let kept = computer.dir.join(SNAPSHOT);
    let moves = [(SNAP_STATE, RESUME_STATE), (SNAP_MEMORY, RESUME_MEMORY)];
    let put_back = || {
        for (name, in_jail) in moves {
            let _ = fs::rename(jail.join(in_jail), kept.join(name));
        }
    };
    for (name, in_jail) in moves {
        // Given to Firecracker's user before they're in its jail, where it
        // could put a link in their place.
        let from = kept.join(name);
        let moved =
            chown(&from, Some(uid), Some(gid)).and_then(|()| fs::rename(&from, jail.join(in_jail)));
        if let Err(e) = moved {
            put_back();
            return Err(Failed::Other(format!(
                "couldn't move its snapshot into its jail: {e}."
            )));
        }
    }
    let load = client.load_snapshot(&SnapshotLoad {
        snapshot_path: inside(RESUME_STATE),
        mem_backend: MemoryBackend {
            backend_type: MemoryBackendType::File,
            backend_path: inside(RESUME_MEMORY),
        },
        resume_vm: true,
    });
    if let Err(e) = load {
        put_back();
        return Err(Failed::Snapshot(e));
    }
    // Resumed. Its files were moved, not copied: resuming from them again
    // would undo what the disk has had since. What's left is its version.
    let _ = remove_all(&kept);
    Ok(())
}

/// The version that made its snapshot, if it has a whole one.
fn snapshot_version(computer: &ComputerSpec) -> Option<String> {
    let kept = computer.dir.join(SNAPSHOT);
    if !kept.join(SNAP_STATE).is_file() || !kept.join(SNAP_MEMORY).is_file() {
        return None;
    }
    let version = fs::read_to_string(kept.join(SNAP_VERSION)).ok()?;
    Some(version.trim().to_owned()).filter(|v| !v.is_empty())
}

fn read_state(computer: &ComputerSpec) -> Option<Running> {
    let bytes = fs::read(computer.dir.join(STATE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_state(computer: &ComputerSpec, running: &Running) -> Result<(), String> {
    let path = computer.dir.join(STATE);
    let new = computer.dir.join(format!("{STATE}.new"));
    let json = serde_json::to_vec_pretty(running).map_err(|e| e.to_string())?;
    fs::write(&new, json)
        .and_then(|()| fs::rename(&new, &path))
        .map_err(|e| format!("couldn't record it in {}: {e}.", path.display()))
}

/// Appends a line for people to its screen.
fn note(computer: &ComputerSpec, line: &str) -> io::Result<()> {
    let mut screen = File::options()
        .create(true)
        .append(true)
        .open(computer.screen())?;
    writeln!(screen, "{line}")
}

/// A name in the jail as Firecracker sees it, from the jail's root.
fn inside(name: &str) -> String {
    format!("/{}", name.trim_start_matches('/'))
}

/// The most characters of a jailer's id.
const JAILER_ID_MOST: usize = 64;

/// The jailer's rule for an id: letters, digits and dashes, at most 64.
/// Its id for a computer is the computer's name on this host
/// ([`ComputerSpec::host_id`]), so the computer's own id has what that
/// leaves.
fn check_id(computer: &ComputerSpec) -> Result<(), String> {
    let id = &computer.id;
    let fits = !id.is_empty()
        && computer.host_id().len() <= JAILER_ID_MOST
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if fits {
        Ok(())
    } else {
        let most = JAILER_ID_MOST - (computer.host_id().len() - id.len());
        Err(format!(
            "A computer's id is letters, digits and dashes, at most {most}: {id:?} isn't."
        ))
    }
}

fn link(from: &Path, to: &Path, what: &str) -> Result<(), String> {
    let _ = fs::remove_file(to);
    fs::hard_link(from, to).map_err(|e| {
        let why = if e.kind() == io::ErrorKind::CrossesDevices {
            "it must be on the same filesystem as the computers' folders".to_owned()
        } else {
            e.to_string()
        };
        format!(
            "couldn't link {what} ({}) into the jail: {why}.",
            from.display()
        )
    })
}

fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(format!("Couldn't remove {}: {e}.", path.display()))
        }
        _ => Ok(()),
    }
}

/// Removes a folder and what's in it (links are removed, not followed).
fn remove_all(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(format!("Couldn't remove {}: {e}.", path.display()))
        }
        _ => Ok(()),
    }
}
