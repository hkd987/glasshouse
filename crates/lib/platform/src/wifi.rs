//! Wi-Fi through the base system's `wpa_supplicant` (CF-12, ADR 0059).
//!
//! [`Control`] speaks wpa_supplicant's control socket as `wpa_cli` does:
//! one command per datagram, one reply. Passwords go over the socket, never
//! on a command line where `ps` would show them. Names (SSIDs) are sent
//! hex-encoded, so no character in one can change what a command says.
//!
//! [`radios`] and [`ensure_station`] find the computer's Wi-Fi radio and
//! give it its station interface (`wlan0`) the way rc.conf would at boot:
//! `wlans_<radio>="wlan0"`, with `ifconfig_wlan0="WPA DHCP"` from
//! post-install.sh, so wpa_supplicant runs on it and DHCP follows the link.

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::{PlatformError, Result, cmd};

/// Where wpa_supplicant puts its control sockets, one per interface
/// (`ctrl_interface` in /etc/wpa_supplicant.conf).
pub const CONTROL_DIR: &str = "/var/run/wpa_supplicant";
/// The station interface on the computer's radio.
pub const INTERFACE: &str = "wlan0";
/// How the organization's networks are tagged in wpa_supplicant's list
/// (`id_str`), so they're told from the person's own.
pub const ORGANIZATION: &str = "glasshouse-org";

/// How a network is secured, as its beacons say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    /// No password.
    Open,
    /// A password (WPA2 or WPA3 Personal).
    Personal,
    /// Each person signs in (802.1X): not joined from Settings yet.
    Enterprise,
}

impl Security {
    /// From a scan's flags: `[WPA2-PSK-CCMP][ESS]`, `[RSN-SAE-CCMP]`,
    /// `[WPA2-EAP-CCMP]`, `[ESS]`.
    fn from_flags(flags: &str) -> Security {
        if flags.contains("-EAP") {
            Security::Enterprise
        } else if flags.contains("-PSK") || flags.contains("-SAE") || flags.contains("WEP") {
            Security::Personal
        } else {
            Security::Open
        }
    }
}

/// A network nearby: the strongest of its access points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Nearby {
    pub name: String,
    /// dBm: -30 is next to it, -80 barely there.
    pub signal: i32,
    pub security: Security,
}

/// A network wpa_supplicant knows: one joined before, or the
/// organization's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Known {
    pub id: u32,
    pub name: String,
    /// The one in use now.
    pub current: bool,
    pub disabled: bool,
    /// Joining it failed, as a wrong password does: wpa_supplicant waits
    /// before trying again (`[TEMP-DISABLED]`).
    pub failed: bool,
    /// The organization's, set by glassd.
    pub organization: bool,
}

/// Where the radio is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Status {
    /// wpa_supplicant's state: `COMPLETED` once joined, `SCANNING`,
    /// `ASSOCIATING`, `DISCONNECTED`, `INACTIVE`.
    pub state: String,
    pub network: Option<String>,
    pub address: Option<String>,
}

impl Status {
    pub fn joined(&self) -> bool {
        self.state == "COMPLETED"
    }
}

/// One interface's control socket.
#[derive(Debug, Clone)]
pub struct Control {
    socket: PathBuf,
}

static NEXT: AtomicU32 = AtomicU32::new(0);

impl Control {
    pub fn new(dir: &Path, interface: &str) -> Self {
        Self {
            socket: dir.join(interface),
        }
    }

    /// wpa_supplicant is running on the interface.
    pub fn available(&self) -> bool {
        self.request("PING").is_ok_and(|r| r.trim() == "PONG")
    }

    /// Sends `command` and returns the reply. Our end is a socket of its
    /// own next to the server's, removed after.
    pub fn request(&self, command: &str) -> Result<String> {
        let dir = self.socket.parent().unwrap_or(Path::new("/tmp"));
        let ours = dir.join(format!(
            ".glass-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&ours);
        let io = |e| PlatformError::io(format!("wpa_supplicant ({})", self.socket.display()), e);
        let socket = UnixDatagram::bind(&ours).map_err(io)?;
        let reply = (|| {
            socket.set_read_timeout(Some(Duration::from_secs(5)))?;
            socket.connect(&self.socket)?;
            socket.send(command.as_bytes())?;
            let mut buf = vec![0u8; 64 * 1024];
            let n = socket.recv(&mut buf)?;
            Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
        })();
        let _ = std::fs::remove_file(&ours);
        reply.map_err(io)
    }

    /// A command that answers `OK`.
    fn ok(&self, command: &str) -> Result<()> {
        let reply = self.request(command)?;
        if reply.trim() == "OK" {
            Ok(())
        } else {
            // Never echo the command: it may carry a password.
            Err(PlatformError::Command {
                command: command
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" "),
                status: "refused".into(),
                stderr: reply.trim().into(),
            })
        }
    }

    pub fn status(&self) -> Result<Status> {
        Ok(parse_status(&self.request("STATUS")?))
    }

    /// Starts a scan; its results come in a few seconds. Busy scanning
    /// already is fine.
    pub fn scan(&self) -> Result<()> {
        match self.request("SCAN")?.trim() {
            "OK" | "FAIL-BUSY" => Ok(()),
            other => Err(PlatformError::Command {
                command: "SCAN".into(),
                status: "refused".into(),
                stderr: other.into(),
            }),
        }
    }

    /// The networks the last scan saw, strongest first, one line per name.
    pub fn nearby(&self) -> Result<Vec<Nearby>> {
        Ok(parse_scan_results(&self.request("SCAN_RESULTS")?))
    }

    /// The networks wpa_supplicant knows, with which are the
    /// organization's.
    pub fn known(&self) -> Result<Vec<Known>> {
        let mut known = parse_networks(&self.request("LIST_NETWORKS")?);
        for k in &mut known {
            let tag = self.request(&format!("GET_NETWORK {} id_str", k.id))?;
            k.organization = tag.trim() == format!("\"{ORGANIZATION}\"");
        }
        Ok(known)
    }

    /// Adds `name`, open or with `password`, enabled; the organization's
    /// with a higher priority, so it's chosen over the person's own. Its id.
    pub fn add(&self, name: &str, password: Option<&str>, organization: bool) -> Result<u32> {
        check_name(name)?;
        if let Some(p) = password {
            check_password(p)?;
        }
        let reply = self.request("ADD_NETWORK")?;
        let id: u32 = reply.trim().parse().map_err(|_| PlatformError::Command {
            command: "ADD_NETWORK".into(),
            status: "refused".into(),
            stderr: reply.trim().into(),
        })?;
        let set = || -> Result<()> {
            self.ok(&format!("SET_NETWORK {id} ssid {}", hex(name)))?;
            match password {
                Some(p) => {
                    // WPA2 and WPA3 Personal both: SAE with management
                    // frame protection where the network offers it.
                    self.ok(&format!(
                        "SET_NETWORK {id} key_mgmt WPA-PSK WPA-PSK-SHA256 SAE"
                    ))?;
                    self.ok(&format!("SET_NETWORK {id} ieee80211w 1"))?;
                    self.ok(&format!("SET_NETWORK {id} psk \"{p}\""))?;
                    self.ok(&format!("SET_NETWORK {id} sae_password \"{p}\""))?;
                }
                None => self.ok(&format!("SET_NETWORK {id} key_mgmt NONE"))?,
            }
            if organization {
                self.ok(&format!("SET_NETWORK {id} id_str \"{ORGANIZATION}\""))?;
                self.ok(&format!("SET_NETWORK {id} priority 10"))?;
            }
            self.ok(&format!("ENABLE_NETWORK {id}"))
        };
        if let Err(e) = set() {
            let _ = self.ok(&format!("REMOVE_NETWORK {id}"));
            return Err(e);
        }
        Ok(id)
    }

    /// Joins `id` now, leaving the rest enabled to fall back on.
    pub fn select(&self, id: u32) -> Result<()> {
        self.ok(&format!("SELECT_NETWORK {id}"))?;
        // SELECT disables every other network; enable them again, so the
        // computer still joins them where this one isn't.
        for k in parse_networks(&self.request("LIST_NETWORKS")?) {
            if k.id != id {
                let _ = self.ok(&format!("ENABLE_NETWORK {}", k.id));
            }
        }
        Ok(())
    }

    pub fn disable(&self, id: u32) -> Result<()> {
        self.ok(&format!("DISABLE_NETWORK {id}"))
    }

    pub fn remove(&self, id: u32) -> Result<()> {
        self.ok(&format!("REMOVE_NETWORK {id}"))
    }

    /// Writes the known networks to /etc/wpa_supplicant.conf (root only),
    /// so they're joined after a restart.
    pub fn save(&self) -> Result<()> {
        self.ok("SAVE_CONFIG")
    }
}

/// A network's name as wpa_supplicant takes it: 1 to 32 bytes.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 32 || name.chars().any(char::is_control) {
        return Err(PlatformError::Unsafe(
            "a Wi-Fi network's name is 1 to 32 characters, none of them control characters".into(),
        ));
    }
    Ok(())
}

/// A password as WPA Personal takes it: 8 to 63 printable characters.
pub fn check_password(password: &str) -> Result<()> {
    if !(8..=63).contains(&password.len()) || !password.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(PlatformError::Unsafe(
            "a Wi-Fi password is 8 to 63 letters, digits, spaces or symbols".into(),
        ));
    }
    Ok(())
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// Undoes wpa_supplicant's escaping of names it prints (`printf_encode`):
/// `\\`, `\"`, `\n`, `\r`, `\t`, `\e` and `\xNN`.
fn unescape(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut it = s.bytes().peekable();
    while let Some(b) = it.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match it.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'e') => bytes.push(0x1b),
            Some(b'x') => {
                let hi = it.next();
                let lo = it.next();
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        match u8::from_str_radix(std::str::from_utf8(&[h, l]).unwrap_or("zz"), 16) {
                            Ok(v) => bytes.push(v),
                            Err(_) => bytes.extend_from_slice(&[b'\\', b'x', h, l]),
                        }
                    }
                    _ => bytes.extend_from_slice(b"\\x"),
                }
            }
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `STATUS`: `key=value` lines.
pub fn parse_status(text: &str) -> Status {
    let mut status = Status::default();
    for line in text.lines() {
        match line.split_once('=') {
            Some(("wpa_state", v)) => status.state = v.into(),
            Some(("ssid", v)) => status.network = Some(unescape(v)),
            Some(("ip_address", v)) => status.address = Some(v.into()),
            _ => {}
        }
    }
    if !status.joined() {
        status.network = None;
        status.address = None;
    }
    status
}

/// `SCAN_RESULTS`: a header, then `bssid\tfrequency\tsignal\tflags\tssid`.
/// One line per name, its strongest access point; hidden networks (no
/// name) left out; strongest first.
pub fn parse_scan_results(text: &str) -> Vec<Nearby> {
    let mut best: Vec<Nearby> = Vec::new();
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        let [_, _, signal, flags, ssid] = fields[..] else {
            continue;
        };
        let name = unescape(ssid);
        if name.is_empty() || name.bytes().all(|b| b == 0) {
            continue;
        }
        let signal: i32 = signal.trim().parse().unwrap_or(-100);
        let security = Security::from_flags(flags);
        match best.iter_mut().find(|n| n.name == name) {
            Some(n) if n.signal >= signal => {}
            Some(n) => {
                n.signal = signal;
                n.security = security;
            }
            None => best.push(Nearby {
                name,
                signal,
                security,
            }),
        }
    }
    best.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.name.cmp(&b.name)));
    best
}

/// `LIST_NETWORKS`: a header, then `id\tssid\tbssid\tflags`.
pub fn parse_networks(text: &str) -> Vec<Known> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let id = fields.first()?.trim().parse().ok()?;
            let flags = fields.get(3).copied().unwrap_or_default();
            Some(Known {
                id,
                name: unescape(fields.get(1).copied().unwrap_or_default()),
                current: flags.contains("[CURRENT]"),
                disabled: flags.contains("[DISABLED]"),
                failed: flags.contains("[TEMP-DISABLED]"),
                organization: false,
            })
        })
        .collect()
}

/// The Wi-Fi radios drivers attached (`net.wlan.devices`), such as
/// `iwlwifi0`.
pub fn radios() -> Result<Vec<String>> {
    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("Wi-Fi".into()));
    }
    Ok(cmd::run("sysctl", &["-n", "net.wlan.devices"])
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

/// Gives `radio` the station interface, as rc.conf does at every start
/// after this (`wlans_<radio>`): wpa_supplicant runs on it and DHCP follows
/// the link. Nothing to do when the interface is there.
pub fn ensure_station(radio: &str) -> Result<()> {
    if !radio.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(PlatformError::Unsafe(format!(
            "{radio:?} isn't a radio's name"
        )));
    }
    if cmd::run("ifconfig", &[INTERFACE]).is_ok() {
        return Ok(());
    }
    cmd::run("sysrc", &[&format!("wlans_{radio}={INTERFACE}")])?;
    cmd::run("service", &["netif", "start", INTERFACE]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf12_reads_what_wpa_supplicant_says() {
        let status = parse_status(
            "bssid=02:00:00:00:01:00\nfreq=2437\nssid=Glass Lab\nid=0\nmode=station\n\
             pairwise_cipher=CCMP\nkey_mgmt=WPA2-PSK\nwpa_state=COMPLETED\n\
             ip_address=192.168.4.20\naddress=02:00:00:00:00:00\n",
        );
        assert!(status.joined());
        assert_eq!(status.network.as_deref(), Some("Glass Lab"));
        assert_eq!(status.address.as_deref(), Some("192.168.4.20"));
        let scanning = parse_status("wpa_state=SCANNING\nssid=stale\n");
        assert!(!scanning.joined());
        assert_eq!(scanning.network, None);

        let nearby = parse_scan_results(
            "bssid / frequency / signal level / flags / ssid\n\
             02:00:00:00:01:00\t2437\t-71\t[WPA2-PSK-CCMP][ESS]\tGlass Lab\n\
             02:00:00:00:01:01\t5180\t-48\t[WPA2-PSK-CCMP][ESS]\tGlass Lab\n\
             02:00:00:00:02:00\t2412\t-60\t[ESS]\tCaf\\xc3\\xa9 \\\"Guest\\\"\n\
             02:00:00:00:03:00\t2412\t-55\t[WPA2-EAP-CCMP][ESS]\tCorp\n\
             02:00:00:00:04:00\t2412\t-40\t[RSN-SAE-CCMP][ESS]\tHome 6E\n\
             02:00:00:00:05:00\t2412\t-30\t[WPA2-PSK-CCMP][ESS]\t\n",
        );
        let names: Vec<&str> = nearby.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Home 6E", "Glass Lab", "Corp", "Café \"Guest\""]);
        assert_eq!(nearby[1].signal, -48, "the strongest access point");
        assert_eq!(nearby[0].security, Security::Personal);
        assert_eq!(nearby[2].security, Security::Enterprise);
        assert_eq!(nearby[3].security, Security::Open);

        let known = parse_networks(
            "network id / ssid / bssid / flags\n0\tGlass Lab\tany\t[CURRENT]\n\
             1\tCorp\tany\t[DISABLED]\n2\tOld\tany\t[TEMP-DISABLED]\n",
        );
        assert_eq!(known.len(), 3);
        assert!(known[0].current && !known[0].disabled);
        assert!(known[1].disabled && !known[1].failed);
        assert!(known[2].failed && !known[2].disabled);
    }

    #[test]
    fn cf12_names_and_passwords_can_t_change_a_command() {
        assert_eq!(hex("a\" b"), "61222062");
        assert!(check_name("").is_err());
        assert!(check_name(&"x".repeat(33)).is_err());
        assert!(check_name("Lab\nSET_NETWORK").is_err());
        assert!(check_name("Café \"Guest\"").is_ok());
        assert!(check_password("short").is_err());
        assert!(check_password("correct horse\nbattery").is_err());
        assert!(check_password("pa\"ss word ok").is_ok());
        assert!(check_password(&"x".repeat(64)).is_err());
    }

    /// The control socket, against a stand-in that answers as
    /// wpa_supplicant does.
    #[test]
    fn cf12_talks_to_the_control_socket() {
        let dir = tempfile::tempdir().unwrap();
        let server = UnixDatagram::bind(dir.path().join("wlan0")).unwrap();
        let handle = std::thread::spawn(move || {
            let mut said = Vec::new();
            let mut buf = [0u8; 4096];
            for _ in 0..10 {
                let (n, from) = server.recv_from(&mut buf).unwrap();
                let command = String::from_utf8_lossy(&buf[..n]).into_owned();
                let reply = match command.as_str() {
                    "PING" => "PONG\n",
                    "ADD_NETWORK" => "3\n",
                    _ => "OK\n",
                };
                server
                    .send_to(reply.as_bytes(), from.as_pathname().unwrap())
                    .unwrap();
                said.push(command);
            }
            said
        });
        let control = Control::new(dir.path(), "wlan0");
        assert!(control.available());
        assert_eq!(
            control
                .add("Glass Lab", Some("correct horse"), true)
                .unwrap(),
            3
        );
        let said = handle.join().unwrap();
        assert_eq!(said[1], "ADD_NETWORK");
        assert_eq!(said[2], "SET_NETWORK 3 ssid 476c617373204c6162");
        assert!(
            said.iter()
                .any(|c| c == "SET_NETWORK 3 psk \"correct horse\"")
        );
        // Our ends of the conversation are gone.
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1);
    }
}
