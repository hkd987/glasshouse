//! Agent computers on Linux namespaces (ADR 0066), where there's no KVM:
//! development, and these sessions.
//!
//! A computer is a folder, its disk: its own `/etc`, `/root`, `/work`,
//! `/opt` and the rest, kept between tasks. Each run gets fresh user,
//! mount, PID, network, IPC and UTS namespaces over it:
//!
//! - the agent is root inside, an unprivileged account outside (the
//!   computer's owner): `setpriv` drops to it before `unshare` maps it to
//!   root, so the namespace grants nothing the owner hadn't;
//! - the host's system folders (`/usr` and the links beside it) are bound
//!   read-only, and so are the agent programs' folders: an agent installs
//!   into its home, `/opt` or a virtual environment, never the system;
//! - no network: only a loopback that is down. The task socket is its one
//!   way out, bound at [`glass_platform::sandbox::AGENT_SOCKET_PATH`];
//! - its own `/proc`, and only `null`, `zero`, `full`, `random`, `urandom`
//!   and `tty` in `/dev`.
//!
//! Each run starts below the person's priority (`nice`) with its memory
//! capped at the computer's (`prlimit --as`), as the Linux tool runner's
//! are; a VM's are its own.
//!
//! When the run's first process exits, the kernel ends every process in
//! its PID namespace, and the mounts go with the namespace (PA-2). Nothing
//! runs between tasks: a namespace computer keeps its disk, not its
//! programs.
//!
//! This is a real boundary, weaker than a VM: the agent shares the host's
//! kernel. Firecracker ([`crate::firecracker`]) is the one on AWS.

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::hypervisor::{ComputerSpec, Hypervisor, TaskRun};

/// The host's system folders: bound read-only where they are folders, the
/// same links where they are links (a merged `/usr`).
const SYSTEM: &[&str] = &["usr", "bin", "sbin", "lib", "lib32", "lib64", "libx32"];

/// The computer's own folders, on its disk.
const OWN: &[&str] = &[
    "etc",
    "home",
    "root",
    "work",
    "opt",
    "srv",
    "mnt",
    "var/tmp",
    "var/log",
    "tmp",
    "run/glasshouse",
    "proc",
    "dev",
];

/// What a new computer's `/etc` takes from the host's: enough for programs
/// to start and name things. Never accounts, keys or the host's network.
const ETC: &[&str] = &[
    "ld.so.cache",
    "ld.so.conf",
    "ld.so.conf.d",
    "nsswitch.conf",
    "ssl/certs",
    "ssl/openssl.cnf",
    "ca-certificates.conf",
    "alternatives",
    "localtime",
    "mime.types",
    "protocols",
    "services",
    "profile",
    "bash.bashrc",
];

/// Runs in the new namespaces, as their root: binds what the run sees,
/// then enters the computer. `$1` its disk, `$2` its name, `$3` the task
/// socket's folder, `$4` the working folder; read-only binds up to `--`,
/// then the program and its arguments.
const ENTER: &str = r#"set -eu
root=$1 name=$2 sock=$3 work=$4
shift 4
if [ ! -d "$root" ]; then
  echo "its computer's disk can't be reached as its owner: a folder above $root is closed to them" >&2
  exit 1
fi
# The agent owns its disk: a mount point it made a link is refused, never
# followed.
for d in proc dev run run/glasshouse; do
  if [ -L "$root/$d" ] || [ ! -d "$root/$d" ]; then
    echo "its computer's /$d isn't a folder: put it back with mkdir -p /$d" >&2
    exit 1
  fi
done
while [ "$1" != -- ]; do
  p=$1
  if [ -L "/$p" ]; then
    [ -L "$root/$p" ] || ln -s "$(readlink "/$p")" "$root/$p"
  else
    if [ -L "$root/$p" ]; then
      echo "its computer's /$p is a link; the system's folders aren't its to move" >&2
      exit 1
    fi
    mkdir -p "$root/$p"
    mount --rbind "/$p" "$root/$p"
    mount -o remount,bind,ro "$root/$p"
  fi
  shift
done
shift
mount -t proc proc "$root/proc"
for d in null zero full random urandom tty; do
  [ -e "$root/dev/$d" ] || : > "$root/dev/$d"
  mount --bind "/dev/$d" "$root/dev/$d"
done
mount --bind "$sock" "$root/run/glasshouse"
hostname "$name" 2>/dev/null || true
mkdir -p "$root$work"
exec chroot "$root" /bin/sh -c 'cd "$1" && shift && exec "$@"' sh "$work" "$@"
"#;

/// Agent computers on Linux namespaces.
#[derive(Debug, Clone)]
pub struct NamespaceHypervisor {
    /// Host folders mounted read-only at the same paths in every run: the
    /// agent programs' (glassd's `sandbox.agent_dirs`).
    pub binds: Vec<PathBuf>,
    /// Where the system's tools are (`unshare`, `setpriv`, `mount`,
    /// `chroot`, `hostname`), for the steps before the computer is entered.
    pub tool_path: String,
    /// The niceness runs start with: 0 is the person's, 19 the lowest.
    pub nice: u8,
}

impl NamespaceHypervisor {
    pub fn new(binds: Vec<PathBuf>) -> Self {
        Self {
            binds,
            tool_path: "/usr/sbin:/usr/bin:/sbin:/bin".into(),
            nice: 10,
        }
    }

    /// Whether this host can run them: a quick run of `true` in the same
    /// namespaces. Some containers and hardened kernels refuse them.
    pub fn available() -> Result<(), String> {
        let out = Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--net",
                "--mount",
                "--pid",
                "--fork",
                "true",
            ])
            .output()
            .map_err(|e| format!("unshare: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "this host doesn't allow namespaces for agent computers: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

fn disk(computer: &ComputerSpec) -> PathBuf {
    computer.dir.join("disk")
}

fn awake(computer: &ComputerSpec) -> PathBuf {
    computer.dir.join("awake")
}

impl Hypervisor for NamespaceHypervisor {
    fn kind(&self) -> &'static str {
        "Linux namespaces"
    }

    fn contained(&self) -> bool {
        true
    }

    fn start(&self, computer: &ComputerSpec) -> Result<(), String> {
        let disk = disk(computer);
        if !disk.is_dir() {
            make_disk(computer, &disk)?;
        }
        fs::write(awake(computer), b"").map_err(|e| format!("starting the computer: {e}"))
    }

    fn running(&self, computer: &ComputerSpec) -> bool {
        awake(computer).exists()
    }

    fn command(&self, computer: &ComputerSpec, run: &TaskRun) -> Result<Command, String> {
        let disk = disk(computer);
        if !disk.is_dir() {
            return Err(format!("{} has no disk yet: start it first", computer.id));
        }
        let socket_dir = match (run.socket.parent(), run.socket.file_name()) {
            (Some(dir), Some(name)) if name == "agent.sock" => dir,
            _ => {
                return Err(format!(
                    "{} isn't a task socket (…/agent.sock)",
                    run.socket.display()
                ));
            }
        };
        let owner =
            fs::metadata(&computer.dir).map_err(|e| format!("{}: {e}", computer.dir.display()))?;
        let mut command = if glass_platform::users::effective_uid() == 0 && owner.uid() != 0 {
            let mut command = Command::new("setpriv");
            command.args([
                &format!("--reuid={}", owner.uid()),
                &format!("--regid={}", owner.gid()),
                "--clear-groups",
                "prlimit",
            ]);
            command
        } else {
            Command::new("prlimit")
        };
        let memory = u64::from(computer.memory_mib.max(256)) << 20;
        command
            .args([
                &format!("--as={memory}"),
                "--",
                "nice",
                "-n",
                &self.nice.min(19).to_string(),
                "unshare",
                "--user",
                "--map-root-user",
                "--net",
                "--mount",
                "--pid",
                "--ipc",
                "--uts",
                "--fork",
                "--kill-child",
                "--",
                "/bin/sh",
                "-c",
                &format!("PATH={}\n{ENTER}", self.tool_path),
                "glass-computer",
            ])
            .arg(&disk)
            .arg(&computer.id)
            .arg(socket_dir)
            .arg(&run.workdir);
        for name in SYSTEM {
            if fs::symlink_metadata(Path::new("/").join(name)).is_ok() {
                command.arg(name);
            }
        }
        for bind in &self.binds {
            let relative = bind
                .strip_prefix("/")
                .map_err(|_| format!("{} isn't an absolute path", bind.display()))?;
            command.arg(relative);
        }
        command
            .arg("--")
            .arg(&run.process.program)
            .args(&run.process.args)
            .env_clear()
            .envs(run.process.env.iter().map(|(k, v)| (k, v)))
            .current_dir("/");
        Ok(command)
    }

    fn host_path(&self, computer: &ComputerSpec, inside: &Path) -> Option<PathBuf> {
        // The agent may have made any of it a link: open what's under it
        // with `glass_platform::fs::ConfinedDir`, never std::fs.
        Some(disk(computer).join(inside.strip_prefix("/").unwrap_or(inside)))
    }

    /// A run's processes have its computer's disk as their root, so a
    /// process whose root is under `dir` is one an earlier glassd left
    /// (it died, so `--kill-child` never fired): it's killed.
    fn stop_leftovers(&self, dir: &Path) -> Vec<String> {
        let mut stopped = Vec::new();
        let Ok(dir) = dir.canonicalize() else {
            return stopped;
        };
        let Ok(procs) = fs::read_dir("/proc") else {
            return stopped;
        };
        for entry in procs.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|p| p.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(root) = fs::read_link(entry.path().join("root")) else {
                continue;
            };
            if root.starts_with(&dir) && root != dir {
                let killed = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status()
                    .is_ok_and(|s| s.success());
                if killed {
                    stopped.push(format!("process {pid} in {}", root.display()));
                }
            }
        }
        stopped
    }

    fn suspend(&self, computer: &ComputerSpec) -> Result<(), String> {
        // Its runs' processes end with them; what's left is its disk.
        match fs::remove_file(awake(computer)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("stopping the computer: {e}"))
            }
            _ => Ok(()),
        }
    }
}

/// Makes a computer's disk the first time: its own folders, a small `/etc`,
/// all of it its owner's (root inside). Made beside and renamed into
/// place, so a disk is whole or absent.
fn make_disk(computer: &ComputerSpec, disk: &Path) -> Result<(), String> {
    let making = computer.dir.join("disk.new");
    let fail =
        |what: &str, e: std::io::Error| format!("making {}'s disk: {what}: {e}", computer.id);
    if making.exists() {
        fs::remove_dir_all(&making).map_err(|e| fail("clearing a half-made disk", e))?;
    }
    for dir in OWN {
        fs::create_dir_all(making.join(dir)).map_err(|e| fail(dir, e))?;
    }
    use std::os::unix::fs::PermissionsExt;
    for tmp in ["tmp", "var/tmp"] {
        fs::set_permissions(making.join(tmp), fs::Permissions::from_mode(0o1777))
            .map_err(|e| fail(tmp, e))?;
    }
    fs::set_permissions(making.join("root"), fs::Permissions::from_mode(0o700))
        .map_err(|e| fail("root", e))?;
    for name in ETC {
        let from = Path::new("/etc").join(name);
        if fs::symlink_metadata(&from).is_ok() {
            copy(&from, &making.join("etc").join(name)).map_err(|e| fail(name, e))?;
        }
    }
    let etc = making.join("etc");
    let id = &computer.id;
    for (name, text) in [
        (
            "passwd",
            "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n".to_owned(),
        ),
        ("group", "root:x:0:\nnogroup:x:65534:\n".to_owned()),
        ("hostname", format!("{id}\n")),
        (
            "hosts",
            format!("127.0.0.1\tlocalhost\n127.0.1.1\t{id}\n::1\tlocalhost\n"),
        ),
        // No network: nothing to resolve with.
        ("resolv.conf", String::new()),
    ] {
        fs::write(etc.join(name), text).map_err(|e| fail(name, e))?;
    }
    let owner = fs::metadata(&computer.dir).map_err(|e| fail("its folder", e))?;
    chown_all(&making, owner.uid(), owner.gid()).map_err(|e| fail("giving it to its owner", e))?;
    fs::rename(&making, disk).map_err(|e| fail("putting it in place", e))
}

/// Copies a file, a link or a folder of them, links as links.
fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        symlink(fs::read_link(from)?, to)
    } else if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else if meta.is_file() {
        fs::copy(from, to).map(drop)
    } else {
        Ok(())
    }
}

/// Gives a tree to `uid`, links themselves rather than what they name.
pub(crate) fn chown_all(path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
    std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
    if fs::symlink_metadata(path)?.is_dir() {
        for entry in fs::read_dir(path)? {
            chown_all(&entry?.path(), uid, gid)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    use glass_platform::sandbox::ProcessSpec;

    use super::*;

    fn computer(dir: &Path) -> ComputerSpec {
        let dir = dir.join("dana-reconciler");
        fs::create_dir_all(&dir).unwrap();
        ComputerSpec {
            id: "dana-reconciler".into(),
            owner: 1001,
            dir,
            memory_mib: 1024,
            vcpus: 1,
        }
    }

    /// A folder every account can reach, as glassd's computers folder is:
    /// the run drops to the computer's owner before it enters.
    fn reachable() -> tempfile::TempDir {
        let dir = tempfile::Builder::new()
            .prefix("glass-computers-test")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    fn run(computer: &ComputerSpec, script: &str) -> TaskRun {
        let tasks = computer.dir.join("tasks/glass-test");
        fs::create_dir_all(&tasks).unwrap();
        TaskRun {
            socket: tasks.join("agent.sock"),
            workdir: "/work".into(),
            process: ProcessSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            },
        }
    }

    #[test]
    fn adr0066_a_namespace_computer_keeps_its_disk_and_nothing_else() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let dir = reachable();
        let computer = computer(dir.path());
        let hv = NamespaceHypervisor::new(Vec::new());
        assert!(!hv.running(&computer));
        hv.start(&computer).unwrap();
        assert!(hv.running(&computer));
        assert_eq!(hv.kind(), "Linux namespaces");

        // Root inside, on its own host name, with its own files.
        let out = hv
            .command(
                &computer,
                &run(
                    &computer,
                    "id -u; hostname; pwd; echo kept > /work/notes.txt; mkdir -p /opt/tool; \
                     touch /usr/escape 2>/dev/null && echo wrote-usr; \
                     cat /etc/hostname; ls /run/glasshouse",
                ),
            )
            .unwrap()
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[..4],
            ["0", "dana-reconciler", "/work", "dana-reconciler"]
        );
        assert!(!text.contains("wrote-usr"), "{text}");

        // The next run finds them: the disk is kept.
        let out = hv
            .command(
                &computer,
                &run(&computer, "cat /work/notes.txt; ls -d /opt/tool"),
            )
            .unwrap()
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "kept\n/opt/tool\n");
        let notes = hv
            .host_path(&computer, Path::new("/work/notes.txt"))
            .unwrap();
        assert_eq!(fs::read_to_string(notes).unwrap(), "kept\n");

        hv.suspend(&computer).unwrap();
        assert!(!hv.running(&computer));
    }

    #[test]
    fn adr0066_a_computer_has_no_network_and_none_of_the_hosts_files() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let dir = reachable();
        let computer = computer(dir.path());
        let hv = NamespaceHypervisor::new(Vec::new());
        hv.start(&computer).unwrap();
        let script = "cat /proc/net/dev | grep -c : ; \
                      ls /home; ls /etc | grep -cx shadow; \
                      ls /proc | grep -cE '^[0-9]+$'";
        let out = hv
            .command(&computer, &run(&computer, script))
            .unwrap()
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().collect();
        // Only a loopback; an empty /home; no shadow file; only its own
        // processes (the shell, the pipeline's).
        assert_eq!(lines[0], "1", "{text}");
        assert_eq!(lines[1], "0", "{text}");
        let processes: u32 = lines[2].parse().unwrap();
        assert!(processes < 6, "{text}");
    }

    #[test]
    fn adr0066_the_task_socket_is_the_only_way_out() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let dir = reachable();
        let computer = computer(dir.path());
        let hv = NamespaceHypervisor::new(Vec::new());
        hv.start(&computer).unwrap();
        let task = run(
            &computer,
            "test -S /run/glasshouse/agent.sock && echo socket",
        );
        let listener = UnixListener::bind(&task.socket).unwrap();
        let out = hv.command(&computer, &task).unwrap().output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "socket\n");
        drop(listener);

        let bad = TaskRun {
            socket: computer.dir.join("elsewhere.sock"),
            ..task
        };
        assert!(hv.command(&computer, &bad).is_err());
    }

    #[test]
    fn adr0066_root_in_a_computer_is_its_owner_outside() {
        if glass_platform::users::effective_uid() != 0 {
            eprintln!("skipped: needs root, as glassd runs");
            return;
        }
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let dir = reachable();
        let computer = computer(dir.path());
        std::os::unix::fs::chown(&computer.dir, Some(65534), Some(65534)).unwrap();
        let hv = NamespaceHypervisor::new(Vec::new());
        hv.start(&computer).unwrap();
        let out = hv
            .command(&computer, &run(&computer, "id -u; echo mine > /root/file"))
            .unwrap()
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n");
        let file = hv.host_path(&computer, Path::new("/root/file")).unwrap();
        assert_eq!(fs::metadata(file).unwrap().uid(), 65534);
    }

    #[test]
    fn adr0066_what_a_dead_glassd_left_running_is_stopped() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let dir = reachable();
        let computer = computer(dir.path());
        let hv = NamespaceHypervisor::new(Vec::new());
        hv.start(&computer).unwrap();
        // A run whose glassd went away: nothing waits for it.
        let mut orphan = hv
            .command(&computer, &run(&computer, "sleep 60"))
            .unwrap()
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let stopped = hv.stop_leftovers(dir.path());
        assert!(!stopped.is_empty(), "nothing stopped");
        let status = orphan.wait().unwrap();
        assert!(!status.success());
        assert!(hv.stop_leftovers(dir.path()).is_empty());
    }

    #[test]
    fn adr0066_a_run_is_below_the_persons_priority_with_its_memory_capped() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let dir = reachable();
        let computer = computer(dir.path());
        let hv = NamespaceHypervisor::new(Vec::new());
        hv.start(&computer).unwrap();
        let out = hv
            .command(
                &computer,
                &run(
                    &computer,
                    "ps -o nice= -p $$; python3 -c 'bytearray(2 << 30)' 2>&1 | tail -1",
                ),
            )
            .unwrap()
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        assert_eq!(lines[0], "10", "{text}");
        assert!(lines[1].contains("MemoryError"), "{text}");
    }

    #[test]
    fn adr0066_a_mount_point_made_a_link_is_refused() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let dir = reachable();
        let computer = computer(dir.path());
        let hv = NamespaceHypervisor::new(Vec::new());
        hv.start(&computer).unwrap();
        // The agent, root on its computer, swaps /proc for a link.
        let out = hv
            .command(
                &computer,
                &run(&computer, "umount /proc; rmdir /proc; ln -s / /proc"),
            )
            .unwrap()
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = hv
            .command(&computer, &run(&computer, "echo ran"))
            .unwrap()
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("isn't a folder"));
    }

    #[test]
    fn adr0066_a_computers_etc_takes_nothing_secret() {
        let dir = reachable();
        let computer = computer(dir.path());
        NamespaceHypervisor::new(Vec::new())
            .start(&computer)
            .unwrap();
        let etc = computer.dir.join("disk/etc");
        for secret in ["shadow", "gshadow", "ssh", "ssl/private", "sudoers"] {
            assert!(!etc.join(secret).exists(), "{secret}");
        }
        let passwd = fs::read_to_string(etc.join("passwd")).unwrap();
        assert!(passwd.starts_with("root:x:0:0:"));
        assert_eq!(passwd.lines().count(), 2);
        assert!(!computer.dir.join("disk.new").exists());
    }
}
