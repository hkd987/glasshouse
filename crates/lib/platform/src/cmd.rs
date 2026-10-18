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

fn render(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}
