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
