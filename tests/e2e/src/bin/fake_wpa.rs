//! `glass-fake-wpa <dir> [<interface>]`: wpa_supplicant's control socket,
//! as far as glassd uses it (CF-12, ADR 0059), with networks in the air and
//! no radio. For the development host and the apps' tests, which have no
//! Wi-Fi; the VM's emulated hardware has none either (QEMU emulates no
//! Wi-Fi card, and wtap(4) only does ad hoc and mesh).
//!
//! In the air: Northwind Staff (a password: "staff-password-not-real"), Café
//! Guest (open), Free Airport WiFi (open), Northwind Corp (802.1X) and Home 6E
//! (a password: "correct horse battery"). A network joins when it's in the
//! air and its password is right; then the computer has 192.168.4.20.
//! Exits when its parent does.

use std::collections::BTreeMap;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::time::Duration;

struct Air {
    name: &'static str,
    signal: i32,
    flags: &'static str,
    password: Option<&'static str>,
}

const AIR: [Air; 5] = [
    Air {
        name: "Northwind Staff",
        signal: -42,
        flags: "[WPA2-PSK-CCMP][ESS]",
        password: Some("staff-password-not-real"),
    },
    Air {
        name: "Café Guest",
        signal: -58,
        flags: "[ESS]",
        password: None,
    },
    Air {
        name: "Free Airport WiFi",
        signal: -63,
        flags: "[ESS]",
        password: None,
    },
    Air {
        name: "Northwind Corp",
        signal: -66,
        flags: "[WPA2-EAP-CCMP][ESS]",
        password: None,
    },
    Air {
        name: "Home 6E",
        signal: -79,
        flags: "[RSN-SAE-CCMP][ESS]",
        password: Some("correct horse battery"),
    },
];

#[derive(Default, Clone)]
struct Network {
    ssid: String,
    psk: Option<String>,
    id_str: Option<String>,
    disabled: bool,
}

#[derive(Default)]
struct State {
    networks: BTreeMap<u32, Network>,
    next: u32,
    current: Option<u32>,
}

impl State {
    /// The network in use: the selected one if it can join, else the
    /// first enabled one that can (organization's first, by priority).
    fn joins(&self, id: u32) -> bool {
        let Some(n) = self.networks.get(&id) else {
            return false;
        };
        AIR.iter().any(|a| {
            a.name == n.ssid
                && !a.flags.contains("EAP")
                && match a.password {
                    None => n.psk.is_none(),
                    Some(p) => n.psk.as_deref() == Some(p),
                }
        })
    }

    fn settle(&mut self) {
        if self
            .current
            .is_some_and(|c| self.networks.get(&c).is_none_or(|n| n.disabled) || !self.joins(c))
        {
            self.current = None;
        }
        if self.current.is_none() {
            let mut ids: Vec<u32> = self
                .networks
                .iter()
                .filter(|(_, n)| !n.disabled)
                .map(|(id, _)| *id)
                .collect();
            ids.sort_by_key(|id| self.networks[id].id_str.is_none());
            self.current = ids.into_iter().find(|id| self.joins(*id));
        }
    }

    fn handle(&mut self, command: &str) -> String {
        let mut words = command.splitn(4, ' ');
        let verb = words.next().unwrap_or_default();
        let id = words.next().and_then(|w| w.parse::<u32>().ok());
        let ok = "OK\n".to_owned();
        match verb {
            "PING" => "PONG\n".into(),
            "SCAN" | "SAVE_CONFIG" => ok,
            "STATUS" => match self.current.map(|c| &self.networks[&c]) {
                Some(n) => format!(
                    "bssid=02:00:00:00:01:00\nssid={}\nid={}\nmode=station\nwpa_state=COMPLETED\n\
                     ip_address=192.168.4.20\n",
                    escape(&n.ssid),
                    self.current.unwrap_or_default()
                ),
                None => "wpa_state=DISCONNECTED\n".into(),
            },
            "SCAN_RESULTS" => {
                let mut out = String::from("bssid / frequency / signal level / flags / ssid\n");
                for (i, a) in AIR.iter().enumerate() {
                    out += &format!(
                        "02:00:00:00:0{i}:00\t2437\t{}\t{}\t{}\n",
                        a.signal,
                        a.flags,
                        escape(a.name)
                    );
                }
                out
            }
            "LIST_NETWORKS" => {
                let mut out = String::from("network id / ssid / bssid / flags\n");
                for (id, n) in &self.networks {
                    let in_air = AIR.iter().any(|a| a.name == n.ssid);
                    let flags = if self.current == Some(*id) {
                        "[CURRENT]"
                    } else if n.disabled {
                        "[DISABLED]"
                    } else if in_air && !self.joins(*id) {
                        // A wrong password: wpa_supplicant waits before
                        // trying it again.
                        "[TEMP-DISABLED]"
                    } else {
                        ""
                    };
                    out += &format!("{id}\t{}\tany\t{flags}\n", escape(&n.ssid));
                }
                out
            }
            "ADD_NETWORK" => {
                let id = self.next;
                self.next += 1;
                self.networks.insert(
                    id,
                    Network {
                        disabled: true,
                        ..Network::default()
                    },
                );
                format!("{id}\n")
            }
            "SET_NETWORK" => {
                let (Some(id), Some(key), Some(value)) = (id, words.next(), words.next()) else {
                    return "FAIL\n".into();
                };
                let Some(n) = self.networks.get_mut(&id) else {
                    return "FAIL\n".into();
                };
                let quoted = || value.strip_prefix('"').and_then(|v| v.strip_suffix('"'));
                match key {
                    "ssid" => match unhex(value) {
                        Some(ssid) => n.ssid = ssid,
                        None => return "FAIL\n".into(),
                    },
                    "psk" => n.psk = quoted().map(str::to_owned),
                    "id_str" => n.id_str = quoted().map(str::to_owned),
                    _ => {}
                }
                ok
            }
            "GET_NETWORK" => match (id.and_then(|id| self.networks.get(&id)), words.next()) {
                (Some(n), Some("id_str")) => match &n.id_str {
                    Some(s) => format!("\"{s}\""),
                    None => "FAIL\n".into(),
                },
                _ => "FAIL\n".into(),
            },
            "ENABLE_NETWORK" | "DISABLE_NETWORK" | "SELECT_NETWORK" | "REMOVE_NETWORK" => {
                let Some(id) = id.filter(|id| self.networks.contains_key(id)) else {
                    return "FAIL\n".into();
                };
                match verb {
                    "ENABLE_NETWORK" => self.networks.get_mut(&id).map(|n| n.disabled = false),
                    "DISABLE_NETWORK" => self.networks.get_mut(&id).map(|n| n.disabled = true),
                    "REMOVE_NETWORK" => self.networks.remove(&id).map(drop),
                    _ => {
                        for (other, n) in &mut self.networks {
                            n.disabled = *other != id;
                        }
                        self.current = Some(id);
                        Some(())
                    }
                };
                self.settle();
                ok
            }
            _ => "UNKNOWN COMMAND\n".into(),
        }
    }
}

/// As wpa_supplicant prints names: printable ASCII as it is, the rest
/// `\xNN`.
fn escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'\\' => "\\\\".into(),
            b'"' => "\\\"".into(),
            0x20..=0x7e => (b as char).to_string(),
            _ => format!("\\x{b:02x}"),
        })
        .collect()
}

fn unhex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(
        args.next()
            .expect("usage: glass-fake-wpa <dir> [<interface>]"),
    );
    let interface = args.next().unwrap_or_else(|| "wlan0".into());
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&interface);
    let _ = std::fs::remove_file(&path);
    let socket = UnixDatagram::bind(&path)?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let parent = std::os::unix::process::parent_id();
    let mut state = State::default();
    let mut buf = vec![0u8; 4096];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                let command = String::from_utf8_lossy(&buf[..n]).into_owned();
                let reply = state.handle(command.trim_end());
                if let Some(to) = from.as_pathname() {
                    let _ = socket.send_to(reply.as_bytes(), to);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
        // Gone with whatever started it.
        if std::os::unix::process::parent_id() != parent {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
    }
}
