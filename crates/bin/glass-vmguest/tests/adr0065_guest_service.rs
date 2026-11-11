//! ADR 0065 §10: `glass-vmguest`, this crate's binary, serving on a Unix
//! socket as it does in a guest behind the vsock bridge, spoken to directly.
//! The whole way through, with `glass-vmexec` and a stand-in vsock device,
//! is `glass-vmexec`'s test.
//!
//! Needs `unshare --mount` (root, or user namespaces).

#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use glass_computers::firecracker::exec::{self, Ended, ExecRequest, Frame, SocketForward};

struct Service {
    dir: PathBuf,
    child: Child,
}

impl Service {
    fn start(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("glass-vmguest-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let listen = dir.join("guest.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_glass-vmguest"))
            .arg("--listen")
            .arg(&listen)
            .arg("--runs")
            .arg(dir.join("runs"))
            .args(["--connect", "unix:/nonexistent/{port}"])
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !listen.exists() {
            assert!(Instant::now() < deadline, "glass-vmguest didn't listen");
            thread::sleep(Duration::from_millis(10));
        }
        Self { dir, child }
    }

    /// Runs a script; its standard output, standard error and how it ended.
    fn run(&self, script: &str) -> (String, String, Frame) {
        let conn = UnixStream::connect(self.dir.join("guest.sock")).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let request = ExecRequest {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            workdir: self.dir.join("work").display().to_string(),
            socket: SocketForward {
                path: self.dir.join("run/agent.sock").display().to_string(),
                port: 20_000,
            },
        };
        (&conn)
            .write_all(format!("{}\n", request.line().unwrap()).as_bytes())
            .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut from = BufReader::new(&conn);
        loop {
            match exec::read_frame(&mut from).unwrap() {
                Some(Frame::Out(data)) => out.extend(data),
                Some(Frame::Err(data)) => err.extend(data),
                Some(last) => {
                    let text = |bytes| String::from_utf8(bytes).unwrap();
                    return (text(out), text(err), last);
                }
                None => panic!("glass-vmguest closed the connection before the task ended"),
            }
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn adr0065_the_guest_service_runs_a_task_and_says_how_it_ended() {
    let service = Service::start("runs");
    let (out, err, last) = service.run("echo hi; echo oops >&2; exit 4");
    assert_eq!((out.as_str(), err.as_str()), ("hi\n", "oops\n"));
    assert_eq!(last, Frame::Exit(Ended::Code(4)));

    // Another on the same service, as tasks come one after another.
    let (out, _, last) = service.run("pwd");
    assert_eq!(out, format!("{}\n", service.dir.join("work").display()));
    assert_eq!(last, Frame::Exit(Ended::Code(0)));
    // Each run's folder goes with it.
    assert_eq!(fs::read_dir(service.dir.join("runs")).unwrap().count(), 0);
}

/// Whether the file at `path` is still being written: its content now and
/// a little later differ.
fn still_written(path: &std::path::Path) -> bool {
    let before = fs::read(path).unwrap_or_default();
    thread::sleep(Duration::from_millis(400));
    fs::read(path).unwrap_or_default() != before
}

// Audit 6.6: a task's processes were ended by killing its process group,
// so one that had left the group outlived the task.
#[test]
fn pa2_what_a_task_started_goes_with_it_whatever_group_it_moved_to() {
    let service = Service::start("escape");
    let beat = service.dir.join("work/beat");
    // A process in a session of its own, writing the time every 50 ms,
    // and the task ends at once. `setsid --fork` leaves the group too.
    let script = format!(
        "setsid --fork sh -c 'while :; do date +%s%N > {beat}; sleep 0.05; done' \
         >/dev/null 2>&1 </dev/null; sleep 0.3; test -s {beat} && echo beating",
        beat = beat.display()
    );
    let (out, err, last) = service.run(&script);
    assert_eq!(
        (out.as_str(), last),
        ("beating\n", Frame::Exit(Ended::Code(0))),
        "{err}"
    );
    assert!(!still_written(&beat), "it outlived the task");

    // One that also holds the task's output: the task's end isn't kept
    // waiting for it (30 s here, and for good before).
    let asked = Instant::now();
    let (out, _, last) = service.run("setsid --fork sleep 600; echo left one behind");
    assert_eq!(
        (out.as_str(), last),
        ("left one behind\n", Frame::Exit(Ended::Code(0)))
    );
    assert!(
        asked.elapsed() < Duration::from_secs(10),
        "{:?}",
        asked.elapsed()
    );
}

#[test]
fn a_task_is_not_its_namespaces_first_process_and_sees_only_its_own() {
    let service = Service::start("namespace");
    // The first process is not ended by a signal it has no handler for:
    // a task that ends itself with one must still end, and say which.
    let (out, _, last) = service.run("echo $$; kill -TERM $$; echo still here");
    assert_ne!(out, "1\n");
    assert!(!out.contains("still here"), "{out}");
    assert_eq!(last, Frame::Exit(Ended::Code(128 + 15)));
    // In /proc, its own few processes: not the service's, nor this test's.
    let (out, err, _) =
        service.run("ls /proc | grep -c '^[0-9]'; test -d /proc/1 && cat /proc/1/comm");
    let mut lines = out.lines();
    let count: usize = lines
        .next()
        .unwrap_or_default()
        .parse()
        .unwrap_or(usize::MAX);
    assert!(count <= 6, "{count} processes in view: {out} {err}");
    assert_eq!(
        lines.next(),
        Some("sh"),
        "the first is the one of ours that waits"
    );
}

#[test]
fn adr0065_the_guest_service_refuses_what_isnt_a_request() {
    let service = Service::start("refuses");
    let conn = UnixStream::connect(service.dir.join("guest.sock")).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    (&conn).write_all(b"{\"program\":\"/bin/true\"}\n").unwrap();
    let last = exec::read_frame(&mut BufReader::new(&conn)).unwrap();
    assert!(
        matches!(&last, Some(Frame::Fail(why)) if why.starts_with("That isn't a request")),
        "{last:?}"
    );

    let wrong = Command::new(env!("CARGO_BIN_EXE_glass-vmguest"))
        .arg("--what")
        .output()
        .unwrap();
    assert_eq!(wrong.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("usage: glass-vmguest"));
}
