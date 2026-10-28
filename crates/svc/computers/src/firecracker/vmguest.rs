//! `glass-vmguest`, the exec service in a Firecracker computer's guest
//! ([`super::exec`] has the protocol): runs each task's process as the
//! host's helper asks, and streams back its output and how it ended.
//!
//! It opens no vsock socket itself: that takes code this repository keeps
//! to glass-platform's FFI. It listens on a Unix socket
//! ([`DEFAULT_LISTEN`]), which the guest image bridges to vsock port
//! [`super::exec::EXEC_PORT`]:
//!
//! ```text
//! socat VSOCK-LISTEN:10000,fork,reuseaddr UNIX-CONNECT:/run/glass-vmguest.sock
//! ```
//!
//! and it takes each connection to a task's socket out to the host through
//! a [`Connector`], by default a command run per connection with the
//! connection on its standard input and output ([`DEFAULT_CONNECT`]).
//!
//! A task's process gets a mount namespace of its own (`unshare --mount`,
//! from util-linux), in which the task's socket's folder is bound over the
//! folder of `socket.path`, and a process group of its own, which is killed
//! when the helper's connection ends and when the process does.
//!
//! The binary is `crates/bin/glass-vmguest`; the work is here so tests can
//! drive it beside the host's side.

use std::fs;
use std::io::{self, BufReader, Read};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::exec::{self, Ended, ExecRequest, Frame};
use super::http;

/// Where the service listens, bridged to vsock port 10000.
pub const DEFAULT_LISTEN: &str = "/run/glass-vmguest.sock";
/// Where each run's folder (its task socket) goes.
pub const DEFAULT_RUNS: &str = "/run/glass-vmguest";
/// How a connection to a task's socket reaches the host's port.
pub const DEFAULT_CONNECT: &str = "socat - VSOCK-CONNECT:2:{port}";
/// Where the tools it runs are: `unshare`, `mount`, `mkdir`, `kill`.
pub const DEFAULT_TOOLS: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

/// The most output one frame carries.
const CHUNK: usize = 64 * 1024;

/// Runs in the task's own mount namespace: gives it its socket and its
/// folder, then becomes it. `$1` the tools' path, `$2` the run's folder
/// (its socket), `$3` the folder the socket goes in, `$4` the working
/// folder, `$5` the file that says it got this far; then the program and
/// its arguments. Tools get the tools' path for themselves; the program
/// gets the task's environment as it is.
const ENTER: &str = r#"set -e
tools=$1 run=$2 dir=$3 work=$4 started=$5
shift 5
PATH=$tools mkdir -p "$dir" "$work"
PATH=$tools mount --bind "$run" "$dir"
cd "$work"
: > "$started"
exec "$@"
"#;

/// How a connection to a task's socket reaches the host's port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connector {
    /// A command, `{port}` in its arguments replaced, run per connection
    /// with the connection on its standard input and output.
    Command(Vec<String>),
    /// A Unix socket, `{port}` in its path replaced.
    Unix(String),
}

impl Connector {
    /// `unix:<path>`, or a command line split at spaces.
    pub fn parse(spec: &str) -> Result<Self, String> {
        if let Some(path) = spec.strip_prefix("unix:") {
            return Ok(Self::Unix(path.into()));
        }
        let argv: Vec<String> = spec.split_whitespace().map(str::to_owned).collect();
        if argv.is_empty() {
            return Err("A connector is a command or unix:<path>.".into());
        }
        Ok(Self::Command(argv))
    }
}

/// How the service runs tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestConfig {
    pub runs: PathBuf,
    pub connect: Connector,
    /// A `PATH` for its tools.
    pub tools: String,
    /// Also gives each task a user namespace, mapped to root: for running
    /// the service unprivileged in development. In a guest it is root.
    pub user_namespace: bool,
}

impl GuestConfig {
    /// For the user it runs as: root in a guest, anyone in development.
    pub fn new(runs: PathBuf, connect: Connector) -> Self {
        Self {
            runs,
            connect,
            tools: DEFAULT_TOOLS.into(),
            user_namespace: glass_platform::users::effective_uid() != 0,
        }
    }
}

/// Serves each connection on `listener` in a thread of its own.
pub fn serve(listener: UnixListener, config: Arc<GuestConfig>) -> io::Result<()> {
    fs::create_dir_all(&config.runs)?;
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let config = Arc::clone(&config);
        thread::spawn(move || handle(conn, &config));
    }
    Ok(())
}

/// One connection: a request, the task's output, how it ended.
pub fn handle(conn: UnixStream, config: &GuestConfig) {
    let Ok(read_half) = conn.try_clone() else {
        return;
    };
    let mut from = BufReader::new(read_half);
    let to = Arc::new(Mutex::new(conn));
    let last = match exec::read_request(&mut from) {
        Ok(request) => run(&request, config, from, &to),
        Err(e) => Frame::Fail(format!("That isn't a request: {e}.")),
    };
    send(&to, &last);
    let conn = to.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = conn.shutdown(Shutdown::Both);
}

/// The run's number, naming its folder.
static RUNS: AtomicU64 = AtomicU64::new(0);

/// Runs one task; its last frame.
fn run(
    request: &ExecRequest,
    config: &GuestConfig,
    watch: BufReader<UnixStream>,
    to: &Arc<Mutex<UnixStream>>,
) -> Frame {
    let n = RUNS.fetch_add(1, Ordering::Relaxed);
    let folder = config.runs.join(n.to_string());
    let started = config.runs.join(format!("{n}.started"));
    let last = match prepare(request, config, &folder) {
        Ok((socket_dir, forward)) => {
            let last = match spawn(request, config, &folder, &socket_dir, &started) {
                Ok(child) => watch_over(child, request, config, watch, to, &started),
                Err(why) => Frame::Fail(why),
            };
            forward.stop();
            last
        }
        Err(why) => Frame::Fail(why),
    };
    let _ = fs::remove_dir_all(&folder);
    let _ = fs::remove_file(&started);
    last
}

/// Checks the request and makes the run's folder with the task's socket;
/// the folder the socket goes in, and its forwarding.
fn prepare(
    request: &ExecRequest,
    config: &GuestConfig,
    folder: &Path,
) -> Result<(PathBuf, Forward), String> {
    let program = Path::new(&request.program);
    if program.is_absolute() && !program.is_file() {
        return Err(format!("There's no program at {}.", request.program));
    }
    if !Path::new(&request.workdir).is_absolute() {
        return Err(format!("{} isn't an absolute path.", request.workdir));
    }
    let socket = Path::new(&request.socket.path);
    let (Some(dir), Some(name), true) = (socket.parent(), socket.file_name(), socket.is_absolute())
    else {
        return Err(format!(
            "{} isn't a path for a socket.",
            request.socket.path
        ));
    };
    let _ = fs::remove_dir_all(folder);
    fs::create_dir_all(folder).map_err(|e| format!("Couldn't make {}: {e}.", folder.display()))?;
    let forward = Forward::start(
        &folder.join(name),
        config.connect.clone(),
        request.socket.port,
    )?;
    Ok((dir.to_owned(), forward))
}

fn spawn(
    request: &ExecRequest,
    config: &GuestConfig,
    folder: &Path,
    socket_dir: &Path,
    started: &Path,
) -> Result<std::process::Child, String> {
    let mut command = Command::new(tool("unshare", &config.tools));
    if config.user_namespace {
        command.args(["--user", "--map-root-user"]);
    }
    command
        .args([
            "--mount",
            "--propagation",
            "private",
            "--",
            "/bin/sh",
            "-c",
            ENTER,
        ])
        .arg("glass-task")
        .arg(&config.tools)
        .arg(folder)
        .arg(socket_dir)
        .arg(&request.workdir)
        .arg(started)
        .arg(&request.program)
        .args(&request.args)
        .env_clear()
        .envs(request.env.iter().map(|(name, value)| (name, value)))
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
        .spawn()
        .map_err(|e| format!("Couldn't start {}: {e}.", request.program))
}

/// Streams its output while it runs, kills its process group if the helper
/// goes, and when it ends; its last frame.
fn watch_over(
    mut child: std::process::Child,
    request: &ExecRequest,
    config: &GuestConfig,
    mut watch: BufReader<UnixStream>,
    to: &Arc<Mutex<UnixStream>>,
    started: &Path,
) -> Frame {
    let group = child.id();
    let gone = Arc::new(AtomicBool::new(false));
    let helper_gone = Arc::clone(&gone);
    // Nothing more comes from the helper: the connection ending is the
    // helper going. It's shut down when the run is over, which ends this.
    thread::spawn(move || {
        let mut sink = [0u8; 256];
        while matches!(watch.read(&mut sink), Ok(n) if n > 0) {}
        helper_gone.store(true, Ordering::SeqCst);
    });
    let pumps = [
        pump(child.stdout.take(), true, Arc::clone(to)),
        pump(child.stderr.take(), false, Arc::clone(to)),
    ];
    let mut killed = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        // Killed while the leader is unreaped, so its group's id is still
        // its own.
        if !killed && gone.load(Ordering::SeqCst) {
            kill_group(group, &config.tools);
            killed = true;
        }
        thread::sleep(Duration::from_millis(10));
    };
    // What it left running goes with it (PA-2), and lets go of its output.
    kill_group(group, &config.tools);
    for pump in pumps {
        let _ = pump.join();
    }
    match status {
        Some(status) if started.exists() => Frame::Exit(ended(status)),
        Some(status) => Frame::Fail(format!(
            "Couldn't give {} a view of its own ({status}).",
            request.program
        )),
        None => Frame::Fail(format!("Lost track of {}.", request.program)),
    }
}

fn ended(status: ExitStatus) -> Ended {
    match (status.code(), status.signal()) {
        (Some(code), _) => Ended::Code(code),
        (None, Some(signal)) => Ended::Signal(signal),
        (None, None) => Ended::Code(1),
    }
}

/// Sends what `source` writes as frames until it closes.
fn pump(
    source: Option<impl Read + Send + 'static>,
    out: bool,
    to: Arc<Mutex<UnixStream>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let Some(mut source) = source else { return };
        let mut buf = vec![0; CHUNK];
        loop {
            match source.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    send(
                        &to,
                        &if out {
                            Frame::Out(data)
                        } else {
                            Frame::Err(data)
                        },
                    );
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    })
}

/// Sends a frame, if the helper is still there to take it.
fn send(to: &Mutex<UnixStream>, frame: &Frame) {
    let mut conn = to.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = exec::write_frame(&mut *conn, frame);
}

fn kill_group(group: u32, tools: &str) {
    let _ = Command::new(tool("kill", tools))
        .args(["-s", "KILL", "--", &format!("-{group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A tool from the tools' path, or its name to look up.
fn tool(name: &str, tools: &str) -> PathBuf {
    tools
        .split(':')
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| name.into())
}

/// The task's socket in the guest: each connection to it goes out to the
/// host's port through the connector.
struct Forward {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    accepting: JoinHandle<()>,
}

impl Forward {
    fn start(path: &Path, connect: Connector, port: u32) -> Result<Self, String> {
        let listener =
            http::bind(path).map_err(|e| format!("Couldn't make {}: {e}.", path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let accepting = thread::spawn(move || {
            for conn in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let connect = connect.clone();
                thread::spawn(move || connect_out(conn, &connect, port));
            }
        });
        Ok(Self {
            path: path.to_owned(),
            stop,
            accepting,
        })
    }

    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        if http::connect(&self.path).is_ok() {
            let _ = self.accepting.join();
        }
    }
}

/// Takes one connection to the task's socket out to the host's port.
fn connect_out(conn: UnixStream, connect: &Connector, port: u32) {
    let port = port.to_string();
    match connect {
        Connector::Unix(template) => {
            if let Ok(out) = http::connect(Path::new(&template.replace("{port}", &port))) {
                exec::splice(conn, out);
            }
        }
        Connector::Command(argv) => {
            let argv: Vec<String> = argv.iter().map(|a| a.replace("{port}", &port)).collect();
            let Some((program, args)) = argv.split_first() else {
                return;
            };
            let Ok(mut child) = Command::new(program)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            else {
                return;
            };
            let (Some(mut stdin), Some(mut stdout), Ok(reading)) =
                (child.stdin.take(), child.stdout.take(), conn.try_clone())
            else {
                let _ = child.kill();
                let _ = child.wait();
                return;
            };
            let forth = thread::spawn(move || {
                exec::copy(&mut &reading, &mut stdin);
                // Dropping its input tells the connector there's no more.
            });
            exec::copy(&mut stdout, &mut &conn);
            // The host's side is done: so is this connection.
            let _ = conn.shutdown(Shutdown::Both);
            let _ = forth.join();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn adr0065_a_connector_is_a_command_or_a_unix_socket() {
        assert_eq!(
            Connector::parse(DEFAULT_CONNECT),
            Ok(Connector::Command(vec![
                "socat".into(),
                "-".into(),
                "VSOCK-CONNECT:2:{port}".into()
            ]))
        );
        assert_eq!(
            Connector::parse("unix:/run/vsock/{port}"),
            Ok(Connector::Unix("/run/vsock/{port}".into()))
        );
        assert!(Connector::parse("  ").is_err());
    }

    #[test]
    fn adr0065_a_command_connector_carries_a_task_socket_connection() {
        // The command stands in for `socat - VSOCK-CONNECT:2:{port}`: what
        // the task sends goes to its input, what it says comes back.
        let dir = std::env::temp_dir().join(format!("glass-vmguest-unit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("agent.sock");
        let connect = Connector::Command(vec![
            "/bin/sh".into(),
            "-c".into(),
            r#"read asked; echo "host port {port} got $asked""#.into(),
        ]);
        let forward = Forward::start(&socket, connect, 20_000).unwrap();
        let conn = UnixStream::connect(&socket).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        (&conn).write_all(b"ping\n").unwrap();
        let mut answer = String::new();
        (&conn).read_to_string(&mut answer).unwrap();
        assert_eq!(answer, "host port 20000 got ping\n");
        forward.stop();
        let _ = fs::remove_dir_all(&dir);
    }
}
