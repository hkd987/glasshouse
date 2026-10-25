//! Running base-system tools (`jail`, `zfs`, `mount`) with useful errors.
//!
//! We drive FreeBSD through its admin commands rather than raw syscalls where
//! a stable command exists: they are the documented interfaces, every
//! operator can reproduce what the daemon did, and no `unsafe` is needed.

use std::process::Command;

use crate::{PlatformError, Result};

/// Runs `program args…` and returns its stdout. Fails with stderr on a
/// non-zero exit.
pub fn run(program: &str, args: &[&str]) -> Result<String> {
    let rendered = render(program, args);
    tracing::debug!(command = %rendered, "running");
    let output = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin")
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

/// [`run`] with `input` on the program's standard input, such as a
/// password for `pw -h 0`. The input never appears in an error.
pub fn run_with_input(program: &str, args: &[&str], input: &[u8]) -> Result<String> {
    use std::io::Write;
    use std::process::Stdio;

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
    let output = child
        .wait_with_output()
        .map_err(|e| PlatformError::io(format!("waiting for `{rendered}`"), e))?;
    if output.status.success() {
        written.map_err(|e| PlatformError::io(format!("writing to `{rendered}`"), e))?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(PlatformError::Command {
            command: rendered,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
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
