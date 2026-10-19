//! What the IT self-service agent inspects and fixes on this machine (AE-4):
//! network, storage, load and services, read through the base system's own
//! tools (ADR 0007). Everything here reads, except [`restart_service`].
//!
//! FreeBSD is the target; Linux works for what development hosts need
//! (storage, load, processes). Anything else reports `Unsupported`.

use std::net::{IpAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use serde::Serialize;

use crate::{PlatformError, Result, cmd};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Interface {
    pub name: String,
    pub up: bool,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetworkStatus {
    /// Interfaces other than loopback.
    pub interfaces: Vec<Interface>,
    pub gateway: Option<String>,
    pub nameservers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FsUsage {
    pub total_kb: u64,
    pub used_kb: u64,
    pub available_kb: u64,
}

impl FsUsage {
    pub fn percent_used(&self) -> u64 {
        (self.used_kb * 100)
            .checked_div(self.used_kb + self.available_kb)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Load {
    /// 1, 5 and 15 minute load averages.
    pub average: [f64; 3],
    pub cpus: u32,
    pub memory_total_mb: u64,
    pub memory_free_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Process {
    pub pid: u32,
    pub cpu_percent: f64,
    pub memory_mb: u64,
    pub command: String,
}

/// Interfaces, the default gateway and the configured DNS servers.
pub fn network() -> Result<NetworkStatus> {
    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("network diagnostics".into()));
    }
    let interfaces = parse_ifconfig(&cmd::run("ifconfig", &["-a"])?);
    let gateway = cmd::run("route", &["-n", "get", "default"])
        .ok()
        .and_then(|out| field(&out, "gateway:"));
    let nameservers = std::fs::read_to_string("/etc/resolv.conf")
        .map(|conf| {
            conf.lines()
                .filter_map(|l| l.trim().strip_prefix("nameserver"))
                .map(|s| s.trim().to_owned())
                .collect()
        })
        .unwrap_or_default();
    Ok(NetworkStatus {
        interfaces,
        gateway,
        nameservers,
    })
}

/// Looks `host` up in DNS, giving up after `timeout`.
pub fn resolve(host: &str, timeout: Duration) -> std::result::Result<Vec<IpAddr>, String> {
    let (tx, rx) = mpsc::channel();
    let target = format!("{host}:443");
    // The resolver can block for a long time; leave it behind on timeout.
    std::thread::spawn(move || {
        let result = target
            .to_socket_addrs()
            .map(|addrs| addrs.map(|a| a.ip()).collect::<Vec<_>>())
            .map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| Err(format!("no answer within {} s", timeout.as_secs())))
}

/// Space on the filesystem holding `path`.
pub fn filesystem_usage(path: &Path) -> Result<FsUsage> {
    let path = path.to_string_lossy();
    let out = cmd::run("df", &["-k", "-P", &path])?;
    // POSIX output: Filesystem 1024-blocks Used Available Capacity Mounted-on
    let fields: Vec<&str> = out
        .lines()
        .nth(1)
        .map(|l| l.split_whitespace().collect())
        .unwrap_or_default();
    let number = |i: usize| fields.get(i).and_then(|f| f.parse().ok());
    match (number(1), number(2), number(3)) {
        (Some(total_kb), Some(used_kb), Some(available_kb)) => Ok(FsUsage {
            total_kb,
            used_kb,
            available_kb,
        }),
        _ => Err(PlatformError::Unsupported(format!(
            "reading `df` output for {path}"
        ))),
    }
}

/// This computer, as About Glasshouse describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Computer {
    /// The host name.
    pub name: String,
    /// The processor's model name.
    pub processor: String,
    pub memory_mb: u64,
}

/// The host name, processor model and memory.
pub fn computer() -> Result<Computer> {
    let name = cmd::run("hostname", &[])?.trim().to_owned();
    let memory_mb = load()?.memory_total_mb;
    let processor = if cfg!(target_os = "freebsd") {
        cmd::run("sysctl", &["-n", "hw.model"])?.trim().to_owned()
    } else {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|info| {
                info.lines()
                    .find_map(|l| l.strip_prefix("model name"))
                    .and_then(|v| v.split_once(':'))
                    .map(|(_, model)| model.trim().to_owned())
            })
            .unwrap_or_default()
    };
    Ok(Computer {
        name,
        processor,
        memory_mb,
    })
}

/// Load averages, CPUs and memory.
pub fn load() -> Result<Load> {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    if cfg!(target_os = "freebsd") {
        let sysctl = |name: &str| cmd::run("sysctl", &["-n", name]).map(|s| s.trim().to_owned());
        // vm.loadavg reads "{ 0.10 0.20 0.30 }".
        let average = parse_load(&sysctl("vm.loadavg")?.replace(['{', '}'], ""));
        let page: u64 = sysctl("hw.pagesize")?.parse().unwrap_or(4096);
        let total: u64 = sysctl("hw.physmem")?.parse().unwrap_or(0);
        let free: u64 = sysctl("vm.stats.vm.v_free_count")?.parse().unwrap_or(0);
        Ok(Load {
            average,
            cpus,
            memory_total_mb: total / 1024 / 1024,
            memory_free_mb: free * page / 1024 / 1024,
        })
    } else if cfg!(target_os = "linux") {
        let read = |p: &str| std::fs::read_to_string(p).map_err(|e| PlatformError::io(p, e));
        let meminfo = read("/proc/meminfo")?;
        let kb = |key: &str| {
            meminfo
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
                .unwrap_or(0)
        };
        Ok(Load {
            average: parse_load(&read("/proc/loadavg")?),
            cpus,
            memory_total_mb: kb("MemTotal:") / 1024,
            memory_free_mb: kb("MemAvailable:") / 1024,
        })
    } else {
        Err(PlatformError::Unsupported("load averages".into()))
    }
}

/// What the sandbox's resource limits need to know about this machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Resources {
    pub cpus: u64,
    /// Bytes, 0 when unknown.
    pub physical_memory: u64,
    /// Resource accounting is on (`kern.racct.enable=1`, read at boot), so
    /// rctl(8) limits can be set. Off on other systems.
    pub accounting: bool,
}

/// One `sysctl` call, so glassd can size each task's limits at start.
pub fn resources() -> Resources {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u64);
    if !cfg!(target_os = "freebsd") {
        return Resources {
            cpus,
            ..Resources::default()
        };
    }
    // Names that don't exist make sysctl fail; then nothing is assumed.
    let out = cmd::run("sysctl", &["-n", "hw.physmem", "kern.racct.enable"]).unwrap_or_default();
    let mut lines = out.lines().map(str::trim);
    Resources {
        cpus,
        physical_memory: lines.next().and_then(|v| v.parse().ok()).unwrap_or(0),
        accounting: lines.next() == Some("1"),
    }
}

/// The user's busiest processes, most CPU first. Only that user's: what
/// other people run is not the agent's business.
pub fn top_processes(uid: u32, limit: usize) -> Result<Vec<Process>> {
    let uid = uid.to_string();
    // One -o per column: FreeBSD's ps folds `-o a=,b=` into one header.
    let out = cmd::run(
        "ps",
        &[
            "-U", &uid, "-o", "pid=", "-o", "pcpu=", "-o", "rss=", "-o", "comm=",
        ],
    )?;
    let mut processes: Vec<Process> = out
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some(Process {
                pid: parts.next()?.parse().ok()?,
                cpu_percent: parts.next()?.parse().ok()?,
                memory_mb: parts.next()?.parse::<u64>().ok()? / 1024,
                command: parts.collect::<Vec<_>>().join(" "),
            })
        })
        .filter(|p| p.command != "ps")
        .collect();
    processes.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
    processes.truncate(limit);
    Ok(processes)
}

/// One of a user's processes, and the jail it runs in: agents run in jails
/// named after their task (Activity Monitor groups them by it).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserProcess {
    pub pid: u32,
    /// `None` outside any jail.
    pub jail: Option<String>,
    pub cpu_percent: f64,
    pub memory_kb: u64,
    pub command: String,
}

/// Every process `uid` runs, in jails or not.
pub fn user_processes(uid: u32) -> Result<Vec<UserProcess>> {
    let uid = uid.to_string();
    // One -o per column: FreeBSD's ps folds `-o a=,b=` into one header.
    let mut args = vec!["-U", &uid, "-o", "pid="];
    if cfg!(target_os = "freebsd") {
        args.extend(["-o", "jail="]);
    }
    args.extend(["-o", "pcpu=", "-o", "rss=", "-o", "comm="]);
    Ok(parse_user_processes(
        &cmd::run("ps", &args)?,
        cfg!(target_os = "freebsd"),
    ))
}

fn parse_user_processes(out: &str, jails: bool) -> Vec<UserProcess> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let jail = match jails {
                true => Some(parts.next()?).filter(|j| *j != "-").map(str::to_owned),
                false => None,
            };
            Some(UserProcess {
                pid,
                jail,
                cpu_percent: parts.next()?.parse().ok()?,
                memory_kb: parts.next()?.parse().ok()?,
                command: parts.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

/// Time every processor has spent since boot, in clock ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTime {
    pub busy: u64,
    pub total: u64,
}

impl CpuTime {
    /// How busy the processors were between `earlier` and this, 0 to 100.
    pub fn percent_busy_since(&self, earlier: &CpuTime) -> f64 {
        let total = self.total.saturating_sub(earlier.total);
        let busy = self.busy.saturating_sub(earlier.busy);
        match total {
            0 => 0.0,
            _ => (busy as f64 * 100.0 / total as f64).min(100.0),
        }
    }
}

/// The processors' time so far (sample twice for how busy they are).
pub fn cpu_time() -> Result<CpuTime> {
    if cfg!(target_os = "freebsd") {
        // user nice system interrupt idle
        let ticks = cmd::run("sysctl", &["-n", "kern.cp_time"])?;
        let ticks: Vec<u64> = ticks
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        let total: u64 = ticks.iter().sum();
        Ok(CpuTime {
            busy: total - ticks.get(4).copied().unwrap_or(0),
            total,
        })
    } else if cfg!(target_os = "linux") {
        let stat = std::fs::read_to_string("/proc/stat")
            .map_err(|e| PlatformError::io("/proc/stat", e))?;
        Ok(parse_proc_stat(&stat))
    } else {
        Err(PlatformError::Unsupported("processor time".into()))
    }
}

/// /proc/stat's first line: user nice system idle iowait irq softirq steal.
fn parse_proc_stat(stat: &str) -> CpuTime {
    let ticks: Vec<u64> = stat
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("cpu "))
        .unwrap_or_default()
        .split_whitespace()
        .take(8)
        .filter_map(|t| t.parse().ok())
        .collect();
    let total: u64 = ticks.iter().sum();
    let idle = ticks.get(3).copied().unwrap_or(0) + ticks.get(4).copied().unwrap_or(0);
    CpuTime {
        busy: total - idle,
        total,
    }
}

/// Memory in use: neither free nor cache the system can take back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Memory {
    pub total_mb: u64,
    pub used_mb: u64,
}

pub fn memory() -> Result<Memory> {
    if cfg!(target_os = "freebsd") {
        let out = cmd::run(
            "sysctl",
            &[
                "-n",
                "hw.physmem",
                "hw.pagesize",
                "vm.stats.vm.v_free_count",
                "vm.stats.vm.v_inactive_count",
            ],
        )?;
        let v: Vec<u64> = out.lines().filter_map(|l| l.trim().parse().ok()).collect();
        let [total, page, free, inactive] = v[..] else {
            return Err(PlatformError::Unsupported("memory counters".into()));
        };
        let total_mb = total / 1024 / 1024;
        let available_mb = (free + inactive) * page / 1024 / 1024;
        Ok(Memory {
            total_mb,
            used_mb: total_mb.saturating_sub(available_mb),
        })
    } else {
        let load = load()?;
        Ok(Memory {
            total_mb: load.memory_total_mb,
            used_mb: load.memory_total_mb.saturating_sub(load.memory_free_mb),
        })
    }
}

/// Whether the service is enabled to run on this machine.
pub fn service_enabled(name: &str) -> bool {
    cfg!(target_os = "freebsd")
        && valid_service(name)
        && cmd::run("service", &[name, "enabled"]).is_ok()
}

/// Restarts an rc.d service. Callers check it's on IT's approved list.
pub fn restart_service(name: &str) -> Result<()> {
    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("restarting services".into()));
    }
    if !valid_service(name) {
        return Err(PlatformError::Unsafe(format!(
            "{name:?} is not a service name"
        )));
    }
    cmd::run("service", &[name, "restart"]).map(|_| ())
}

fn valid_service(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn parse_load(text: &str) -> [f64; 3] {
    let mut values = text.split_whitespace().filter_map(|v| v.parse().ok());
    [
        values.next().unwrap_or(0.0),
        values.next().unwrap_or(0.0),
        values.next().unwrap_or(0.0),
    ]
}

fn field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix(key))
        .map(|v| v.trim().to_owned())
}

/// Parses FreeBSD `ifconfig -a`: an unindented `name: flags=…<UP,…>` line
/// per interface, then indented `inet`/`inet6` lines.
fn parse_ifconfig(text: &str) -> Vec<Interface> {
    let mut out: Vec<Interface> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(char::is_whitespace) {
            let Some((name, rest)) = line.split_once(':') else {
                continue;
            };
            let flags = rest
                .split_once('<')
                .and_then(|(_, f)| f.split_once('>'))
                .map_or("", |(f, _)| f);
            out.push(Interface {
                name: name.to_owned(),
                up: flags.split(',').any(|f| f == "UP"),
                addresses: Vec::new(),
            });
        } else if let Some(iface) = out.last_mut() {
            let mut words = line.split_whitespace();
            if let (Some("inet"), Some(addr)) = (words.next(), words.next()) {
                iface.addresses.push(addr.to_owned());
            }
        }
    }
    out.retain(|i| !i.name.starts_with("lo"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_name_their_jail() {
        let out =
            "  812 glass-1a2b3c4d  12.5  20480 node\n  900 -   0.0  3100 glass-terminal\nbogus\n";
        let got = parse_user_processes(out, true);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].jail.as_deref(), Some("glass-1a2b3c4d"));
        assert_eq!((got[0].pid, got[0].memory_kb), (812, 20480));
        assert_eq!(got[0].cpu_percent, 12.5);
        assert_eq!(got[1].jail, None);
        assert_eq!(got[1].command, "glass-terminal");
        let linux = parse_user_processes(" 7 1.0 512 sh\n", false);
        assert_eq!((linux[0].pid, linux[0].jail.as_deref()), (7, None));
    }

    #[test]
    fn busy_is_the_share_of_ticks_not_idle() {
        let a = parse_proc_stat("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3\n");
        assert_eq!(
            a,
            CpuTime {
                busy: 200,
                total: 1000
            }
        );
        let b = CpuTime {
            busy: 300,
            total: 1200,
        };
        assert_eq!(b.percent_busy_since(&a), 50.0);
        assert_eq!(a.percent_busy_since(&a), 0.0);
    }

    #[test]
    fn parses_freebsd_ifconfig() {
        let text = "vtnet0: flags=1008843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST,LOWER_UP> metric 0 mtu 1500
\toptions=4c07bb<RXCSUM,TXCSUM>
\tinet 10.0.2.15 netmask 0xffffff00 broadcast 10.0.2.255
\tinet6 fe80::1%vtnet0 prefixlen 64 scopeid 0x1
lo0: flags=1008049<UP,LOOPBACK,RUNNING,MULTICAST,LOWER_UP> metric 0 mtu 16384
\tinet 127.0.0.1 netmask 0xff000000
wlan0: flags=8802<BROADCAST,SIMPLEX,MULTICAST> metric 0 mtu 1500
";
        let interfaces = parse_ifconfig(text);
        assert_eq!(interfaces.len(), 2, "loopback is left out");
        assert_eq!(interfaces[0].name, "vtnet0");
        assert!(interfaces[0].up);
        assert_eq!(interfaces[0].addresses, ["10.0.2.15"]);
        assert!(!interfaces[1].up, "wlan0 is down");
    }

    #[test]
    fn reads_load_and_gateway_fields() {
        assert_eq!(parse_load(" 0.52 0.31 0.12 "), [0.52, 0.31, 0.12]);
        let route = "   route to: default\ndestination: default\n    gateway: 10.0.2.2\n";
        assert_eq!(field(route, "gateway:").as_deref(), Some("10.0.2.2"));
    }

    #[test]
    fn service_names_are_plain_words() {
        assert!(valid_service("ntpd"));
        assert!(valid_service("openvpn_corp"));
        assert!(!valid_service("ntpd; rm -rf /"));
        assert!(!valid_service("../x"));
        assert!(!valid_service(""));
    }

    #[test]
    fn storage_and_processes_work_on_dev_hosts() {
        let usage = filesystem_usage(Path::new("/")).unwrap();
        assert!(usage.total_kb > 0 && usage.percent_used() <= 100);
        let mine = top_processes(users_uid(), 5).unwrap();
        assert!(mine.len() <= 5);
        assert!(load().unwrap().cpus >= 1);
        let computer = computer().unwrap();
        assert!(!computer.name.is_empty() && computer.memory_mb > 0);
    }

    fn users_uid() -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self").map_or(0, |m| m.uid())
    }
}
