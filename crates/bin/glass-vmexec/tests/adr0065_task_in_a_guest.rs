//! ADR 0065 §10, end to end without KVM: `glass-vmexec`, this crate's
//! binary, runs tasks in a "guest" on this host.
//!
//! Between them is a stand-in for Firecracker's vsock device. The host
//! connects to its socket and asks for guest port 10000 (`CONNECT 10000\n`,
//! answered `OK <n>\n`), which it splices to the guest's exec service; the
//! guest's connections to host port P it splices to `<uds>_P`. The exec
//! service is `glass-vmguest`'s, run here from the library its binary runs.
//! A stand-in task socket answers what the task asks through it.
//!
//! Needs `unshare --mount` (root, or user namespaces) and curl.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use glass_computers::firecracker::exec::{self, EXEC_PORT, ExecRequest, SocketForward};
use glass_computers::firecracker::vmguest::{self, Connector, GuestConfig};

/// The task socket's port on the host.
const PORT: u32 = 20_000;

/// A guest with its exec service, the vsock device between, and a task
/// socket on the host, in a folder of their own.
struct Guest {
    dir: PathBuf,
    /// Firecracker's vsock socket, in the computer's jail.
    uds: PathBuf,
    task_socket: PathBuf,
}

impl Guest {
    fn new(name: &str) -> Self {
        Self::with(name, |_| {})
    }

    /// With the exec service's configuration changed first.
    fn with(name: &str, change: impl FnOnce(&mut GuestConfig)) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("glass-vmexec-{name}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for sub in ["jail", "guest/vsock"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let service = dir.join("guest/glass-vmguest.sock");
        let listener = UnixListener::bind(&service).unwrap();
        let vsock = dir.join("guest/vsock");
        let mut config = GuestConfig::new(
            dir.join("guest/runs"),
            Connector::Unix(format!("{}/{{port}}", vsock.display())),
        );
        change(&mut config);
        thread::spawn(move || vmguest::serve(listener, Arc::new(config)));
        let uds = dir.join("jail/vsock.sock");
        vsock_device(&uds, service, &vsock, &[PORT]);
        let task_socket = dir.join("task.sock");
        task_socket_standin(&task_socket);
        Self {
            dir,
            uds,
            task_socket,
        }
    }

    /// Where the task finds its socket in the guest
    /// (`/run/glasshouse/agent.sock` in a real one).
    fn agent_socket(&self) -> PathBuf {
        self.dir.join("guest/run/glasshouse/agent.sock")
    }

    fn workdir(&self) -> PathBuf {
        self.dir.join("guest/work/t-1")
    }

    fn request(&self, script: &str) -> ExecRequest {
        ExecRequest {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("GREETING".into(), "hello from the host".into()),
            ],
            workdir: self.workdir().display().to_string(),
            socket: SocketForward {
                path: self.agent_socket().display().to_string(),
                port: PORT,
            },
        }
    }

    fn helper(&self, request: &ExecRequest) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_glass-vmexec"));
        command
            .arg("--uds")
            .arg(&self.uds)
            .args(["--port", &EXEC_PORT.to_string()])
            .arg("--socket")
            .arg(&self.task_socket)
            .arg("--")
            .arg(request.line().unwrap());
        command
    }

    fn run(&self, script: &str) -> Output {
        run_helper(&mut self.helper(&self.request(script)))
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Firecracker's vsock device as both sides see it.
fn vsock_device(uds: &Path, service: PathBuf, guest_vsock: &Path, ports: &[u32]) {
    let host = UnixListener::bind(uds).unwrap();
    thread::spawn(move || {
        for conn in host.incoming() {
            let Ok(conn) = conn else { continue };
            let service = service.clone();
            thread::spawn(move || {
                // Only the exec service listens in this guest; a connection
                // to any other port is closed, as Firecracker does.
                if read_line(&conn).as_deref() != Some(&format!("CONNECT {EXEC_PORT}")) {
                    return;
                }
                let Ok(guest) = UnixStream::connect(&service) else {
                    return;
                };
                if (&conn).write_all(b"OK 1073741824\n").is_ok() {
                    exec::splice(conn, guest);
                }
            });
        }
    });
    for &port in ports {
        let guest_side = UnixListener::bind(guest_vsock.join(port.to_string())).unwrap();
        let host_side = PathBuf::from(format!("{}_{port}", uds.display()));
        thread::spawn(move || {
            for conn in guest_side.incoming() {
                let Ok(conn) = conn else { continue };
                // Nothing listening on the host: the guest's connection ends.
                if let Ok(host) = UnixStream::connect(&host_side) {
                    thread::spawn(move || exec::splice(conn, host));
                }
            }
        });
    }
}

fn read_line(conn: &UnixStream) -> Option<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < 64 {
        match (&*conn).read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => return String::from_utf8(line).ok(),
            Ok(1) => line.push(byte[0]),
            _ => return None,
        }
    }
    None
}

/// The person's world, as far as these tests go: says what it was asked.
fn task_socket_standin(path: &Path) {
    let listener = UnixListener::bind(path).unwrap();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            thread::spawn(move || {
                let mut reader = BufReader::new(&conn);
                let mut asked = String::new();
                let _ = reader.read_line(&mut asked);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let body = format!("the person's world got {}\n", asked.trim());
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = (&conn).write_all(reply.as_bytes());
            });
        }
    });
}

/// The longest a helper may take here: a test that hangs is a failure.
const DEADLINE: Duration = Duration::from_secs(30);

/// Runs the helper to its end, killing it at the deadline.
fn run_helper(command: &mut Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let drain = |mut from: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = from.read_to_end(&mut bytes);
            bytes
        })
    };
    let stdout = drain(Box::new(child.stdout.take().unwrap()));
    let stderr = drain(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("glass-vmexec ran past {DEADLINE:?}");
        }
        thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// Alive and not a zombie.
fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with(['Z', 'X']))
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn adr0065_a_task_in_the_guest_prints_and_exits_with_its_code() {
    let guest = Guest::new("exit");
    let output =
        guest.run(r#"echo "$GREETING"; echo "${HOME-no home}"; pwd; echo trouble >&2; exit 3"#);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        format!(
            "hello from the host\nno home\n{}\n",
            guest.workdir().display()
        )
    );
    assert_eq!(text(&output.stderr), "trouble\n");
}

#[test]
fn adr0065_a_task_killed_by_a_signal_exits_with_128_and_it() {
    let guest = Guest::new("signal");
    let output = guest.run("echo going; kill -TERM $$");
    assert_eq!(
        output.status.code(),
        Some(128 + 15),
        "{}",
        text(&output.stderr)
    );
    assert_eq!(text(&output.stdout), "going\n");
}

#[test]
fn adr0065_a_task_reaches_its_socket_through_the_forwarded_port() {
    let guest = Guest::new("socket");
    let socket = guest.agent_socket();
    let output = guest.run(&format!(
        "curl -sS --unix-socket {} http://glasshouse/hello",
        socket.display()
    ));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "the person's world got GET /hello HTTP/1.1\n"
    );
    // The socket was the task's alone: there's none outside its namespace,
    // and the helper's forwarding socket went with it.
    assert!(!socket.exists());
    assert!(!guest.dir.join(format!("jail/vsock.sock_{PORT}")).exists());
}

#[test]
fn adr0065_an_unprivileged_guest_service_gives_tasks_their_socket_too() {
    // As in development, where the service isn't root: each task gets a
    // user namespace too, mapped to root.
    let guest = Guest::with("userns", |config| config.user_namespace = true);
    let output = guest.run(&format!(
        "id -u; curl -sS --unix-socket {} http://glasshouse/again",
        guest.agent_socket().display()
    ));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "0\nthe person's world got GET /again HTTP/1.1\n"
    );
}

#[test]
fn adr0065_when_the_helper_goes_its_task_goes() {
    let guest = Guest::new("gone");
    let pids = guest.dir.join("pids");
    let script = format!(
        "sleep 300 & echo $$ $! > {}; echo started; wait",
        pids.display()
    );
    let mut helper = guest
        .helper(&guest.request(&script))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = helper.stdout.take().unwrap();
    let (said, heard) = mpsc::channel();
    thread::spawn(move || {
        let mut started = String::new();
        let _ = BufReader::new(stdout).read_line(&mut started);
        let _ = said.send(started);
    });
    let started = heard.recv_timeout(DEADLINE);
    if started.is_err() {
        let _ = helper.kill();
        let _ = helper.wait();
    }
    assert_eq!(started.as_deref(), Ok("started\n"));
    let pids: Vec<u32> = fs::read_to_string(&pids)
        .unwrap()
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    assert!(pids.iter().all(|&pid| alive(pid)));

    // The task ends (glassd kills the helper): its process group goes,
    // what it started in the background too (PA-2).
    helper.kill().unwrap();
    helper.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|&pid| alive(pid)) {
        assert!(Instant::now() < deadline, "the task outlived its helper");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn adr0065_a_program_that_isnt_there_isnt_started() {
    let guest = Guest::new("missing");
    let mut request = guest.request("");
    request.program = "/nonexistent/agent".into();
    let output = run_helper(&mut guest.helper(&request));
    assert_eq!(output.status.code(), Some(127));
    assert_eq!(
        text(&output.stderr),
        "glass-vmexec: There's no program at /nonexistent/agent.\n"
    );
}

#[test]
fn adr0065_without_an_exec_service_the_helper_says_so() {
    let guest = Guest::new("no-service");
    let request = guest.request("true").line().unwrap();
    let output = run_helper(
        Command::new(env!("CARGO_BIN_EXE_glass-vmexec"))
            .arg("--uds")
            .arg(&guest.uds)
            .args(["--port", "10001", "--socket"])
            .arg(&guest.task_socket)
            .args(["--", &request]),
    );
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(
        text(&output.stderr),
        "glass-vmexec: nothing in the computer answers on vsock port 10001: is its exec \
         service running?\n"
    );

    let wrong = run_helper(Command::new(env!("CARGO_BIN_EXE_glass-vmexec")).arg("--what"));
    assert_eq!(wrong.status.code(), Some(125));
    assert!(
        text(&wrong.stderr).contains("usage: glass-vmexec"),
        "{}",
        text(&wrong.stderr)
    );
}
