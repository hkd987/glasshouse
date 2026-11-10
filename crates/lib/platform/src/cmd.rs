//! Running base-system tools (`jail`, `zfs`, `mount`) with useful errors.
//!
//! We drive FreeBSD through its admin commands rather than raw syscalls where
//! a stable command exists: they are the documented interfaces, every
//! operator can reproduce what the daemon did, and no `unsafe` is needed.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::{PlatformError, Result};

/// How long a command may take unless its caller says otherwise
/// ([`run_within`]). These are admin commands that answer in milliseconds
/// to seconds; the slowest here by design is a batch of snapshots
/// destroyed at once, at tens of seconds on a full pool. Five minutes is
/// far past all of them, and an end for one that never answers: a stalled
/// `zfs` held a person's writing turn for good, and a `jail -r` that hung
/// a task's teardown.
pub const LIMIT: Duration = Duration::from_secs(300);
/// How long what a program printed is waited for once it has exited. Its
/// pipes close with it, unless a process it started keeps them: a service
/// script's daemon, which holds them for as long as it runs. Waiting for
/// that end (as `Command::output` does) never returned.
const DRAIN: Duration = Duration::from_millis(250);

/// Runs `program args…` and returns its stdout. Fails with stderr on a
/// non-zero exit, and when it hasn't exited within [`LIMIT`] (it is
/// stopped).
pub fn run(program: &str, args: &[&str]) -> Result<String> {
    run_within(program, args, LIMIT)
}

/// [`run`], given `limit` to finish in.
pub fn run_within(program: &str, args: &[&str], limit: Duration) -> Result<String> {
    let rendered = render(program, args);
    tracing::debug!(command = %rendered, "running");
    let child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PlatformError::io(format!("cannot run `{rendered}`"), e))?;
    finish(child, rendered, limit)
}

/// [`run`] with `input` on the program's standard input, such as a
/// password for `pw -h 0`. The input never appears in an error.
pub fn run_with_input(program: &str, args: &[&str], input: &[u8]) -> Result<String> {
    use std::io::Write;

    let rendered = render(program, args);
    tracing::debug!(command = %rendered, "running with input");
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PlatformError::io(format!("cannot run `{rendered}`"), e))?;
    // Written, then closed; the child is waited for even when writing
    // failed (it exited early), so none is left behind.
    let written = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(input));
    let output = finish(child, rendered.clone(), LIMIT)?;
    written.map_err(|e| PlatformError::io(format!("writing to `{rendered}`"), e))?;
    Ok(output)
}

/// What a program printed on one of its streams, gathered as it comes, and
/// word that the stream ended.
struct Printed {
    bytes: Arc<Mutex<Vec<u8>>>,
    ended: mpsc::Receiver<()>,
}

impl Printed {
    fn of(stream: Option<impl Read + Send + 'static>) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (done, ended) = mpsc::channel();
        if let Some(mut stream) = stream {
            let bytes = Arc::clone(&bytes);
            // A thread of its own: reading both streams from one would
            // stall a program that fills the other's pipe.
            std::thread::spawn(move || {
                let mut buf = [0; 8192];
                while let Ok(n @ 1..) = stream.read(&mut buf) {
                    bytes
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(&buf[..n]);
                }
                let _ = done.send(());
            });
        }
        Self { bytes, ended }
    }

    /// Whether the stream ended within `wait`.
    fn ended_within(&self, wait: Duration) -> bool {
        !matches!(
            self.ended.recv_timeout(wait),
            Err(RecvTimeoutError::Timeout)
        )
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned()
    }
}

/// Waits for `child` to exit, within `limit`, and returns what it printed.
fn finish(mut child: Child, rendered: String, limit: Duration) -> Result<String> {
    let deadline = Instant::now() + limit;
    let out = Printed::of(child.stdout.take());
    let err = Printed::of(child.stderr.take());
    // Its output ending is the usual sign it is done, and is heard the
    // moment it happens; a program that closes its output and runs on, or
    // one whose child keeps the pipe, is looked at every few milliseconds.
    let mut out_ended = false;
    let status: ExitStatus = loop {
        if !out_ended {
            out_ended = out.ended_within(Duration::from_millis(10));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(PlatformError::io(format!("waiting for `{rendered}`"), e)),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PlatformError::Command {
                command: rendered,
                status: format!("no answer in {} s; stopped", limit.as_secs()),
                stderr: err.text().trim().to_owned(),
            });
        }
        if out_ended {
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    // The rest of what it printed, for as long as that can take.
    let drained = Instant::now() + DRAIN;
    for printed in [&out, &err] {
        printed.ended_within(drained.saturating_duration_since(Instant::now()));
    }
    if status.success() {
        Ok(out.text())
    } else {
        Err(PlatformError::Command {
            command: rendered,
            status: status.to_string(),
            stderr: err.text().trim().to_owned(),
        })
    }
}

/// [`run`] with the file at `input` on the program's standard input, such
/// as an image for `bectl import`: it's read as the program reads, never
/// whole into memory.
pub fn run_with_file(program: &str, args: &[&str], input: &std::path::Path) -> Result<String> {
    let rendered = render(program, args);
    tracing::debug!(command = %rendered, input = %input.display(), "running with a file");
    let file = std::fs::File::open(input)
        .map_err(|e| PlatformError::io(format!("opening {}", input.display()), e))?;
    let output = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin")
        .stdin(file)
        .output()
        .map_err(|e| PlatformError::io(format!("cannot run `{rendered}`"), e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(PlatformError::Command {
            command: rendered,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn render(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_returns_what_it_printed_or_why_it_failed() {
        assert_eq!(run("echo", &["hello", "there"]).unwrap(), "hello there\n");
        assert_eq!(run("true", &[]).unwrap(), "");
        let failed = run("sh", &["-c", "echo out; echo 'no such pool' >&2; exit 3"]).unwrap_err();
        let PlatformError::Command {
            command,
            status,
            stderr,
        } = failed
        else {
            panic!("{failed}");
        };
        assert!(command.starts_with("sh -c"));
        assert!(status.contains('3'), "{status}");
        assert_eq!(stderr, "no such pool");
        assert!(matches!(
            run("/no/such/program", &[]),
            Err(PlatformError::Io { .. })
        ));
        // More than a pipe holds, on both streams at once.
        let big = run(
            "sh",
            &[
                "-c",
                "head -c 300000 /dev/zero | tr '\\0' e >&2; head -c 300000 /dev/zero | tr '\\0' o",
            ],
        )
        .unwrap();
        assert_eq!(big.len(), 300_000);
        assert_eq!(run_with_input("cat", &[], b"typed in").unwrap(), "typed in");
    }

    #[test]
    fn a_command_that_never_answers_is_stopped_at_its_limit() {
        let started = Instant::now();
        let stuck = run_within("sleep", &["30"], Duration::from_millis(300)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            matches!(&stuck, PlatformError::Command { status, .. } if status.contains("no answer")),
            "{stuck}"
        );
    }

    #[test]
    fn a_command_whose_child_keeps_its_output_open_still_returns() {
        // What a service script does when the daemon it starts inherits
        // its output: the script exits and the pipe never closes. Waiting
        // for the pipe, as `Command::output` does, waited for the daemon.
        let started = Instant::now();
        let out = run("sh", &["-c", "sleep 20 & echo started"]).unwrap();
        assert_eq!(out, "started\n");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited {:?} for a child's output to end",
            started.elapsed()
        );
    }
}
