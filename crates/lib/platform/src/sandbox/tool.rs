//! Running a tool once on files glassd prepared, confined as tightly as the
//! backend can (ADR 0032): glassd bundles the apps people make this way, so
//! the bundler reads only the build folder and writes only there.
//!
//! In a jail ([`super::JailSandbox`]) the build folder is the whole
//! filesystem: the tool's own folder mounted read-only beside it, no
//! devices, no network, an unprivileged user, removed as the tool exits
//! (and its mount with it). On the
//! development host ([`super::HostSandbox`]) it is an ordinary child
//! process, NOT A SECURITY BOUNDARY, like everything there.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::{PlatformError, Result};

/// One run of a tool.
#[derive(Debug, Clone)]
pub struct ToolRun {
    /// A name for the run: letters, digits and dashes (it names the jail).
    pub name: String,
    /// The tool: a program that needs nothing beside it (statically linked).
    pub program: PathBuf,
    /// The build folder: the tool's working folder, and in a jail its root.
    /// The user the tool runs as must be able to read and write it.
    pub dir: PathBuf,
    /// Arguments, with paths relative to `dir`.
    pub args: Vec<String>,
    /// How long it may run.
    pub limit: Duration,
    /// Who it runs as in a jail (the host backend runs it as glassd).
    pub user: String,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub success: bool,
    /// What it said went wrong, at most 16 KB.
    pub stderr: String,
    /// It was stopped at its time limit.
    pub timed_out: bool,
}

const MAX_STDERR: u64 = 16 * 1024;

/// Waits for `child` until `limit`, stopping it with `stop` after that.
pub(crate) fn wait(
    mut child: Child,
    limit: Duration,
    stop: impl FnOnce(&mut Child),
) -> Result<ToolOutput> {
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| PlatformError::io("waiting for a tool", e))?
        {
            break Some(status);
        }
        if started.elapsed() >= limit {
            stop(&mut child);
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    if let Some(err) = child.stderr.take() {
        let _ = err.take(MAX_STDERR).read_to_string(&mut stderr);
    }
    Ok(ToolOutput {
        success: status.is_some_and(|s| s.success()),
        stderr,
        timed_out: status.is_none(),
    })
}

/// The tool as an ordinary child process in its folder (the host backend).
pub(crate) fn run_unconfined(run: &ToolRun) -> Result<ToolOutput> {
    super::check_name(&run.name)?;
    let child = Command::new(&run.program)
        .args(&run.args)
        .env_clear()
        .current_dir(&run.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PlatformError::io(format!("running {}", run.program.display()), e))?;
    wait(child, run.limit, |child| {
        let _ = child.kill();
    })
}

/// The arguments to `jail(8)` that run `run` in a jail of its own, rooted
/// at its folder: the tool's folder mounted read-only at `/.tool`, no
/// devices, no addresses, nothing a jail may be allowed. It goes when the
/// tool exits; `jail(8)` leaves the mount, which [`run_jailed`] removes.
pub(crate) fn jail_args(run: &ToolRun) -> Result<Vec<String>> {
    super::check_name(&run.name)?;
    let (Some(tool_dir), Some(file)) = (run.program.parent(), run.program.file_name()) else {
        return Err(PlatformError::Unsafe(format!(
            "{} isn't a program's path",
            run.program.display()
        )));
    };
    let path = |p: &std::path::Path| {
        let text = p.to_string_lossy();
        // Each is one fstab field and one jail parameter.
        if text
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\')
        {
            Err(PlatformError::Unsafe(format!(
                "{text:?} has spaces or quotes"
            )))
        } else {
            Ok(text.into_owned())
        }
    };
    let (root, tool_dir) = (path(&run.dir)?, path(tool_dir)?);
    let file = path(std::path::Path::new(file))?;
    let mut args = vec![
        "-u".to_owned(),
        run.user.clone(),
        "-c".to_owned(),
        format!("name={}", run.name),
        format!("path={root}"),
        format!("host.hostname={}", run.name),
        "ip4=disable".to_owned(),
        "ip6=disable".to_owned(),
        "children.max=0".to_owned(),
        "securelevel=3".to_owned(),
        "enforce_statfs=2".to_owned(),
        "allow.raw_sockets=0".to_owned(),
        "allow.mount=0".to_owned(),
        "allow.sysvipc=0".to_owned(),
        format!("mount={tool_dir} {root}/.tool nullfs ro 0 0"),
        format!("command=/.tool/{file}"),
    ];
    args.extend(run.args.iter().cloned());
    Ok(args)
}

/// Runs `run` in a jail of its own; stopping it removes the jail. Either
/// way the tool's mount goes before this returns, so the folder can be
/// removed.
pub(crate) fn run_jailed(run: &ToolRun) -> Result<ToolOutput> {
    let args = jail_args(run)?;
    let mount_point = run.dir.join(".tool");
    std::fs::create_dir_all(&mount_point)
        .map_err(|e| PlatformError::io("making the tool's mount point", e))?;
    let child = Command::new("/usr/sbin/jail")
        .args(&args)
        .env_clear()
        .env("PATH", "/sbin:/bin:/usr/sbin:/usr/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PlatformError::io("running jail(8)", e))?;
    let name = run.name.clone();
    let out = wait(child, run.limit, move |child| {
        let _ = crate::cmd::run("jail", &["-r", &name]);
        let _ = child.kill();
    });
    // Not mounted (jail(8) failed before mounting) is fine.
    let _ = crate::cmd::run("umount", &["-f", &mount_point.to_string_lossy()]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> ToolRun {
        ToolRun {
            name: "glass-build-1".into(),
            program: "/usr/local/lib/glasshouse/bundler/esbuild".into(),
            dir: "/var/db/glasshouse/apps/.build/1".into(),
            args: vec!["app=app/src/main.ts".into(), "--bundle".into()],
            limit: Duration::from_secs(30),
            user: "nobody".into(),
        }
    }

    #[test]
    fn a_tool_runs_in_its_folder_with_no_network_as_nobody() {
        let args = jail_args(&run()).unwrap();
        for expected in [
            "-u",
            "nobody",
            "path=/var/db/glasshouse/apps/.build/1",
            "ip4=disable",
            "ip6=disable",
            "securelevel=3",
            "children.max=0",
            "mount=/usr/local/lib/glasshouse/bundler /var/db/glasshouse/apps/.build/1/.tool nullfs ro 0 0",
            "command=/.tool/esbuild",
            "app=app/src/main.ts",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected}: {args:?}");
        }
        // No persist: the jail goes when the tool exits. No devices.
        assert!(!args.iter().any(|a| a == "persist" || a.contains("devfs")));
    }

    #[test]
    fn odd_paths_and_names_are_refused() {
        let mut spaced = run();
        spaced.dir = "/var/db/a b".into();
        assert!(jail_args(&spaced).is_err());
        let mut named = run();
        named.name = "glass build".into();
        assert!(jail_args(&named).is_err());
    }

    #[test]
    fn a_tool_that_runs_too_long_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let run = ToolRun {
            name: "slow".into(),
            program: "/bin/sleep".into(),
            dir: dir.path().to_owned(),
            args: vec!["5".into()],
            limit: Duration::from_millis(200),
            user: "nobody".into(),
        };
        let started = Instant::now();
        let out = run_unconfined(&run).unwrap();
        assert!(out.timed_out && !out.success);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
