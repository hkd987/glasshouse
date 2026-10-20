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
/// `jail -c` for `run`, with `rules` (rctl(8), set before the tool
/// starts) when resource accounting is on.
pub(crate) fn jail_args(run: &ToolRun, rules: &[String]) -> Result<Vec<String>> {
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
    ];
    if !rules.is_empty() {
        args.push(format!(
            "exec.prestart=/usr/bin/rctl -a {}",
            rules.join(" ")
        ));
    }
    args.push(format!("command=/.tool/{file}"));
    args.extend(run.args.iter().cloned());
    Ok(args)
}

/// Runs `run` in a jail of its own; stopping it removes the jail. Either
/// way the tool's mount goes before this returns, so the folder can be
/// removed.
pub(crate) fn run_jailed(run: &ToolRun, limits: &super::ResourceLimits) -> Result<ToolOutput> {
    // What an agent's task may use, a tool may use: a runaway build can't
    // take the person's memory or fill the process table.
    let rules = limits.rctl_rules(&run.name);
    let args = jail_args(run, &rules)?;
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
    if !rules.is_empty() {
        let _ = crate::cmd::run("rctl", &["-r", &format!("jail:{}", run.name)]);
    }
    out
}

/// Our servers' confinement for a tool, on Linux, where there are no
/// jails: the base system's `prlimit` caps its memory, CPU time, file size
/// and open files, and `unshare` gives it user, mount, network, IPC and PID
/// namespaces with the build folder as its root and the tool copied in
/// beside its files. No network, nothing outside the folder, nobody's
/// privileges. The admin console bundles the apps people share this way
/// (ADR 0032); where `unshare` can't, the run fails, never runs unconfined.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnshareTools;

impl super::ToolRunner for UnshareTools {
    fn run_tool(&self, run: &ToolRun) -> Result<ToolOutput> {
        run_unshared(run)
    }

    /// It runs as its caller, who is root only of a namespace that holds
    /// nothing but the tool's folder.
    fn runs_as_user(&self) -> bool {
        false
    }
}

/// Where the tool is copied in its folder.
const UNSHARED_TOOL: &str = ".tool";

/// `prlimit … -- unshare … --root=<dir> /.tool <args>`.
pub(crate) fn unshare_args(run: &ToolRun) -> Result<Vec<String>> {
    super::check_name(&run.name)?;
    let root = run.dir.to_string_lossy();
    let mut args = vec![
        "--as=4294967296".to_owned(),
        format!("--cpu={}", run.limit.as_secs().max(1)),
        "--fsize=67108864".to_owned(),
        "--nofile=256".to_owned(),
        "--".to_owned(),
        "unshare".to_owned(),
        "--user".to_owned(),
        "--map-root-user".to_owned(),
        "--net".to_owned(),
        "--mount".to_owned(),
        "--ipc".to_owned(),
        "--pid".to_owned(),
        "--fork".to_owned(),
        "--kill-child".to_owned(),
        format!("--root={root}"),
        "--wd=/".to_owned(),
        format!("/{UNSHARED_TOOL}"),
    ];
    args.extend(run.args.iter().cloned());
    Ok(args)
}

fn run_unshared(run: &ToolRun) -> Result<ToolOutput> {
    if !cfg!(target_os = "linux") {
        return Err(PlatformError::Unsupported(
            "unshare confines tools on Linux only".into(),
        ));
    }
    let args = unshare_args(run)?;
    let tool = run.dir.join(UNSHARED_TOOL);
    std::fs::copy(&run.program, &tool)
        .map_err(|e| PlatformError::io(format!("copying {} in", run.program.display()), e))?;
    let child = Command::new("prlimit")
        .args(&args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PlatformError::io("running prlimit and unshare", e))?;
    let out = wait(child, run.limit, |child| {
        let _ = child.kill();
    });
    let _ = std::fs::remove_file(&tool);
    out
}

#[cfg(test)]
mod tests {
    use super::super::ToolRunner;
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
        let args = jail_args(&run(), &[]).unwrap();
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
    fn a_tool_gets_the_limits_an_agents_task_gets() {
        let limits = super::super::ResourceLimits {
            memory_bytes: 1 << 30,
            max_processes: 64,
            ..Default::default()
        };
        let rules = limits.rctl_rules("glass-build-1");
        let args = jail_args(&run(), &rules).unwrap();
        let prestart = args
            .iter()
            .find(|a| a.starts_with("exec.prestart="))
            .unwrap();
        assert!(
            prestart.contains("jail:glass-build-1:memoryuse:deny=1073741824"),
            "{prestart}"
        );
        assert!(
            prestart.contains("jail:glass-build-1:maxproc:deny=64"),
            "{prestart}"
        );
        // Set before the tool starts.
        let at = |p: &str| args.iter().position(|a| a.starts_with(p)).unwrap();
        assert!(at("exec.prestart=") < at("command="));
        // Without accounting, no rules and no rctl.
        assert!(
            !jail_args(&run(), &[])
                .unwrap()
                .iter()
                .any(|a| a.contains("rctl"))
        );
    }

    #[test]
    fn odd_paths_and_names_are_refused() {
        let mut spaced = run();
        spaced.dir = "/var/db/a b".into();
        assert!(jail_args(&spaced, &[]).is_err());
        let mut named = run();
        named.name = "glass build".into();
        assert!(jail_args(&named, &[]).is_err());
    }

    #[test]
    fn on_our_servers_a_tool_runs_rooted_in_its_folder_with_no_network() {
        let args = unshare_args(&run()).unwrap();
        for expected in [
            "--net",
            "--user",
            "--mount",
            "--pid",
            "--root=/var/db/glasshouse/apps/.build/1",
            "--wd=/",
            "/.tool",
            "--cpu=30",
            "app=app/src/main.ts",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected}: {args:?}");
        }
    }

    /// The real thing, where this machine can: esbuild (the apps
    /// workspace's) bundles in its folder and sees nothing else.
    #[cfg(target_os = "linux")]
    #[test]
    fn unshared_esbuild_bundles_and_reads_nothing_outside() {
        let esbuild = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/node_modules/@esbuild/linux-x64/bin/esbuild");
        if !esbuild.exists() {
            eprintln!("skipped: no esbuild in apps/node_modules");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(
            dir.path().join("main.ts"),
            "import secret from \"/etc/hostname\";\nconsole.log(secret);\n",
        )
        .unwrap();
        let run = ToolRun {
            name: "unshared".into(),
            program: esbuild,
            dir: dir.path().to_owned(),
            args: vec![
                "main.ts".into(),
                "--bundle".into(),
                "--loader:.txt=text".into(),
                "--outdir=out".into(),
                "--log-level=error".into(),
            ],
            limit: Duration::from_secs(30),
            user: "nobody".into(),
        };
        let out = UnshareTools.run_tool(&run).unwrap();
        // /etc/hostname isn't in its world.
        assert!(!out.success, "{out:?}");
        assert!(out.stderr.contains("Could not resolve"), "{}", out.stderr);
        assert!(!dir.path().join(UNSHARED_TOOL).exists());

        std::fs::write(dir.path().join("main.ts"), "console.log(1 + 1);\n").unwrap();
        let out = UnshareTools.run_tool(&run).unwrap();
        assert!(out.success, "{}", out.stderr);
        assert!(dir.path().join("out/main.js").exists());
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
