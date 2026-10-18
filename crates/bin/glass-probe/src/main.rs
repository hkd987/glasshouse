//! The containment probe (PA-1: an agent with no grants can't read user
//! files, open network connections or see other processes).
//!
//! Run as an agent with no scope, it tries everything it shouldn't be able
//! to do and records what happened. Every check must come out denied, except
//! one sanity check that the sandbox works at all (writing its own scratch
//! space), so a pass can't come from a broken environment, and one that the
//! sandbox puts the person first (lower priority, resource limits).
//!
//! Results go to stderr (the task log) and, summarized, to the tile caption.
//! The exit code is 0 only when every check passed, so the task ends Done on
//! a contained system and Failed on one that leaks.
//!
//! With a prompt starting `load` it is a busy agent instead ([`load`]).

mod load;

use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;

use glass_agent_sdk::Agent;
use serde_json::json;

#[derive(Debug)]
struct Check {
    name: &'static str,
    /// What a contained agent must see.
    want: Want,
    /// Whether the attempt succeeded.
    reached: bool,
    observed: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Denied,
    Allowed,
}

impl Check {
    fn passed(&self) -> bool {
        match self.want {
            Want::Denied => !self.reached,
            Want::Allowed => self.reached,
        }
    }
}

fn attempt(name: &'static str, want: Want, result: Result<String, String>) -> Check {
    match result {
        Ok(observed) => Check {
            name,
            want,
            reached: true,
            observed,
        },
        Err(observed) => Check {
            name,
            want,
            reached: false,
            observed,
        },
    }
}

fn io<T>(r: std::io::Result<T>, ok: impl FnOnce(T) -> String) -> Result<String, String> {
    r.map(ok).map_err(|e| e.to_string())
}

fn read_file(path: &str) -> Result<String, String> {
    io(std::fs::read(path), |b| format!("read {} bytes", b.len()))
}

fn list_dir(path: &str) -> Result<String, String> {
    match std::fs::read_dir(path) {
        Ok(entries) => {
            let names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            if names.is_empty() {
                Err("exists but is empty".into())
            } else {
                Ok(format!("lists {}", names.join(", ")))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

fn tcp(addr: &str) -> Result<String, String> {
    let addr: SocketAddr = addr.parse().map_err(|e| format!("{e}"))?;
    io(
        TcpStream::connect_timeout(&addr, Duration::from_secs(3)),
        |_| format!("connected to {addr}"),
    )
}

fn processes() -> Result<String, String> {
    let out = Command::new("/bin/ps")
        .args(["-ax", "-o", "pid=", "-o", "comm="])
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let foreign: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            let comm = l.split_whitespace().nth(1).unwrap_or_default();
            !matches!(comm, "glass-probe" | "ps" | "jexec")
        })
        .collect();
    if foreign.is_empty() {
        Err("sees only its own processes".into())
    } else {
        Ok(format!("sees {}", foreign.join("; ")))
    }
}

/// This machine's memory in bytes, from getconf(1) (FreeBSD names it
/// PHYS_PAGES, glibc _PHYS_PAGES).
fn physical_memory() -> Option<usize> {
    let getconf = |name: &str| -> Option<usize> {
        let out = Command::new("/usr/bin/getconf").arg(name).output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    };
    let pages = getconf("PHYS_PAGES").or_else(|| getconf("_PHYS_PAGES"))?;
    pages.checked_mul(getconf("PAGESIZE")?)
}

/// The person comes first: a contained agent starts below their CPU
/// priority, and its task can't commit three quarters of the machine's
/// memory (its limit is a quarter by default; without one, the commit
/// succeeds). The memory limit stands for all of the jail's rctl limits,
/// which are added in one go when the jail is created: one in force means
/// all are.
fn limited() -> Result<String, String> {
    let out = Command::new("/bin/ps")
        .args(["-o", "nice=", "-p", &std::process::id().to_string()])
        .output()
        .map_err(|e| e.to_string())?;
    let nice: i32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| "can't read its own priority".to_owned())?;
    if nice <= 0 {
        return Err(format!("runs at the person's priority (nice {nice})"));
    }
    let total = physical_memory().ok_or("can't read this machine's memory size")?;
    let want = total / 4 * 3;
    let mut memory: Vec<u8> = Vec::new();
    let reserved = memory.try_reserve_exact(want).is_ok();
    // Keep the allocation from being optimized away.
    std::hint::black_box(memory.as_mut_ptr());
    let gb = want as f64 / f64::from(1 << 30);
    if reserved {
        Err(format!(
            "nice {nice}, but committed {gb:.1} GB, three quarters of memory"
        ))
    } else {
        Ok(format!(
            "nice {nice}; committing {gb:.1} GB (three quarters of memory) was refused"
        ))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let agent = match Agent::from_env().await {
        Ok(agent) => agent,
        Err(e) => {
            eprintln!("glass-probe: {e}");
            return ExitCode::FAILURE;
        }
    };
    if agent.prompt.trim_start().starts_with("load") {
        let outcome = load::run(&agent).await;
        let (Ok(line) | Err(line)) = &outcome;
        // The last line is the tile's summary (or failure reason).
        eprintln!("{line}");
        let _ = agent.progress(line, None, None).await;
        return if outcome.is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let _ = agent.progress("Probing the sandbox", None, None).await;
    let work = agent.work_dir.clone();

    let mut checks = vec![
        attempt(
            "sandbox works: write own scratch space",
            Want::Allowed,
            io(std::fs::write(work.join("probe.txt"), b"ok"), |()| {
                "wrote /work/probe.txt".into()
            }),
        ),
        attempt("files: list /home", Want::Denied, list_dir("/home")),
        attempt("files: list /root", Want::Denied, list_dir("/root")),
        attempt(
            "files: read the audit log",
            Want::Denied,
            read_file("/var/db/glasshouse/audit/audit.log"),
        ),
        attempt(
            "files: read daemon config",
            Want::Denied,
            read_file("/usr/local/etc/glasshouse/glassd.toml"),
        ),
        attempt(
            "files: read password hashes",
            Want::Denied,
            read_file("/etc/master.passwd"),
        ),
        attempt(
            "files: write the base system",
            Want::Denied,
            io(std::fs::write("/bin/.glass-probe", b"x"), |()| {
                "wrote /bin/.glass-probe".into()
            }),
        ),
        attempt("net: TCP to the VM host", Want::Denied, tcp("10.0.2.2:22")),
        attempt("net: TCP to the internet", Want::Denied, tcp("1.1.1.1:443")),
        attempt(
            "net: TCP over IPv6",
            Want::Denied,
            tcp("[2606:4700:4700::1111]:443"),
        ),
        attempt(
            "net: open a UDP socket",
            Want::Denied,
            io(UdpSocket::bind("0.0.0.0:0"), |s| {
                format!(
                    "bound {}",
                    s.local_addr().map(|a| a.to_string()).unwrap_or_default()
                )
            }),
        ),
        attempt(
            "net: resolve DNS",
            Want::Denied,
            io(("example.com", 443).to_socket_addrs(), |a| {
                format!(
                    "resolved {}",
                    a.map(|x| x.to_string()).collect::<Vec<_>>().join(", ")
                )
            }),
        ),
        attempt(
            "net: reach the control socket",
            Want::Denied,
            io(
                UnixStream::connect("/var/run/glasshouse/control.sock"),
                |_| "connected".into(),
            ),
        ),
        attempt("proc: see other processes", Want::Denied, processes()),
        attempt(
            "proc: signal init",
            Want::Denied,
            match Command::new("/bin/kill").args(["-0", "1"]).status() {
                Ok(s) if s.success() => Ok("signaled pid 1".into()),
                Ok(s) => Err(format!("kill failed ({s})")),
                Err(e) => Err(e.to_string()),
            },
        ),
        attempt(
            "dev: open /dev/mem",
            Want::Denied,
            io(std::fs::File::open("/dev/mem"), |_| "opened".into()),
        ),
        attempt(
            "limits: below the person's priority, memory capped",
            Want::Allowed,
            limited(),
        ),
    ];

    for (name, path, tool) in [
        (
            "broker: path escape with ..",
            "../../etc/passwd",
            "files_read",
        ),
        (
            "broker: folder not granted",
            "Secrets/plan.txt",
            "files_read",
        ),
        (
            "broker: write with no grant",
            "Anything/x.txt",
            "files_write",
        ),
    ] {
        let args = if tool == "files_write" {
            json!({"path": path, "content": "x"})
        } else {
            json!({"path": path})
        };
        let result = match agent.call(tool, args).await {
            Ok(r) if r.is_error => Err(r.text()),
            Ok(r) => Ok(format!("succeeded: {}", r.text())),
            Err(e) => Err(e.to_string()),
        };
        checks.push(attempt(name, Want::Denied, result));
    }

    // Anything the probe managed to create is evidence, not something to
    // leave behind.
    let _ = std::fs::remove_file("/bin/.glass-probe");
    let _ = std::fs::remove_file(Path::new(&work).join("probe.txt"));

    let passed = checks.iter().filter(|c| c.passed()).count();
    let report = json!({
        "passed": passed,
        "total": checks.len(),
        "checks": checks.iter().map(|c| json!({
            "name": c.name,
            "want": if c.want == Want::Denied { "denied" } else { "allowed" },
            "passed": c.passed(),
            "observed": c.observed,
        })).collect::<Vec<_>>(),
    });
    eprintln!("{report}");
    for check in &checks {
        eprintln!(
            "{} {} — {}",
            if check.passed() { "PASS" } else { "FAIL" },
            check.name,
            check.observed
        );
    }
    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| !c.passed())
        .map(|c| c.name)
        .collect();
    let caption = format!("Containment: {passed}/{} checks passed", checks.len());
    // The last line is what the tile shows as the failure reason.
    if failed.is_empty() {
        eprintln!("{caption}");
    } else {
        eprintln!("{caption}; failed: {}", failed.join("; "));
    }
    let _ = agent
        .progress(&caption, Some(passed as u32), Some(checks.len() as u32))
        .await;
    if passed == checks.len() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
