//! The hypervisor against stand-ins: Firecracker's API (as v1.17.0's
//! swagger describes it) served on a Unix socket, recording each request;
//! and a jailer, a script that records how it was called, links the jail's
//! API socket to the stand-in and leaves a process running with
//! Firecracker's command line.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, OnceLock};

use glass_platform::sandbox::ProcessSpec;
use serde_json::{Value, json};

use super::*;

const ID: &str = "dana-reconciler";
const DEVICE_STATE: &[u8] = b"device state";
const GUEST_MEMORY: &[u8] = b"guest memory";

/// A request the stand-in took.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    method: String,
    path: String,
    body: Option<Value>,
}

fn get(path: &str) -> Seen {
    Seen {
        method: "GET".into(),
        path: path.into(),
        body: None,
    }
}

fn put(path: &str, body: Value) -> Seen {
    Seen {
        method: "PUT".into(),
        path: path.into(),
        body: Some(body),
    }
}

fn patch(path: &str, body: Value) -> Seen {
    Seen {
        method: "PATCH".into(),
        path: path.into(),
        body: Some(body),
    }
}

/// Configuring and starting a computer from its disk.
fn boot_sequence() -> Vec<Seen> {
    vec![
        get("/"),
        put(
            "/machine-config",
            json!({"vcpu_count": 2, "mem_size_mib": 1024}),
        ),
        put(
            "/boot-source",
            json!({"kernel_image_path": "/vmlinux", "boot_args": "console=ttyS0 reboot=k panic=1"}),
        ),
        put(
            "/drives/rootfs",
            json!({"drive_id": "rootfs", "path_on_host": "/rootfs.img",
                   "is_root_device": true, "is_read_only": true}),
        ),
        put(
            "/drives/data",
            json!({"drive_id": "data", "path_on_host": "/data.img",
                   "is_root_device": false, "is_read_only": false}),
        ),
        put("/vsock", json!({"guest_cid": 3, "uds_path": "/vsock.sock"})),
        put("/actions", json!({"action_type": "InstanceStart"})),
    ]
}

fn full_snapshot() -> Seen {
    put(
        "/snapshot/create",
        json!({"snapshot_type": "Full", "snapshot_path": "/vm.snap", "mem_file_path": "/vm.mem"}),
    )
}

/// Firecracker, as far as its API shows: it takes what the swagger
/// describes when its state allows it, and finds the files it's given in
/// its jail.
#[derive(Debug)]
struct Fake {
    jail: PathBuf,
    version: String,
    state: &'static str,
    /// The process answering, from the jail's pid file: a new one starts
    /// afresh.
    pid: Option<u32>,
    /// (method, path, status, fault message) to answer with.
    refusals: Vec<(String, String, u16, String)>,
    seen: Vec<Seen>,
    heads: Vec<String>,
}

impl Fake {
    fn answer(&mut self, method: &str, path: &str, body: Option<Value>) -> (u16, Option<Value>) {
        let pid = process::read_pid(&self.jail.join("firecracker.pid"));
        if pid != self.pid {
            self.pid = pid;
            self.state = "Not started";
        }
        self.seen.push(Seen {
            method: method.into(),
            path: path.into(),
            body: body.clone(),
        });
        if let Some((.., status, message)) = self
            .refusals
            .iter()
            .find(|(m, p, ..)| m == method && p == path)
        {
            return (*status, Some(json!({ "fault_message": message })));
        }
        let body = body.unwrap_or(Value::Null);
        let jail = self.jail.clone();
        let file = |path: &Value| path.as_str().map(|p| jail.join(p.trim_start_matches('/')));
        let exists = |path: &Value| file(path).is_some_and(|p| p.is_file());
        let read = |path: &Value| file(path).and_then(|p| fs::read(p).ok());
        let fresh = self.state == "Not started";
        let ok = match (method, path) {
            ("GET", "/") => {
                let info = json!({"app_name": "Firecracker", "id": "anonymous-instance",
                                  "state": self.state, "vmm_version": self.version});
                return (200, Some(info));
            }
            ("PUT", "/machine-config") => {
                fresh && body["vcpu_count"].is_u64() && body["mem_size_mib"].is_u64()
            }
            ("PUT", "/boot-source") => fresh && exists(&body["kernel_image_path"]),
            ("PUT", p) if p.starts_with("/drives/") => {
                fresh
                    && body["drive_id"]
                        .as_str()
                        .is_some_and(|id| p == format!("/drives/{id}"))
                    && exists(&body["path_on_host"])
            }
            ("PUT", "/vsock") => fresh && body["guest_cid"].as_u64().is_some_and(|cid| cid >= 3),
            ("PUT", "/actions") => {
                let ok = fresh && body["action_type"] == "InstanceStart";
                if ok {
                    self.state = "Running";
                }
                ok
            }
            ("PATCH", "/vm") => {
                let to = match body["state"].as_str() {
                    Some("Paused") => Some("Paused"),
                    Some("Resumed") => Some("Running"),
                    _ => None,
                };
                match to {
                    Some(to) if !fresh => {
                        self.state = to;
                        true
                    }
                    _ => false,
                }
            }
            ("PUT", "/snapshot/create") => {
                let ok = self.state == "Paused" && body["snapshot_type"] == "Full";
                if ok {
                    fs::write(file(&body["snapshot_path"]).unwrap(), DEVICE_STATE).unwrap();
                    fs::write(file(&body["mem_file_path"]).unwrap(), GUEST_MEMORY).unwrap();
                }
                ok
            }
            ("PUT", "/snapshot/load") => {
                let ok = fresh
                    && body["mem_backend"]["backend_type"] == "File"
                    && read(&body["snapshot_path"]).as_deref() == Some(DEVICE_STATE)
                    && read(&body["mem_backend"]["backend_path"]).as_deref() == Some(GUEST_MEMORY);
                if ok {
                    self.state = if body["resume_vm"] == true {
                        "Running"
                    } else {
                        "Paused"
                    };
                }
                ok
            }
            _ => false,
        };
        if ok {
            (204, None)
        } else {
            let message = format!(
                "The stand-in won't take {method} {path} {body} while {}",
                self.state
            );
            (400, Some(json!({ "fault_message": message })))
        }
    }
}

/// The stand-in API server.
struct StandIn {
    fake: Arc<Mutex<Fake>>,
}

impl StandIn {
    fn start(socket: &Path, jail: PathBuf, version: &str) -> Self {
        let listener = UnixListener::bind(socket).unwrap();
        let fake = Arc::new(Mutex::new(Fake {
            jail,
            version: version.into(),
            state: "Not started",
            pid: None,
            refusals: vec![],
            seen: vec![],
            heads: vec![],
        }));
        let serving = Arc::clone(&fake);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => serve(stream, &serving),
                    Err(_) => return,
                }
            }
        });
        Self { fake }
    }

    /// What it took since last asked.
    fn seen(&self) -> Vec<Seen> {
        std::mem::take(&mut self.fake.lock().unwrap().seen)
    }

    fn heads(&self) -> Vec<String> {
        self.fake.lock().unwrap().heads.clone()
    }

    fn pid(&self) -> Option<u32> {
        self.fake.lock().unwrap().pid
    }

    fn set_version(&self, version: &str) {
        self.fake.lock().unwrap().version = version.into();
    }

    fn refuse(&self, method: &str, path: &str, status: u16, message: &str) {
        let mut fake = self.fake.lock().unwrap();
        fake.refusals
            .push((method.into(), path.into(), status, message.into()));
    }
}

/// Answers one request as Firecracker's server does.
fn serve(stream: UnixStream, fake: &Mutex<Fake>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(&stream);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        head.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let mut request = head.lines().next().unwrap_or_default().split(' ');
    let (method, path) = (
        request.next().unwrap_or_default(),
        request.next().unwrap_or_default(),
    );
    assert_eq!(request.next(), Some("HTTP/1.1"));
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, value)| value.trim().parse().unwrap());
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body = (!body.is_empty()).then(|| serde_json::from_slice(&body).unwrap());
    let (status, reply) = {
        let mut fake = fake.lock().unwrap();
        fake.heads.push(head.clone());
        fake.answer(method, path, body)
    };
    let mut out =
        format!("HTTP/1.1 {status} \r\nServer: Firecracker API\r\nConnection: keep-alive\r\n");
    let reply = reply.map(|reply| reply.to_string()).unwrap_or_default();
    if !reply.is_empty() {
        out.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            reply.len()
        ));
    }
    out.push_str("\r\n");
    out.push_str(&reply);
    (&stream).write_all(out.as_bytes()).unwrap();
    // Like Firecracker, keeps the connection until the client is done.
    let _ = reader.read_to_end(&mut Vec::new());
}

/// The stand-in jailer. The test's folder is three above the chroot base
/// (`<test>/computers/<id>/jail`): the stand-in's socket is there, and the
/// record of the jailer's calls goes there.
const JAILER: &str = r#"#!/bin/sh
set -e
id= exe= base= api=
args=$(printf '%s\n' "$@")
while [ $# -gt 0 ]; do
  case "$1" in
    --id) id=$2; shift 2 ;;
    --exec-file) exe=$2; shift 2 ;;
    --chroot-base-dir) base=$2; shift 2 ;;
    --api-sock) api=$2; shift 2 ;;
    *) shift ;;
  esac
done
test=$(cd "$base/../../.." && pwd)
printf '%s\n---\n' "$args" >> "$test/jailer.log"
if [ -f "$test/jailer.fail" ]; then cat "$test/jailer.fail" >&2; exit 1; fi
name=$(basename "$exe")
root="$base/$name/$id/root"
mkdir -p "$root"
ln "$test/api.sock" "$root$api"
sh -c 'i=0; while [ $i -lt 300 ]; do sleep 1; i=$((i+1)); done' firecracker "--id=$id" \
  </dev/null >/dev/null 2>&1 &
echo $! > "$root/$name.pid"
"#;

/// Written once for all the tests (running a file another thread has open
/// for writing fails), in a folder named by its text that later runs share.
fn fake_jailer() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let mut hasher = DefaultHasher::new();
        JAILER.hash(&mut hasher);
        let dir = std::env::temp_dir().join(format!("glass-fc-jailer-{:016x}", hasher.finish()));
        let path = dir.join("jailer");
        if fs::read_to_string(&path).ok().as_deref() != Some(JAILER) {
            fs::create_dir_all(&dir).unwrap();
            let new = dir.join(format!("jailer.{}", std::process::id()));
            fs::write(&new, JAILER).unwrap();
            fs::set_permissions(&new, fs::Permissions::from_mode(0o755)).unwrap();
            fs::rename(&new, &path).unwrap();
        }
        path
    })
    .clone()
}

/// An agent host in a folder of its own, its Firecracker the stand-in.
struct Host {
    dir: PathBuf,
    standin: StandIn,
    hypervisor: FirecrackerHypervisor,
    computer: ComputerSpec,
}

impl Host {
    fn new(name: &str, version: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("glass-fc-{name}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for sub in ["bin", "images", "computers"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(dir.join("bin/firecracker"), "").unwrap();
        fs::write(dir.join("images/vmlinux"), "kernel").unwrap();
        fs::write(dir.join("images/rootfs.img"), "base disk").unwrap();
        let me = fs::metadata(&dir).unwrap();
        let config = FirecrackerConfig {
            firecracker: dir.join("bin/firecracker"),
            jailer: fake_jailer(),
            vmexec: "/usr/libexec/glass-vmexec".into(),
            kernel: dir.join("images/vmlinux"),
            root_image: dir.join("images/rootfs.img"),
            data_disk_bytes: 64 << 20,
            boot_args: DEFAULT_BOOT_ARGS.into(),
            uid: me.uid(),
            gid: me.gid(),
            overhead_mib: DEFAULT_OVERHEAD_MIB,
            parent_cgroup: None,
            netns: None,
            api_timeout: Duration::from_secs(5),
        };
        let computer = ComputerSpec {
            id: ID.into(),
            dir: dir.join("computers").join(ID),
            memory_mib: 1024,
            vcpus: 2,
        };
        let hypervisor = FirecrackerHypervisor::new(config);
        let standin = StandIn::start(&dir.join("api.sock"), hypervisor.jail(&computer), version);
        Self {
            dir,
            standin,
            hypervisor,
            computer,
        }
    }

    /// The jailer's calls, each its arguments.
    fn jailer_calls(&self) -> Vec<Vec<String>> {
        let log = fs::read_to_string(self.dir.join("jailer.log")).unwrap_or_default();
        log.split_terminator("---\n")
            .map(|call| call.lines().map(str::to_owned).collect())
            .collect()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.computer.dir.join(name)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Stops a stand-in Firecracker a failed test left running.
        let _ = self.hypervisor.clear(&self.computer);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn args(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn adr0065_a_firecracker_computer_boots_from_its_disk() {
    let host = Host::new("boot", "1.17.0");
    let (hypervisor, computer) = (&host.hypervisor, &host.computer);
    assert_eq!(hypervisor.start_computer(computer), Ok(Started::Booted));
    assert_eq!(host.standin.seen(), boot_sequence());
    for head in host.standin.heads() {
        assert!(head.contains("\r\nAccept: application/json\r\n"), "{head}");
        if !head.starts_with("GET ") {
            assert!(
                head.contains("\r\nContent-Type: application/json\r\n"),
                "{head}"
            );
        }
    }
    assert!(hypervisor.running(computer));

    // The jailer ran as its command line says.
    assert_eq!(
        host.jailer_calls(),
        vec![args(&hypervisor.jailer_command(computer))]
    );

    // An empty data disk taking no room yet, linked into the jail with the
    // kernel and the base disk.
    let disk = fs::metadata(host.file("data.img")).unwrap();
    assert_eq!(disk.len(), 64 << 20);
    assert!(disk.blocks() * 512 < disk.len());
    let jail = hypervisor.jail(computer);
    assert_eq!(
        fs::metadata(jail.join("data.img")).unwrap().ino(),
        disk.ino()
    );
    assert_eq!(fs::read(jail.join("rootfs.img")).unwrap(), b"base disk");
    assert_eq!(fs::read(jail.join("vmlinux")).unwrap(), b"kernel");

    // Its folder records it, so a restarted host finds it.
    let running = read_state(computer).unwrap();
    assert_eq!(Some(running.pid), host.standin.pid());
    assert_eq!(running.api_socket, jail.join("api.sock"));
    assert_eq!(running.vsock, jail.join("vsock.sock"));
    assert_eq!(running.firecracker_version, "1.17.0");
    let restarted = FirecrackerHypervisor::new(hypervisor.config().clone());
    assert!(restarted.running(computer));
    assert_eq!(
        restarted.start_computer(computer),
        Ok(Started::AlreadyRunning)
    );
    assert_eq!(host.standin.seen(), vec![]);

    hypervisor.suspend(computer).unwrap();
}

#[test]
fn adr0065_a_firecracker_computer_snapshots_and_resumes() {
    let host = Host::new("snapshot", "1.17.0");
    let (hypervisor, computer) = (&host.hypervisor, &host.computer);
    hypervisor.start_computer(computer).unwrap();
    let pid = read_state(computer).unwrap().pid;
    host.standin.seen();

    // Paused, snapshotted whole, stopped.
    hypervisor.suspend(computer).unwrap();
    assert_eq!(
        host.standin.seen(),
        vec![
            get("/"),
            patch("/vm", json!({"state": "Paused"})),
            full_snapshot()
        ]
    );
    assert!(!process::alive(pid, ID));
    assert!(!hypervisor.running(computer));

    // Its folder holds all of it, and nothing of the run.
    assert_eq!(
        fs::read(host.file("snapshot/vm.snap")).unwrap(),
        DEVICE_STATE
    );
    assert_eq!(
        fs::read(host.file("snapshot/vm.mem")).unwrap(),
        GUEST_MEMORY
    );
    assert_eq!(
        fs::read_to_string(host.file("snapshot/firecracker-version")).unwrap(),
        "1.17.0\n"
    );
    assert!(host.file("data.img").is_file());
    assert!(!host.file("jail").exists());
    assert!(!host.file("firecracker.json").exists());

    // Suspending it again changes nothing.
    hypervisor.suspend(computer).unwrap();
    assert_eq!(host.standin.seen(), vec![]);
    assert!(host.file("snapshot/vm.mem").is_file());

    // Started again, a new Firecracker resumes it from the snapshot.
    assert_eq!(hypervisor.start_computer(computer), Ok(Started::Resumed));
    assert_eq!(
        host.standin.seen(),
        vec![
            get("/"),
            put(
                "/snapshot/load",
                json!({"snapshot_path": "/resume.snap",
                       "mem_backend": {"backend_type": "File", "backend_path": "/resume.mem"},
                       "resume_vm": true}),
            ),
        ]
    );
    assert_ne!(Some(pid), host.standin.pid());
    assert!(hypervisor.running(computer));
    assert!(!host.file("snapshot").exists());

    // The memory it resumed from backs the guest now: the next snapshot is
    // written beside it.
    hypervisor.suspend(computer).unwrap();
    assert_eq!(host.standin.seen()[2], full_snapshot());
    assert_eq!(
        fs::read(host.file("snapshot/vm.mem")).unwrap(),
        GUEST_MEMORY
    );
}

#[test]
fn adr0065_a_snapshot_from_another_firecracker_boots_from_disk() {
    let host = Host::new("upgrade", "1.16.2");
    let (hypervisor, computer) = (&host.hypervisor, &host.computer);
    hypervisor.start_computer(computer).unwrap();
    let disk = File::options()
        .read(true)
        .write(true)
        .open(host.file("data.img"))
        .unwrap();
    disk.write_all_at(b"the agent's files", 4096).unwrap();
    hypervisor.suspend(computer).unwrap();
    host.standin.seen();

    // The host's Firecracker was updated while the computer slept.
    host.standin.set_version("1.17.0");
    assert_eq!(
        hypervisor.start_computer(computer),
        Ok(Started::BootedFromDisk {
            saved_by: "1.16.2".into(),
            host: "1.17.0".into()
        })
    );
    assert_eq!(host.standin.seen(), boot_sequence());

    // Its files are kept; the snapshot, which nothing here can resume, isn't.
    let mut files = [0; 17];
    disk.read_exact_at(&mut files, 4096).unwrap();
    assert_eq!(&files, b"the agent's files");
    assert!(!host.file("snapshot").exists());
    let screen = fs::read_to_string(computer.screen()).unwrap();
    assert!(
        screen.contains("put away by Firecracker 1.16.2"),
        "{screen}"
    );
    assert!(screen.contains("its files are all here"), "{screen}");

    hypervisor.suspend(computer).unwrap();
    assert_eq!(
        fs::read_to_string(host.file("snapshot/firecracker-version")).unwrap(),
        "1.17.0\n"
    );
}

#[test]
fn adr0065_a_firecracker_refusal_is_a_readable_error() {
    let host = Host::new("refusal", "1.17.0");
    let (hypervisor, computer) = (&host.hypervisor, &host.computer);
    host.standin.refuse(
        "PUT",
        "/drives/data",
        400,
        "Unable to add the block device: Permission denied (os error 13)",
    );
    assert_eq!(
        hypervisor.start_computer(computer),
        Err(
            "Couldn't start dana-reconciler: Firecracker refused PUT /drives/data (HTTP 400): \
             Unable to add the block device: Permission denied (os error 13)."
                .into()
        )
    );
    // Nothing of it is left running; its disk stays.
    let pid = host.standin.pid().unwrap();
    assert!(!process::alive(pid, ID));
    assert!(!hypervisor.running(computer));
    assert!(!host.file("jail").exists());
    assert!(!host.file("firecracker.json").exists());
    assert!(host.file("data.img").is_file());

    // A refusal without a reason, and no Firecracker at all.
    host.standin.refuse("PATCH", "/vm", 400, "");
    let client = Client::new(host.dir.join("api.sock"));
    assert_eq!(
        client.pause(),
        Err("Firecracker refused PATCH /vm (HTTP 400): it gave no reason.".into())
    );
    let nowhere = Client::new(host.dir.join("nowhere.sock"));
    let error = nowhere.instance_info().unwrap_err();
    assert!(
        error.starts_with(&format!(
            "Couldn't reach Firecracker's API at {} for GET /: ",
            host.dir.join("nowhere.sock").display()
        )),
        "{error}"
    );
}

#[test]
fn adr0065_a_jailer_that_fails_says_why() {
    let host = Host::new("jailer", "1.17.0");
    fs::write(
        host.dir.join("jailer.fail"),
        "Failed to create the jail: Permission denied\n",
    )
    .unwrap();
    assert_eq!(
        host.hypervisor.start_computer(&host.computer),
        Err(
            "Couldn't start dana-reconciler: the jailer failed (exit status: 1): \
             Failed to create the jail: Permission denied"
                .into()
        )
    );
    assert!(!host.file("jail").exists());
    assert_eq!(host.standin.seen(), vec![]);
}

#[test]
fn adr0065_running_means_its_firecracker_is_alive() {
    let host = Host::new("running", "1.17.0");
    let (hypervisor, computer) = (&host.hypervisor, &host.computer);
    assert!(!hypervisor.running(computer));
    hypervisor.start_computer(computer).unwrap();
    let running = read_state(computer).unwrap();

    // A record whose pid another process has now (after a restart) isn't it.
    let reused = Running {
        pid: std::process::id(),
        ..running.clone()
    };
    write_state(computer, &reused).unwrap();
    assert!(!hypervisor.running(computer));
    write_state(computer, &running).unwrap();
    assert!(hypervisor.running(computer));

    // Firecracker owns its jail: a link put where its socket was isn't
    // followed to whatever it points at.
    let socket = &running.api_socket;
    fs::remove_file(socket).unwrap();
    std::os::unix::fs::symlink(host.dir.join("api.sock"), socket).unwrap();
    host.standin.seen();
    let refused = hypervisor.suspend(computer).unwrap_err();
    assert!(
        refused.ends_with("isn't Firecracker's socket."),
        "{refused}"
    );
    assert_eq!(host.standin.seen(), vec![]);
    assert!(hypervisor.running(computer));
    fs::remove_file(socket).unwrap();
    fs::hard_link(host.dir.join("api.sock"), socket).unwrap();

    // Its Firecracker ended: the next start boots it afresh.
    process::stop(running.pid, ID).unwrap();
    assert!(!hypervisor.running(computer));
    host.standin.seen();
    assert_eq!(hypervisor.start_computer(computer), Ok(Started::Booted));
    assert_eq!(host.standin.seen(), boot_sequence());
    hypervisor.suspend(computer).unwrap();
}

#[test]
fn adr0065_the_jailer_runs_each_computer_in_a_jail_of_its_own() {
    let mut config = FirecrackerConfig {
        firecracker: "/usr/bin/firecracker".into(),
        jailer: "/usr/bin/jailer".into(),
        vmexec: "/usr/libexec/glass-vmexec".into(),
        kernel: "/var/lib/glasshouse/vmlinux".into(),
        root_image: "/var/lib/glasshouse/rootfs.img".into(),
        data_disk_bytes: DEFAULT_DATA_DISK_BYTES,
        boot_args: DEFAULT_BOOT_ARGS.into(),
        uid: 1500,
        gid: 1500,
        overhead_mib: DEFAULT_OVERHEAD_MIB,
        parent_cgroup: Some("glasshouse-computers".into()),
        netns: Some("/var/run/netns/computers".into()),
        api_timeout: DEFAULT_API_TIMEOUT,
    };
    let computer = ComputerSpec {
        id: ID.into(),
        dir: "/srv/computers/dana-reconciler".into(),
        memory_mib: 1024,
        vcpus: 2,
    };
    let command = jailer::command(&config, &computer);
    assert_eq!(command.get_program(), "/usr/bin/jailer");
    assert_eq!(
        args(&command),
        [
            "--id",
            "dana-reconciler",
            "--exec-file",
            "/usr/bin/firecracker",
            "--uid",
            "1500",
            "--gid",
            "1500",
            "--cgroup-version",
            "2",
            "--cgroup",
            "memory.max=1140850688",
            "--cgroup",
            "cpu.max=200000 100000",
            "--parent-cgroup",
            "glasshouse-computers",
            "--chroot-base-dir",
            "/srv/computers/dana-reconciler/jail",
            "--netns",
            "/var/run/netns/computers",
            "--daemonize",
            "--new-pid-ns",
            "--",
            "--api-sock",
            "/api.sock",
        ]
    );

    // Without them, the jailer's own cgroup and the host's namespace.
    config.parent_cgroup = None;
    config.netns = None;
    let plain = args(&jailer::command(&config, &computer));
    assert!(
        !plain
            .iter()
            .any(|a| a == "--parent-cgroup" || a == "--netns")
    );

    // Ids are the jailer's: letters, digits and dashes, at most 64.
    let hypervisor = FirecrackerHypervisor::new(config);
    for id in ["", "../etc", "dana_reconciler", &"a".repeat(65)] {
        let odd = ComputerSpec {
            id: id.into(),
            ..computer.clone()
        };
        let error = hypervisor.start_computer(&odd).unwrap_err();
        assert!(
            error.starts_with("A computer's id is letters, digits and dashes"),
            "{error}"
        );
    }
}

#[test]
fn adr0065_a_task_runs_in_the_guest_through_the_exec_helper() {
    let host = Host::new("exec", "1.17.0");
    let (hypervisor, computer) = (&host.hypervisor, &host.computer);
    let run = TaskRun {
        socket: "/var/run/glasshouse/tasks/t-1.sock".into(),
        workdir: "/work/t-1".into(),
        process: ProcessSpec {
            program: "/usr/local/bin/agent".into(),
            args: vec!["--task".into(), "t-1".into()],
            env: vec![
                ("HOME".into(), "/root".into()),
                ("NOTE".into(), "two\nlines".into()),
            ],
        },
    };
    assert_eq!(
        hypervisor.command(computer, &run).unwrap_err(),
        "dana-reconciler isn't running: start it before running a task in it."
    );

    hypervisor.start_computer(computer).unwrap();
    let command = hypervisor.command(computer, &run).unwrap();
    assert_eq!(command.get_program(), "/usr/libexec/glass-vmexec");
    let given = args(&command);
    let vsock = hypervisor.jail(computer).join("vsock.sock");
    assert_eq!(
        given[..7],
        [
            "--uds",
            vsock.to_str().unwrap(),
            "--port",
            "10000",
            "--socket",
            "/var/run/glasshouse/tasks/t-1.sock",
            "--",
        ]
    );
    assert_eq!(given.len(), 8);
    assert!(!given[7].contains('\n'));
    let request: Value = serde_json::from_str(&given[7]).unwrap();
    assert_eq!(
        request,
        json!({
            "program": "/usr/local/bin/agent",
            "args": ["--task", "t-1"],
            "env": [["HOME", "/root"], ["NOTE", "two\nlines"]],
            "workdir": "/work/t-1",
            "socket": {"path": "/run/glasshouse/agent.sock", "port": 20000},
        })
    );
    let parsed: exec::ExecRequest = serde_json::from_str(&given[7]).unwrap();
    assert_eq!(parsed.env[1], ("NOTE".to_owned(), "two\nlines".to_owned()));

    // Each task its own forwarding port.
    let next = args(&hypervisor.command(computer, &run).unwrap());
    let next: Value = serde_json::from_str(&next[7]).unwrap();
    assert_eq!(next["socket"]["port"], 20001);

    // Files go through the guest; a VM is a boundary.
    assert_eq!(hypervisor.host_path(computer, Path::new("/work/t-1")), None);
    assert_eq!(hypervisor.kind(), "Firecracker microVM");
    assert!(hypervisor.contained());
    hypervisor.suspend(computer).unwrap();
}
