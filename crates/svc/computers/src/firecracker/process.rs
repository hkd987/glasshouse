//! A computer's Firecracker, found by its pid: whether it's alive, and
//! stopping it. Linux's `/proc`, and `kill`; the agent host is Linux.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a killed Firecracker may take to go.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether `pid` is alive and is computer `id`'s Firecracker (`--id=<id>` on
/// its command line, as the jailer runs it), so a pid reused after the host
/// restarted isn't taken for it.
pub(crate) fn alive(pid: u32, id: &str) -> bool {
    if pid <= 1 {
        return false;
    }
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (name) S ...`: the name may hold anything, the state follows its
    // last `)`. A zombie has ended.
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next());
    if matches!(state, None | Some('Z' | 'X' | 'x')) {
        return false;
    }
    let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
    let flag = format!("--id={id}");
    args.contains(&flag.as_bytes())
        || args
            .windows(2)
            .any(|pair| pair[0] == b"--id" && pair[1] == id.as_bytes())
}

/// Kills it, if it's alive, and waits for it to go. Its state is in its
/// snapshot by then, or not wanted.
pub(crate) fn stop(pid: u32, id: &str) -> Result<(), String> {
    if !alive(pid, id) {
        return Ok(());
    }
    let killed = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !alive(pid, id) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let why = match killed {
        Ok(status) if status.success() => String::new(),
        Ok(status) => format!(" (kill {status})"),
        Err(e) => format!(" (kill: {e})"),
    };
    Err(format!(
        "Firecracker for {id} (process {pid}) didn't stop{why}."
    ))
}

/// The pid in a pid file, if it holds one. Firecracker owns its jail, so
/// only a small plain file is read (a link or a pipe could be anything).
pub(crate) fn read_pid(file: &Path) -> Option<u32> {
    let meta = fs::symlink_metadata(file).ok()?;
    if !meta.is_file() || meta.len() > 32 {
        return None;
    }
    let mut text = String::new();
    File::open(file)
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}
