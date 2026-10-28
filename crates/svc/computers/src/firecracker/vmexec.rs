//! `glass-vmexec`, the host's side of a Firecracker computer's exec service
//! ([`super::exec`] has the protocol): runs one task's process in the
//! guest, passes its output through as it comes, and exits as it did.
//!
//! The binary is `crates/bin/glass-vmexec`; the work is here so tests can
//! drive it beside the guest's side.

use std::fs::{self, Permissions};
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, chown};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use super::exec::{self, ExecRequest, Frame};
use super::http;

/// It couldn't run the task: its arguments, no exec service in the guest,
/// or the connection broke before the task ended.
pub const HELPER_FAILED: u8 = 125;
/// The guest couldn't start the process.
pub const NOT_STARTED: u8 = 127;

const USAGE: &str = "usage: glass-vmexec --uds <vsock socket> --port <port> \
                     --socket <task socket> -- <request>";

/// Runs the task its arguments (without the program's name) describe, its
/// output to `out` and `err`; the exit code to exit with.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    match parse(args).and_then(|args| run_task(&args, out, err)) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "glass-vmexec: {e}");
            HELPER_FAILED
        }
    }
}

#[derive(Debug)]
struct Args {
    uds: PathBuf,
    port: u32,
    socket: PathBuf,
    request: ExecRequest,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let (mut uds, mut port, mut socket) = (None, None, None);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || {
            rest.next()
                .ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--uds" => uds = Some(PathBuf::from(value()?)),
            "--socket" => socket = Some(PathBuf::from(value()?)),
            "--port" => {
                let given = value()?;
                port = Some(
                    given
                        .parse()
                        .map_err(|_| format!("{given:?} isn't a port"))?,
                );
            }
            "--" => break,
            other => return Err(format!("{other:?} isn't an argument it takes\n{USAGE}")),
        }
    }
    let (Some(uds), Some(port), Some(socket), Some(line), None) =
        (uds, port, socket, rest.next(), rest.next())
    else {
        return Err(USAGE.into());
    };
    let request = serde_json::from_str(line).map_err(|e| format!("that isn't a request: {e}"))?;
    let absolute = |path: PathBuf| std::path::absolute(&path).unwrap_or(path);
    Ok(Args {
        uds: absolute(uds),
        port,
        socket: absolute(socket),
        request,
    })
}

fn run_task(args: &Args, out: &mut dyn Write, err: &mut dyn Write) -> Result<u8, String> {
    let forward = Forward::start(&args.uds, args.request.socket.port, &args.socket)?;
    let result = exchange(args, out, err);
    forward.stop();
    result
}

/// Asks the guest to run the request and passes on what comes back.
fn exchange(args: &Args, out: &mut dyn Write, err: &mut dyn Write) -> Result<u8, String> {
    let stream = connect_guest(&args.uds, args.port)?;
    let line = args.request.line()?;
    (&stream)
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| format!("couldn't send the computer the task: {e}"))?;
    // Nothing more is sent, and the connection stays open: its end is how
    // the guest learns the task is over.
    let mut from = BufReader::new(&stream);
    loop {
        match exec::read_frame(&mut from) {
            // Where the output goes may have closed; the task goes on.
            Ok(Some(Frame::Out(data))) => {
                let _ = out.write_all(&data).and_then(|()| out.flush());
            }
            Ok(Some(Frame::Err(data))) => {
                let _ = err.write_all(&data).and_then(|()| err.flush());
            }
            Ok(Some(Frame::Exit(ended))) => return Ok(ended.exit_code()),
            Ok(Some(Frame::Fail(why))) => {
                let _ = writeln!(err, "glass-vmexec: {why}");
                return Ok(NOT_STARTED);
            }
            Ok(None) => {
                return Err("the computer closed the connection before the task ended".into());
            }
            Err(e) => return Err(format!("the computer's answer broke off: {e}")),
        }
    }
}

/// Connects to the guest's port through Firecracker's vsock socket.
fn connect_guest(uds: &Path, port: u32) -> Result<UnixStream, String> {
    // Firecracker owns its jail: its socket, not a link to another one.
    if !fs::symlink_metadata(uds).is_ok_and(|m| m.file_type().is_socket()) {
        return Err(format!("{} isn't a computer's vsock socket", uds.display()));
    }
    let stream = http::connect(uds)
        .map_err(|e| format!("couldn't reach the computer at {}: {e}", uds.display()))?;
    (&stream)
        .write_all(format!("CONNECT {port}\n").as_bytes())
        .map_err(|e| format!("couldn't reach the computer at {}: {e}", uds.display()))?;
    // Byte by byte, so nothing after the answer is read here.
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while answer.len() < 64 {
        match (&stream).read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => break,
            Ok(1) => answer.push(byte[0]),
            _ => {
                return Err(format!(
                    "nothing in the computer answers on vsock port {port}: is its exec service \
                     running?"
                ));
            }
        }
    }
    if !answer.starts_with(b"OK ") {
        let answer = String::from_utf8_lossy(&answer);
        return Err(format!("the computer answered {answer:?} instead of OK"));
    }
    Ok(stream)
}

/// The task socket's forwarding: Firecracker hands the guest's connections
/// to host port `P` to `<vsock socket>_P`, which relays each to the task
/// socket.
struct Forward {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    accepting: JoinHandle<()>,
}

impl Forward {
    fn start(uds: &Path, port: u32, task_socket: &Path) -> Result<Self, String> {
        let mut name = uds.file_name().unwrap_or_default().to_owned();
        name.push(format!("_{port}"));
        let path = uds.with_file_name(name);
        match fs::symlink_metadata(&path) {
            // Left by a helper that was killed.
            Ok(meta) if meta.file_type().is_socket() => {
                let _ = fs::remove_file(&path);
            }
            Ok(_) => return Err(format!("{} is there and isn't a socket", path.display())),
            Err(_) => {}
        }
        let listener =
            http::bind(&path).map_err(|e| format!("couldn't listen at {}: {e}", path.display()))?;
        // Firecracker connects to it as the jail's owner, and no one else may.
        let folder = fs::metadata(path.parent().unwrap_or(Path::new("/")))
            .map_err(|e| format!("couldn't look at {}'s folder: {e}", path.display()))?;
        fs::set_permissions(&path, Permissions::from_mode(0o600))
            .and_then(|()| chown(&path, Some(folder.uid()), Some(folder.gid())))
            .map_err(|e| format!("couldn't give {} to the jail's owner: {e}", path.display()))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let task_socket = task_socket.to_owned();
        let accepting = thread::spawn(move || {
            for conn in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let task_socket = task_socket.clone();
                thread::spawn(move || {
                    // No task socket: the guest's connection just closes.
                    if let Ok(task) = http::connect(&task_socket) {
                        exec::splice(conn, task);
                    }
                });
            }
        });
        Ok(Self {
            path,
            stop,
            accepting,
        })
    }

    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accepting thread to see it's over.
        if http::connect(&self.path).is_ok() {
            let _ = self.accepting.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}
