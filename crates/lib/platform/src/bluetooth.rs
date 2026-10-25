//! Classic Bluetooth (CF-13, ADR 0062), through the base system's stack.
//!
//! - The controller is a USB `ubt` device. `service bluetooth start ubt0`
//!   gives it its netgraph nodes (`ubt0hci`, l2cap); without them, it's off.
//! - `hccontrol` looks for devices nearby (an inquiry), asks their names,
//!   and lists the connections open.
//! - `hcsecd` answers a device's pairing with a PIN from
//!   `/etc/bluetooth/hcsecd.conf`, and keeps its link key afterwards in
//!   `/var/db/hcsecd.keys`.
//! - `bthidd` connects keyboards, mice and other input devices, each with a
//!   block in `/etc/bluetooth/bthidd.conf` that `bthidcontrol query` reads
//!   from the device. Its rc script's `-u` sends their input through evdev,
//!   as the desktop reads it. It's enabled while one is paired: an empty
//!   bthidd.conf is a syntax error to it.
//!
//! What glassd writes into those files is marked, so what IT or a person
//! wrote by hand there is kept. Headsets and Bluetooth LE aren't in the base
//! system's stack.
//!
//! [`Controller::StandIn`] is a file that stands in for a computer with no
//! controller (development, tests).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{PlatformError, Result, cmd};

/// The mark on the configuration blocks glassd writes.
const MARK: &str = "# glasshouse class";

/// A device, as an inquiry or the pairing files know it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// `00:1f:20:3a:4b:5c`, in lower case.
    pub address: String,
    pub name: String,
    /// Its Class of Device: what kind of device it says it is.
    pub class: u32,
}

impl Device {
    pub fn kind(&self) -> Kind {
        kind(self.class)
    }
}

/// What a device is, from its Class of Device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Keyboard,
    Mouse,
    KeyboardAndMouse,
    GameController,
    /// Another input device: a pen tablet, a remote.
    Input,
    /// Headphones and speakers: the base system has no Bluetooth audio.
    Audio,
    Phone,
    Computer,
    Other,
}

impl Kind {
    /// Input devices: what `bthidd` connects, and all an organization that
    /// allows only input devices lets a computer pair.
    pub fn input(self) -> bool {
        matches!(
            self,
            Kind::Keyboard
                | Kind::Mouse
                | Kind::KeyboardAndMouse
                | Kind::GameController
                | Kind::Input
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Keyboard => "Keyboard",
            Kind::Mouse => "Mouse",
            Kind::KeyboardAndMouse => "Keyboard and mouse",
            Kind::GameController => "Game controller",
            Kind::Input => "Input device",
            Kind::Audio => "Headphones or speaker",
            Kind::Phone => "Phone",
            Kind::Computer => "Computer",
            Kind::Other => "Device",
        }
    }
}

/// What a device is, from its Class of Device: the major class in bits 8 to
/// 12, and for a peripheral, keyboard and pointer in bits 6 and 7.
pub fn kind(class: u32) -> Kind {
    match (class >> 8) & 0x1f {
        0x01 => Kind::Computer,
        0x02 => Kind::Phone,
        0x04 => Kind::Audio,
        0x05 => match ((class >> 6) & 0x3, (class >> 2) & 0xf) {
            (1, _) => Kind::Keyboard,
            (2, _) => Kind::Mouse,
            (3, _) => Kind::KeyboardAndMouse,
            (0, 1 | 2) => Kind::GameController,
            _ => Kind::Input,
        },
        _ => Kind::Other,
    }
}

/// Whether `address` is a device's address: six bytes in hex, with colons.
pub fn valid_address(address: &str) -> bool {
    let parts: Vec<&str> = address.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A PIN for the person to type on the device they pair: six digits, from
/// the kernel's random numbers.
pub fn pin() -> Result<String> {
    use std::io::Read as _;
    let mut bytes = [0u8; 4];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| PlatformError::io("/dev/urandom", e))?;
    Ok(format!("{:06}", u32::from_le_bytes(bytes) % 1_000_000))
}

/// A device's name as the configuration files may carry it: one line, no
/// quotes or braces, 64 characters at most.
fn plain_name(name: &str) -> String {
    let plain: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '\\' | '{' | '}' | ';' | '#'))
        .take(64)
        .collect();
    let plain = plain.trim();
    if plain.is_empty() {
        "Bluetooth device".into()
    } else {
        plain.to_owned()
    }
}

// ---- what the base system's tools print -----------------------------------

/// The controllers `ngctl list` shows: its `ubt` nodes.
pub fn parse_controllers(ngctl: &str) -> Vec<String> {
    nodes(ngctl)
        .filter(|(_, kind)| *kind == "ubt")
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Whether the stack is up on `controller`: its HCI node is there.
pub fn parse_on(ngctl: &str, controller: &str) -> bool {
    let hci = format!("{controller}hci");
    nodes(ngctl).any(|(name, kind)| name == hci && kind == "hci")
}

fn nodes(ngctl: &str) -> impl Iterator<Item = (&str, &str)> {
    ngctl.lines().filter_map(|line| {
        let name = line.split("Name:").nth(1)?.split_whitespace().next()?;
        let kind = line.split("Type:").nth(1)?.split_whitespace().next()?;
        Some((name, kind))
    })
}

/// The devices an inquiry found, with their classes; their names come from
/// [`parse_remote_name`].
pub fn parse_inquiry(text: &str) -> Vec<(String, u32)> {
    let mut found: Vec<(String, u32)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(address) = line.strip_prefix("BD_ADDR:") {
            let address = address.trim().to_ascii_lowercase();
            if valid_address(&address) && !found.iter().any(|(a, _)| *a == address) {
                found.push((address, 0));
            }
        } else if let (Some(class), Some(last)) = (line.strip_prefix("Class:"), found.last_mut()) {
            // `52:02:04`, the most significant byte first.
            let hex: String = class.trim().split(':').collect();
            last.1 = u32::from_str_radix(&hex, 16).unwrap_or(0);
        }
    }
    found
}

pub fn parse_remote_name(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Name:"))
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
}

/// The devices with a connection open, from `read_connection_list`.
pub fn parse_connections(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.trim_end().ends_with("OPEN"))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_ascii_lowercase)
        .filter(|a| valid_address(a))
        .collect()
}

// ---- hcsecd.conf and bthidd.conf ------------------------------------------

/// A configuration file's top-level `device { ... }` blocks, each with the
/// address it names, and what lies between them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Blocks {
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Device { address: String, text: String },
}

impl Blocks {
    fn parse(text: &str) -> Self {
        let mut parts = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("device") {
            let Some(open) = rest[start..].find('{').map(|o| start + o) else {
                break;
            };
            if !rest[start + "device".len()..open].trim().is_empty()
                || !rest[..start].ends_with(['\n', ' ', '\t']) && !rest[..start].is_empty()
            {
                // Not a block's opening: a word in a comment.
                parts.push(Part::Text(rest[..open + 1].to_owned()));
                rest = &rest[open + 1..];
                continue;
            }
            let mut depth = 0usize;
            let mut end = None;
            for (i, c) in rest[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(mut end) = end else { break };
            // A block may end `};`.
            if rest[end..].starts_with(';') {
                end += 1;
            }
            parts.push(Part::Text(rest[..start].to_owned()));
            let block = &rest[start..end];
            let address = field(block, "bdaddr")
                .map(|a| a.to_ascii_lowercase())
                .unwrap_or_default();
            parts.push(Part::Device {
                address,
                text: block.to_owned(),
            });
            rest = &rest[end..];
        }
        parts.push(Part::Text(rest.to_owned()));
        Self { parts }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Text(t) | Part::Device { text: t, .. } => out.push_str(t),
            }
        }
        out
    }

    fn devices(&self) -> impl Iterator<Item = (&str, &str)> {
        self.parts.iter().filter_map(|p| match p {
            Part::Device { address, text } => Some((address.as_str(), text.as_str())),
            Part::Text(_) => None,
        })
    }

    /// Drops `address`'s block; whether there was one.
    fn remove(&mut self, address: &str) -> bool {
        let before = self.parts.len();
        self.parts
            .retain(|p| !matches!(p, Part::Device { address: a, .. } if a == address));
        self.parts.len() != before
    }

    fn push(&mut self, address: &str, block: &str) {
        let text = self.render();
        if !text.is_empty() && !text.ends_with('\n') {
            self.parts.push(Part::Text("\n".into()));
        }
        self.parts.push(Part::Device {
            address: address.to_owned(),
            text: block.trim_end().to_owned(),
        });
        self.parts.push(Part::Text("\n".into()));
    }
}

/// A field's value in a block: `bdaddr 00:...;`, `name "..."`.
fn field<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    block.lines().find_map(|line| {
        let line = line.trim();
        let value = line.strip_prefix(name)?;
        if !value.starts_with([' ', '\t']) {
            return None;
        }
        Some(value.trim().trim_end_matches(';').trim().trim_matches('"'))
    })
}

/// The devices glassd paired, from `hcsecd.conf`.
fn parse_paired(hcsecd: &str) -> Vec<Device> {
    Blocks::parse(hcsecd)
        .devices()
        .filter_map(|(address, text)| {
            let class = text
                .lines()
                .find_map(|l| l.trim().strip_prefix(MARK))
                .and_then(|c| u32::from_str_radix(c.trim().trim_start_matches("0x"), 16).ok())?;
            Some(Device {
                address: address.to_owned(),
                name: field(text, "name").unwrap_or_default().to_owned(),
                class,
            })
        })
        .collect()
}

/// `device`'s block in `hcsecd.conf`, with the PIN it pairs with.
fn hcsecd_block(device: &Device, pin: &str) -> String {
    format!(
        "device {{\n\t{MARK} 0x{:06x}\n\tbdaddr\t{};\n\tname\t\"{}\";\n\tkey\tnokey;\n\tpin\t\"{pin}\";\n}}",
        device.class,
        device.address,
        plain_name(&device.name)
    )
}

/// What `bthidcontrol query` printed, marked as glassd's.
fn bthidd_block(query: &str, device: &Device) -> Result<String> {
    let blocks = Blocks::parse(query);
    let text = blocks
        .devices()
        .find(|(address, _)| *address == device.address)
        .map(|(_, text)| text.to_owned())
        .ok_or_else(|| {
            PlatformError::Unsupported(format!("{} as an input device", device.address))
        })?;
    Ok(text.replacen('{', &format!("{{\n\t{MARK} 0x{:06x}", device.class), 1))
}

// ---- the controller --------------------------------------------------------

/// The computer's Bluetooth, or a stand-in for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Controller {
    /// The base system's stack on a `ubt` controller.
    System {
        controller: String,
        /// `/etc/bluetooth`.
        etc: PathBuf,
        /// `/var/db/hcsecd.keys`.
        keys: PathBuf,
    },
    /// A JSON file ([`StandIn`]): a computer with no controller.
    StandIn(PathBuf),
}

/// What [`Controller::StandIn`]'s file holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandIn {
    #[serde(default = "yes")]
    pub on: bool,
    #[serde(default)]
    pub nearby: Vec<Device>,
    #[serde(default)]
    pub paired: Vec<Device>,
    #[serde(default)]
    pub connected: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Controller {
    /// The computer's first controller, if it has one.
    pub fn find() -> Option<Self> {
        if !cfg!(target_os = "freebsd") {
            return None;
        }
        let ngctl = cmd::run("ngctl", &["list"]).ok()?;
        let controller = parse_controllers(&ngctl).into_iter().next()?;
        Some(Self::System {
            controller,
            etc: "/etc/bluetooth".into(),
            keys: "/var/db/hcsecd.keys".into(),
        })
    }

    pub fn stand_in(path: impl Into<PathBuf>) -> Self {
        Self::StandIn(path.into())
    }

    fn hci(controller: &str) -> String {
        format!("{controller}hci")
    }

    /// Whether the stack is up: devices can be found and connect.
    pub fn on(&self) -> Result<bool> {
        match self {
            Self::System { controller, .. } => {
                Ok(parse_on(&cmd::run("ngctl", &["list"])?, controller))
            }
            Self::StandIn(path) => Ok(read_stand_in(path)?.on),
        }
    }

    /// Brings the stack up or down. Down, nothing connects, and nothing
    /// paired is forgotten.
    pub fn set_on(&self, on: bool) -> Result<()> {
        match self {
            Self::System { controller, .. } => {
                if on {
                    cmd::run("service", &["bluetooth", "start", controller])?;
                    let _ = cmd::run("service", &["hcsecd", "onestart"]);
                    let _ = cmd::run("service", &["bthidd", "onestart"]);
                } else {
                    let _ = cmd::run("service", &["bthidd", "onestop"]);
                    cmd::run("service", &["bluetooth", "stop", controller])?;
                }
                Ok(())
            }
            Self::StandIn(path) => edit_stand_in(path, |s| s.on = on),
        }
    }

    /// The devices nearby that answer an inquiry, with their names: about
    /// ten seconds.
    pub fn inquiry(&self) -> Result<Vec<Device>> {
        match self {
            Self::System { controller, .. } => {
                let hci = Self::hci(controller);
                let text = cmd::run("hccontrol", &["-n", &hci, "inquiry"])?;
                Ok(parse_inquiry(&text)
                    .into_iter()
                    .map(|(address, class)| {
                        let name =
                            cmd::run("hccontrol", &["-n", &hci, "remote_name_request", &address])
                                .ok()
                                .and_then(|t| parse_remote_name(&t))
                                .unwrap_or_default();
                        Device {
                            address,
                            name,
                            class,
                        }
                    })
                    .collect())
            }
            Self::StandIn(path) => Ok(read_stand_in(path)?.nearby),
        }
    }

    /// The devices with a connection open.
    pub fn connected(&self) -> Result<Vec<String>> {
        match self {
            Self::System { controller, .. } => Ok(parse_connections(&cmd::run(
                "hccontrol",
                &["-n", &Self::hci(controller), "read_connection_list"],
            )?)),
            Self::StandIn(path) => Ok(read_stand_in(path)?.connected),
        }
    }

    /// The devices glassd paired.
    pub fn paired(&self) -> Result<Vec<Device>> {
        match self {
            Self::System { etc, .. } => Ok(parse_paired(&read(&etc.join("hcsecd.conf"))?)),
            Self::StandIn(path) => Ok(read_stand_in(path)?.paired),
        }
    }

    /// Pairs `device` with `pin`, which the person types on it: hcsecd
    /// answers with it, and keeps the link key. An input device is
    /// connected by bthidd from then on.
    pub fn pair(&self, device: &Device, pin: &str) -> Result<()> {
        if !valid_address(&device.address) || !pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err(PlatformError::Unsafe(format!(
                "{:?} isn't a device's address",
                device.address
            )));
        }
        match self {
            Self::System { etc, .. } => {
                let path = etc.join("hcsecd.conf");
                let mut blocks = Blocks::parse(&read(&path)?);
                blocks.remove(&device.address);
                blocks.push(&device.address, &hcsecd_block(device, pin));
                write(&path, &blocks.render())?;
                cmd::run("service", &["hcsecd", "onerestart"])?;
                if device.kind().input() {
                    // Asks the device what it is: its HID descriptor.
                    let query = cmd::run(
                        "timeout",
                        &["30", "bthidcontrol", "-a", &device.address, "query"],
                    )?;
                    let path = etc.join("bthidd.conf");
                    let mut blocks = Blocks::parse(&read(&path)?);
                    blocks.remove(&device.address);
                    blocks.push(&device.address, &bthidd_block(&query, device)?);
                    write(&path, &blocks.render())?;
                    // bthidd runs while an input device is paired: an empty
                    // bthidd.conf is a syntax error to it.
                    cmd::run("sysrc", &["bthidd_enable=YES"])?;
                    cmd::run("service", &["bthidd", "restart"])?;
                }
                Ok(())
            }
            Self::StandIn(path) => edit_stand_in(path, |s| {
                s.paired.retain(|d| d.address != device.address);
                s.paired.push(device.clone());
                if !s.connected.contains(&device.address) {
                    s.connected.push(device.address.clone());
                }
            }),
        }
    }

    /// Forgets `address`: its pairing, its link key and its input block.
    pub fn remove(&self, address: &str) -> Result<()> {
        if !valid_address(address) {
            return Err(PlatformError::Unsafe(format!(
                "{address:?} isn't a device's address"
            )));
        }
        match self {
            Self::System { etc, keys, .. } => {
                let path = etc.join("hcsecd.conf");
                let mut blocks = Blocks::parse(&read(&path)?);
                if blocks.remove(address) {
                    write(&path, &blocks.render())?;
                }
                let path = etc.join("bthidd.conf");
                let mut blocks = Blocks::parse(&read(&path)?);
                let input = blocks.remove(address);
                if input {
                    write(&path, &blocks.render())?;
                }
                // Its link key: a line that starts with its address.
                let text = read(keys)?;
                let kept: String = text
                    .lines()
                    .filter(|l| !l.to_ascii_lowercase().starts_with(address))
                    .map(|l| format!("{l}\n"))
                    .collect();
                if kept != text {
                    write(keys, &kept)?;
                }
                cmd::run("service", &["hcsecd", "onerestart"])?;
                if input && blocks.devices().next().is_none() {
                    let _ = cmd::run("service", &["bthidd", "onestop"]);
                    let _ = cmd::run("sysrc", &["-x", "bthidd_enable"]);
                } else if input {
                    cmd::run("service", &["bthidd", "onerestart"])?;
                }
                Ok(())
            }
            Self::StandIn(path) => edit_stand_in(path, |s| {
                s.paired.retain(|d| d.address != address);
                s.connected.retain(|a| a != address);
            }),
        }
    }
}

/// A configuration file's text; none yet is empty.
fn read(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(PlatformError::io(path.display().to_string(), e)),
    }
}

/// Replaces `path` whole, root's alone: it holds PINs and link keys.
fn write(path: &Path, text: &str) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let next = path.with_extension("glass-new");
    let io = |e| PlatformError::io(path.display().to_string(), e);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&next)
        .map_err(io)?;
    file.write_all(text.as_bytes()).map_err(io)?;
    file.sync_all().map_err(io)?;
    std::fs::rename(&next, path).map_err(io)
}

fn read_stand_in(path: &Path) -> Result<StandIn> {
    let text = read(path)?;
    if text.trim().is_empty() {
        return Ok(StandIn {
            on: true,
            ..StandIn::default()
        });
    }
    serde_json::from_str(&text).map_err(|e| {
        PlatformError::io(
            path.display().to_string(),
            std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        )
    })
}

fn edit_stand_in(path: &Path, edit: impl FnOnce(&mut StandIn)) -> Result<()> {
    let mut state = read_stand_in(path)?;
    edit(&mut state);
    let text = serde_json::to_string_pretty(&state).unwrap_or_default();
    write(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handbook's inquiry, and a keyboard's.
    const INQUIRY: &str = "Inquiry result, num_responses=2
Inquiry result #0
\tBD_ADDR: 00:80:37:29:19:A4
\tPage Scan Rep. Mode: 0x1
\tPage Scan Period Mode: 00
\tPage Scan Mode: 00
\tClass: 52:02:04
\tClock offset: 0x78ef
Inquiry result #1
\tBD_ADDR: 00:1f:20:3a:4b:5c
\tPage Scan Rep. Mode: 0x1
\tPage Scan Period Mode: 00
\tPage Scan Mode: 00
\tClass: 00:25:40
\tClock offset: 0x1a2b
Inquiry complete. Status: No error [00]
";

    const NGCTL: &str = "There are 9 total nodes:
  Name: ubt0            Type: ubt             ID: 00000001   Num hooks: 1
  Name: ubt0hci         Type: hci             ID: 00000002   Num hooks: 3
  Name: ubt0l2cap       Type: l2cap           ID: 00000003   Num hooks: 3
  Name: ngctl1234       Type: socket          ID: 00000009   Num hooks: 0
";

    const QUERY: &str = "device {
\tbdaddr\t\t\t00:1f:20:3a:4b:5c;
\tcontrol_psm\t\t0x11;
\tinterrupt_psm\t\t0x13;
\treconnect_initiate\ttrue;
\tnormally_connectable\tfalse;
\thid_descriptor\t\t{
\t\t0x05 0x01 0x09 0x06 0xa1 0x01 0x85 0x01
\t};
}
";

    #[test]
    fn cf13_the_stack_s_tools_are_read() {
        assert_eq!(parse_controllers(NGCTL), ["ubt0"]);
        assert!(parse_on(NGCTL, "ubt0"));
        assert!(!parse_on("  Name: ubt0  Type: ubt  ID: 1\n", "ubt0"));
        assert_eq!(
            parse_inquiry(INQUIRY),
            [
                ("00:80:37:29:19:a4".to_owned(), 0x520204),
                ("00:1f:20:3a:4b:5c".to_owned(), 0x002540),
            ]
        );
        assert_eq!(
            parse_remote_name("BD_ADDR: 00:80:37:29:19:a4\nName: Pav's T39\n").as_deref(),
            Some("Pav's T39")
        );
        assert_eq!(
            parse_connections(
                "Remote BD_ADDR    Handle Type Mode Role Encrypt Pending Queue State\n\
                 00:1F:20:3A:4B:5C     41  ACL    0 MAST    NONE       0     0 OPEN\n\
                 00:80:37:29:19:a4     42  ACL    0 MAST    NONE       0     0 W4_CONN_COMPLETE\n"
            ),
            ["00:1f:20:3a:4b:5c"]
        );
    }

    #[test]
    fn cf13_a_device_s_class_says_what_it_is() {
        assert_eq!(kind(0x520204), Kind::Phone);
        assert_eq!(kind(0x002540), Kind::Keyboard);
        assert_eq!(kind(0x002580), Kind::Mouse);
        assert_eq!(kind(0x0025c0), Kind::KeyboardAndMouse);
        assert_eq!(kind(0x002508), Kind::GameController);
        assert_eq!(kind(0x240404), Kind::Audio);
        assert_eq!(kind(0x5a020c), Kind::Phone);
        assert_eq!(kind(0x3a010c), Kind::Computer);
        assert!(kind(0x002540).input() && !kind(0x240404).input());
        assert!(valid_address("00:1f:20:3a:4b:5c"));
        assert!(!valid_address("00:1f:20:3a:4b"));
        assert!(!valid_address("00:1f:20:3a:4b:zz"));
        assert!(!valid_address("00:1f:20:3a:4b:5c;\n}"));
    }

    #[test]
    fn cf13_pairing_files_keep_what_glassd_did_not_write() {
        let theirs = "# hcsecd.conf: IT's own\ndevice {\n\tbdaddr\t00:11:22:33:44:55;\n\tname\t\"Lab printer\";\n\tkey\tnokey;\n\tpin\t\"0000\";\n}\n";
        let keyboard = Device {
            address: "00:1f:20:3a:4b:5c".into(),
            name: "K380 \"Office\";\n}".into(),
            class: 0x002540,
        };
        let mut blocks = Blocks::parse(theirs);
        assert_eq!(blocks.render(), theirs, "read and written back as it was");
        blocks.push(&keyboard.address, &hcsecd_block(&keyboard, "482913"));
        let text = blocks.render();
        assert!(text.starts_with(theirs), "{text}");
        assert!(
            text.contains("name\t\"K380 Office\";"),
            "a name can't break out: {text}"
        );
        assert!(text.contains("pin\t\"482913\";"));
        // Only glassd's blocks are listed as paired from here.
        assert_eq!(
            parse_paired(&text),
            [Device {
                name: "K380 Office".into(),
                ..keyboard.clone()
            }]
        );
        let mut again = Blocks::parse(&text);
        assert!(again.remove(&keyboard.address));
        assert_eq!(again.render().trim_end(), theirs.trim_end());

        // bthidcontrol's block, marked as glassd's, nested braces and all.
        let block = bthidd_block(QUERY, &keyboard).unwrap();
        assert!(
            block.starts_with("device {\n\t# glasshouse class 0x002540"),
            "{block}"
        );
        assert!(block.trim_end().ends_with('}'));
        let parsed = Blocks::parse(&format!("{theirs}{block}\n"));
        assert_eq!(
            parsed.devices().map(|(a, _)| a).collect::<Vec<_>>(),
            ["00:11:22:33:44:55", "00:1f:20:3a:4b:5c"]
        );
    }

    #[test]
    fn cf13_the_stand_in_pairs_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bluetooth.json");
        let keyboard = Device {
            address: "00:1f:20:3a:4b:5c".into(),
            name: "Keyboard K380".into(),
            class: 0x002540,
        };
        std::fs::write(
            &path,
            serde_json::to_string(&StandIn {
                on: true,
                nearby: vec![keyboard.clone()],
                ..StandIn::default()
            })
            .unwrap(),
        )
        .unwrap();
        let c = Controller::stand_in(&path);
        assert_eq!(c.inquiry().unwrap(), std::slice::from_ref(&keyboard));
        c.pair(&keyboard, "482913").unwrap();
        assert_eq!(c.paired().unwrap(), std::slice::from_ref(&keyboard));
        assert_eq!(
            c.connected().unwrap(),
            std::slice::from_ref(&keyboard.address)
        );
        assert!(
            c.pair(
                &Device {
                    address: "x".into(),
                    ..keyboard.clone()
                },
                "1"
            )
            .is_err()
        );
        c.set_on(false).unwrap();
        assert!(!c.on().unwrap());
        c.remove(&keyboard.address).unwrap();
        assert!(c.paired().unwrap().is_empty());
    }
}
