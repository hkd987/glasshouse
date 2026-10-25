//! What this computer's parts are and whether this OS drives them: the
//! report `glassctl hardware` prints, to check a laptop before it joins a
//! pilot and to build the certified list (DK-11, DK-12, PM-10). Read
//! through the base system's tools (ADR 0007); nothing here changes
//! anything.
//!
//! [`assess`] decides from the tools' output alone, so the tests run it on
//! output captured from the dev VM and the emulated devices
//! `tests/hardware/matrix.sh` boots (`testdata/hardware`), and on laptops
//! as their tools describe them.

use serde::{Deserialize, Serialize};

use crate::{PlatformError, Result, cmd};

/// A computer's parts, and what this OS makes of each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub maker: String,
    pub model: String,
    pub processor: String,
    pub memory_mb: u64,
    pub parts: Vec<Part>,
    /// False when USB devices and the battery couldn't be read (they need
    /// root): the parts that depend on them read as unknown.
    pub complete: bool,
    /// What a kernel patch for this computer names (ADR 0057): each PCI
    /// function, `pci:8086:2725`, and `model:<maker> <product>`.
    #[serde(default)]
    pub targets: Vec<String>,
}

impl Report {
    /// Whether a patch for `targets` is for this computer: one of them is
    /// a part of it.
    pub fn fits(&self, targets: &[String]) -> bool {
        targets.iter().any(|t| self.targets.contains(t))
    }
}

impl Report {
    /// The parts this OS can't drive: what keeps the computer off the
    /// certified list.
    pub fn blockers(&self) -> impl Iterator<Item = &Part> {
        self.parts
            .iter()
            .filter(|p| p.status == Status::Unsupported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub kind: Kind,
    pub status: Status,
    /// What was found, as the tools name it, with its driver.
    pub devices: Vec<String>,
    /// What the status means on this computer.
    pub note: String,
    /// What the OS doesn't do with it yet, whatever the computer: a
    /// requirement not built, as a phrase ("802.1X on wired networks").
    pub gap: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Graphics,
    Wifi,
    Ethernet,
    Bluetooth,
    Camera,
    Audio,
    Pointer,
    Sleep,
    Battery,
    Tpm,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Graphics => "Graphics",
            Kind::Wifi => "Wi-Fi",
            Kind::Ethernet => "Ethernet",
            Kind::Bluetooth => "Bluetooth",
            Kind::Camera => "Camera",
            Kind::Audio => "Sound",
            Kind::Pointer => "Touchpad",
            Kind::Sleep => "Sleep",
            Kind::Battery => "Battery",
            Kind::Tpm => "Security chip",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// There, and this OS drives it.
    Works,
    /// There and driven, short of what the PRD asks; the note says how.
    Limited,
    /// There, but this OS can't drive it.
    Unsupported,
    /// Not on this computer.
    Absent,
    /// Not read: it needs root.
    Unknown,
}

/// The tools' output [`assess`] decides from.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// `kenv`: the firmware's SMBIOS strings among the rest.
    pub kenv: String,
    /// `sysctl -n hw.model hw.physmem`.
    pub sysctl: String,
    /// `pciconf -lv`.
    pub pciconf: String,
    /// `devinfo -v`: every device, with its driver or `unknown`.
    pub devinfo: String,
    /// `usbconfig dump_curr_config_desc`; None without root.
    pub usb: Option<String>,
    /// `sysctl -n net.wlan.devices`: radios a Wi-Fi driver attached.
    pub wlan: String,
    /// `/dev/sndstat`.
    pub sndstat: String,
    /// `sysctl -n hw.acpi.supported_sleep_state`.
    pub sleep: String,
    /// `acpiconf -i 0`, empty when there's no battery; None without root.
    pub battery: Option<String>,
    /// Whether a graphics driver is loaded (`hw.dri.0` exists).
    pub graphics_driver: bool,
    /// The names in `/dev`.
    pub dev: Vec<String>,
    /// The kernel's messages: `/var/run/dmesg.boot`, then `dmesg`.
    pub dmesg: String,
    /// Whether Intel's Bluetooth firmware is installed.
    pub intel_bt_firmware: bool,
    /// Whether the image carries Intel's graphics driver (`i915kms.ko`),
    /// which is off until a model is checked with it (ADR 0058).
    pub intel_gpu_driver: bool,
    /// Whether the image carries the USB camera driver (`webcamd`).
    pub camera_driver: bool,
    /// `sysctl -n hw.acpi.video.lcd0.levels`: the firmware's brightness
    /// levels, if it has any (ADR 0060).
    pub brightness_levels: String,
}

impl Sources {
    /// What `tests/hardware/capture.sh` saved, one file per tool: a
    /// computer captured on its own base system, judged here.
    pub fn read_dir(dir: &std::path::Path) -> std::io::Result<Sources> {
        let optional = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        let required = |name: &str| {
            std::fs::read_to_string(dir.join(name)).map_err(|e| {
                std::io::Error::new(e.kind(), format!("{}: {e}", dir.join(name).display()))
            })
        };
        Ok(Sources {
            kenv: optional("kenv").unwrap_or_default(),
            sysctl: required("sysctl")?,
            pciconf: required("pciconf")?,
            devinfo: required("devinfo")?,
            usb: optional("usbconfig"),
            wlan: optional("wlan").unwrap_or_default(),
            sndstat: optional("sndstat").unwrap_or_default(),
            sleep: optional("sleep").unwrap_or_default(),
            battery: optional("battery"),
            graphics_driver: optional("dri").is_some_and(|d| !d.trim().is_empty()),
            dev: optional("dev")
                .map(|d| d.lines().map(str::to_owned).collect())
                .unwrap_or_default(),
            dmesg: optional("dmesg").unwrap_or_default(),
            intel_bt_firmware: dir.join("iwmbt-firmware").exists(),
            // A capture comes from the base system's own stick: what this
            // image adds is judged as this image carries it.
            intel_gpu_driver: true,
            camera_driver: true,
            brightness_levels: optional("backlight").unwrap_or_default(),
        })
    }
}

/// Reads this computer's parts. As root for all of them.
pub fn report() -> Result<Report> {
    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("a hardware report".into()));
    }
    let root = cmd::run("id", &["-u"]).is_ok_and(|uid| uid.trim() == "0");
    let read = |program: &str, args: &[&str]| cmd::run(program, args).unwrap_or_default();
    let mut dev: Vec<String> = std::fs::read_dir("/dev")
        .map(|dir| {
            dir.filter_map(|e| e.ok()?.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    dev.sort();
    let sources = Sources {
        kenv: read("kenv", &[]),
        sysctl: cmd::run("sysctl", &["-n", "hw.model", "hw.physmem"])?,
        pciconf: read("pciconf", &["-lv"]),
        devinfo: read("devinfo", &["-v"]),
        usb: root.then(|| read("usbconfig", &["dump_curr_config_desc"])),
        wlan: read("sysctl", &["-n", "net.wlan.devices"]),
        sndstat: std::fs::read_to_string("/dev/sndstat").unwrap_or_default(),
        sleep: read("sysctl", &["-n", "hw.acpi.supported_sleep_state"]),
        battery: root.then(|| read("acpiconf", &["-i", "0"])),
        graphics_driver: cmd::run("sysctl", &["-n", "hw.dri.0.name"]).is_ok(),
        dev,
        dmesg: std::fs::read_to_string("/var/run/dmesg.boot").unwrap_or_default()
            + &read("dmesg", &[]),
        intel_bt_firmware: std::path::Path::new("/usr/local/share/iwmbt-firmware").is_dir(),
        intel_gpu_driver: std::path::Path::new("/boot/modules/i915kms.ko").exists(),
        camera_driver: std::path::Path::new("/usr/local/sbin/webcamd").exists(),
        brightness_levels: read("sysctl", &["-n", "hw.acpi.video.lcd0.levels"]),
    };
    Ok(assess(&sources))
}

/// The release the running kernel was built from: `15.1-RELEASE`, or
/// `15.1-RELEASE-p2` with a patch level.
pub fn kernel_release() -> Result<String> {
    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("the kernel's release".into()));
    }
    Ok(cmd::run("sysctl", &["-n", "kern.osrelease"])?
        .trim()
        .to_owned())
}

/// Whether modules built for `built_for` (`15.1-RELEASE`) run on a kernel
/// of release `running`: the same release, at any patch level, which keeps
/// the kernel's interface for modules.
pub fn release_takes(running: &str, built_for: &str) -> bool {
    let base = match running.rsplit_once("-p") {
        Some((base, level)) if !level.is_empty() && level.bytes().all(|b| b.is_ascii_digit()) => {
            base
        }
        _ => running,
    };
    base == built_for
}

/// Whether kernel module `name` (`if_iwlwifi`) is loaded.
pub fn module_loaded(name: &str) -> bool {
    cfg!(target_os = "freebsd")
        && crate::hardware::module_name(name)
        && cmd::run("kldstat", &["-q", "-n", name]).is_ok()
}

/// Whether the running kernel has driver `name` built in: one of its own
/// modules (`kldstat -v -i 1`) is called that, as `pci/snd_hda` is in
/// GENERIC. A module file of that name never loads beside it (`kldload -n`
/// says it's loaded and does nothing), so a patch to it rebuilds the kernel.
pub fn built_in(name: &str) -> bool {
    cfg!(target_os = "freebsd")
        && module_name(name)
        && cmd::run("kldstat", &["-v", "-i", "1"]).is_ok_and(|text| kernel_has(&text, name))
}

fn kernel_has(kldstat: &str, name: &str) -> bool {
    kldstat
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            fields.next()?.parse::<u32>().ok()?;
            let module = fields.next()?;
            fields.next().is_none().then_some(module)
        })
        .any(|module| module.rsplit('/').next() == Some(name))
}

/// Loads kernel module `name` with what it depends on, unless it's loaded:
/// proof it links against this kernel.
pub fn load_module(name: &str) -> Result<()> {
    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("loading kernel modules".into()));
    }
    if !module_name(name) {
        return Err(PlatformError::Unsupported(format!(
            "a module named {name:?}"
        )));
    }
    cmd::run("kldload", &["-n", name]).map(|_| ())
}

/// Whether this computer sleeps: its firmware offers S3, which the base
/// system's suspend uses (ADR 0056).
pub fn sleeps() -> bool {
    cmd::run("sysctl", &["-n", "hw.acpi.supported_sleep_state"])
        .is_ok_and(|states| states.split_whitespace().any(|s| s == "S3"))
}

/// How long the hardware watchdog waits for `watchdogd` before it restarts
/// the computer, in seconds; Intel's (ichwd) waits it twice over.
pub const WATCHDOG_SECS: u32 = 60;
/// How long it waits once `watchdogd` has gone, as the computer shuts
/// down: a shutdown that hangs restarts too, about two minutes on (Intel
/// waits twice over), sooner than most people hold the power button. One
/// that finishes turns the watchdog off with the rest (ichwd halts it as it
/// shuts down); the rest takes seconds.
pub const WATCHDOG_SHUTDOWN_SECS: u32 = 60;

/// Arms the hardware watchdog with the base system's `watchdogd`, or
/// disarms it (ADR 0057): while armed, a computer that hangs restarts by
/// itself, through a shutdown too. Whether the computer has one
/// (`/dev/fido`). A `watchdogd` the computer's own rc.conf runs is left as
/// it is.
pub fn watchdog(on: bool) -> Result<bool> {
    if !std::path::Path::new("/dev/fido").exists() {
        return Ok(false);
    }
    let enabled = cmd::run("sysrc", &["-n", "watchdogd_enable"])
        .is_ok_and(|v| v.trim().eq_ignore_ascii_case("yes"));
    if enabled {
        return Ok(true);
    }
    let running = cmd::run("service", &["watchdogd", "onestatus"]).is_ok();
    match (on, running) {
        (true, false) => {
            cmd::run("watchdogd", &["-t", &WATCHDOG_SECS.to_string()])?;
        }
        // 15.1's turns it off as it exits; off whatever another leaves.
        (false, true) => {
            cmd::run("service", &["watchdogd", "onestop"])?;
            cmd::run("watchdog", &["-t", "0"])?;
        }
        _ => {}
    }
    Ok(true)
}

/// Keeps the watchdog [`watchdog`] armed through the shutdown (ADR 0057):
/// as the computer stops every process, `watchdogd` turns the watchdog off
/// as it exits (15.1's ignores `-x`, the timeout to leave it with). So it
/// stops first, and the watchdog is left with [`WATCHDOG_SHUTDOWN_SECS`]
/// of its own (`watchdog -t`, which sets it and exits). Only as the
/// computer shuts down: nothing pats it after. Nothing when it isn't
/// armed, or the computer's own rc.conf runs `watchdogd`.
pub fn watchdog_through_shutdown() -> Result<bool> {
    if !std::path::Path::new("/dev/fido").exists()
        || cmd::run("sysrc", &["-n", "watchdogd_enable"])
            .is_ok_and(|v| v.trim().eq_ignore_ascii_case("yes"))
        || cmd::run("service", &["watchdogd", "onestatus"]).is_err()
    {
        return Ok(false);
    }
    cmd::run("service", &["watchdogd", "onestop"])?;
    cmd::run("watchdog", &["-t", &WATCHDOG_SHUTDOWN_SECS.to_string()])?;
    Ok(true)
}

/// Whether this start came from a hardware watchdog's restart: the
/// computer hung (ichwd says so as it attaches). Only this start's
/// messages: the kernel's buffer can keep the last one's.
pub fn watchdog_restarted() -> bool {
    cmd::run("dmesg", &[]).is_ok_and(|text| restarted_by_watchdog(&text))
}

fn restarted_by_watchdog(dmesg: &str) -> bool {
    let this_start = dmesg.rsplit("---<<BOOT>>---").next().unwrap_or(dmesg);
    this_start.contains("resuming after hardware watchdog timeout")
}

/// The kernel's last `count` messages naming `name`, or about loading
/// modules (`link_elf`, `KLD`): why a module didn't load.
pub fn kernel_messages(name: &str, count: usize) -> Vec<String> {
    let Ok(text) = cmd::run("dmesg", &[]) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| l.contains(name) || l.contains("link_elf") || l.contains("KLD"))
        .map(|l| l.trim().to_owned())
        .collect();
    let skip = lines.len().saturating_sub(count);
    lines.drain(..skip);
    lines
}

fn module_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// What the tools' output says about each part.
pub fn assess(s: &Sources) -> Report {
    let pci = parse_pciconf(&s.pciconf);
    let devices = parse_devinfo(&s.devinfo);
    let usb = s.usb.as_deref().map(parse_usb);
    let mut lines = s.sysctl.lines().map(str::trim);
    let processor = lines.next().unwrap_or_default().to_owned();
    let memory_mb = lines
        .next()
        .and_then(|m| m.parse::<u64>().ok())
        .unwrap_or(0)
        / 1024
        / 1024;
    let parts = vec![
        graphics(&pci, s),
        wifi(&pci, s),
        ethernet(&pci),
        bluetooth(usb.as_deref(), &devices, s.intel_bt_firmware),
        camera(&pci, usb.as_deref(), &s.dev, s.camera_driver),
        audio(&pci, &s.sndstat),
        pointer(&devices),
        sleep(&s.sleep),
        battery(s.battery.as_deref()),
        tpm(&devices, &s.dev),
    ];
    let maker = kenv(&s.kenv, "smbios.system.maker");
    let model = kenv(&s.kenv, "smbios.system.product");
    let mut targets: Vec<String> = pci
        .iter()
        .map(|d| format!("pci:{:04x}:{:04x}", d.vendor_id, d.device_id))
        .collect();
    if !maker.is_empty() || !model.is_empty() {
        targets.push(format!("model:{maker} {model}").trim_end().to_owned());
    }
    targets.sort();
    targets.dedup();
    Report {
        maker,
        model,
        processor,
        memory_mb,
        parts,
        complete: s.usb.is_some() && s.battery.is_some(),
        targets,
    }
}

fn part(kind: Kind, status: Status, devices: Vec<String>, note: impl Into<String>) -> Part {
    Part {
        kind,
        status,
        devices,
        note: note.into(),
        gap: None,
    }
}

fn with_gap(mut part: Part, gap: &str) -> Part {
    part.gap = Some(gap.to_owned());
    part
}

fn graphics(pci: &[PciDevice], s: &Sources) -> Part {
    let gpus: Vec<&PciDevice> = pci.iter().filter(|d| d.class >> 16 == 0x03).collect();
    let found: Vec<String> = gpus.iter().map(|d| d.describe()).collect();
    let gap = "more screens, docks and scaling; the desktop draws on one screen (CF-32)";
    if found.is_empty() {
        return part(Kind::Graphics, Status::Absent, found, "No graphics found");
    }
    let intel = gpus.iter().any(|d| d.vendor_id == 0x8086);
    let note = if s.graphics_driver {
        "Its graphics driver is loaded"
    } else if intel && s.intel_gpu_driver {
        "Intel's graphics driver is in this image, off until this model is checked with it \
         (glasshouse_gpu_enable): the screen the firmware set up, at a fixed resolution"
    } else {
        "No driver for these graphics in this image: the screen the firmware set up, at a \
         fixed resolution"
    };
    // The screen's brightness (ADR 0060): the graphics driver's backlight,
    // else the firmware's levels.
    let brightness = if s.dev.iter().any(|d| d == "backlight") {
        "brightness set from Settings and its keys (the graphics driver's backlight)"
    } else if !s.brightness_levels.trim().is_empty() {
        "brightness set from Settings and its keys (the firmware's levels)"
    } else {
        "brightness not set from here; the firmware's own keys may"
    };
    let note = format!("{note}; {brightness}");
    with_gap(part(Kind::Graphics, Status::Limited, found, &note), gap)
}

fn wifi(pci: &[PciDevice], s: &Sources) -> Part {
    let radios: Vec<&str> = s.wlan.split_whitespace().collect();
    let gap =
        "networks with a sign-in of each person's own (802.1X); Settings joins the rest (CF-10)";
    let mut found = Vec::new();
    let (mut working, mut broken) = (false, Vec::new());
    for d in pci.iter().filter(|d| d.class >> 8 == 0x0280) {
        found.push(d.describe());
        if !d.attached() {
            broken.push(format!("no driver for {}", d.name()));
        } else if let Some(line) = firmware_failure(&s.dmesg, &d.driver) {
            broken.push(format!(
                "{}'s firmware isn't in this image ({line})",
                d.driver
            ));
        } else if radios.contains(&d.driver.as_str()) {
            working = true;
        } else {
            broken.push(format!("{} attached but has no radio", d.driver));
        }
    }
    // Radios on USB: a driver attached, so they're in net.wlan.devices.
    for radio in radios
        .iter()
        .filter(|r| !pci.iter().any(|d| d.driver == **r))
    {
        found.push((*radio).to_owned());
        match firmware_failure(&s.dmesg, radio) {
            Some(line) => broken.push(format!("{radio}'s firmware isn't in this image ({line})")),
            None => working = true,
        }
    }
    let (status, note) = match (found.is_empty(), working) {
        (true, _) => (Status::Absent, "No Wi-Fi found".to_owned()),
        (false, true) => (
            Status::Works,
            "The radio's driver and firmware are loaded".into(),
        ),
        (false, false) => (Status::Unsupported, broken.join("; ")),
    };
    let part = part(Kind::Wifi, status, found, note);
    if status == Status::Absent {
        part
    } else {
        with_gap(part, gap)
    }
}

/// The kernel's line saying `driver` couldn't load its firmware.
fn firmware_failure(dmesg: &str, driver: &str) -> Option<String> {
    let prefix = format!("{driver}: ");
    dmesg
        .lines()
        .filter_map(|l| l.strip_prefix(&prefix))
        .find(|rest| {
            let rest = rest.to_ascii_lowercase();
            rest.contains("firmware")
                && ["fail", "error", "no suitable", "not found", "could not"]
                    .iter()
                    .any(|w| rest.contains(w))
        })
        .map(|rest| rest.trim().to_owned())
}

fn ethernet(pci: &[PciDevice]) -> Part {
    let wired: Vec<&PciDevice> = pci.iter().filter(|d| d.class >> 8 == 0x0200).collect();
    let found = wired.iter().map(|d| d.describe()).collect();
    let gap = "802.1X on wired networks (CF-10)";
    match (wired.is_empty(), wired.iter().any(|d| d.attached())) {
        (true, _) => part(
            Kind::Ethernet,
            Status::Absent,
            found,
            "No Ethernet port found",
        ),
        (false, true) => with_gap(
            part(Kind::Ethernet, Status::Works, found, "Its driver is loaded"),
            gap,
        ),
        (false, false) => part(
            Kind::Ethernet,
            Status::Unsupported,
            found,
            "No driver for it",
        ),
    }
}

fn bluetooth(usb: Option<&[UsbDevice]>, devices: &[Device], intel_firmware: bool) -> Part {
    let Some(usb) = usb else {
        return part(
            Kind::Bluetooth,
            Status::Unknown,
            vec![],
            "Needs root to read USB",
        );
    };
    let radios: Vec<&UsbDevice> = usb
        .iter()
        .filter(|d| d.interfaces.contains(&(0xe0, 0x01, 0x01)))
        .collect();
    let found: Vec<String> = radios.iter().map(|d| d.name.clone()).collect();
    if radios.is_empty() {
        return part(Kind::Bluetooth, Status::Absent, found, "No Bluetooth found");
    }
    let attached = devices.iter().any(|d| d.driver() == "ubt");
    let intel = radios
        .iter()
        .any(|d| d.name.to_ascii_lowercase().contains("intel"));
    let (status, note) = if !attached {
        (Status::Unsupported, "No driver for it")
    } else if intel && !intel_firmware {
        (
            Status::Unsupported,
            "Intel's Bluetooth needs its firmware loaded, which isn't in this image",
        )
    } else {
        (Status::Works, "Its driver is loaded")
    };
    with_gap(
        part(Kind::Bluetooth, status, found, note),
        "headsets and Bluetooth LE, which this OS version lacks (CF-13)",
    )
}

/// Intel's image processors, which drive the cameras of most of its laptops
/// since 2021 instead of USB (the ids Linux's ipu6 and ipu7 drivers take).
const INTEL_IMAGE_PROCESSORS: [u16; 9] = [
    0x9a19, 0x9a39, 0x4e19, 0x465d, 0x462e, 0xa75d, 0x7d19, 0x645d, 0xb05d,
];

fn camera(pci: &[PciDevice], usb: Option<&[UsbDevice]>, dev: &[String], driver: bool) -> Part {
    let gap = "a camera driver in the image, for meetings in the browser (DK-11, CF-20)";
    let processors: Vec<String> = pci
        .iter()
        .filter(|d| {
            d.vendor_id == 0x8086
                && (INTEL_IMAGE_PROCESSORS.contains(&d.device_id)
                    || (d.class >> 8 == 0x0480 && d.name().to_ascii_lowercase().contains("imag")))
        })
        .map(PciDevice::describe)
        .collect();
    let usb_cameras: Vec<String> = usb
        .unwrap_or_default()
        .iter()
        .filter(|d| d.interfaces.iter().any(|i| i.0 == 0x0e))
        .map(|d| d.name.clone())
        .collect();
    let video = dev.iter().any(|n| n.starts_with("video"));
    let mut found = processors.clone();
    found.extend(usb_cameras.iter().cloned());
    if !processors.is_empty() && usb_cameras.is_empty() {
        return with_gap(
            part(
                Kind::Camera,
                Status::Unsupported,
                found,
                "A camera behind Intel's image processor (IPU6 or later): no driver for it on \
                 this OS",
            ),
            gap,
        );
    }
    if usb_cameras.is_empty() {
        return if usb.is_none() {
            part(
                Kind::Camera,
                Status::Unknown,
                found,
                "Needs root to read USB",
            )
        } else {
            part(Kind::Camera, Status::Absent, found, "No camera found")
        };
    }
    if video {
        part(
            Kind::Camera,
            Status::Works,
            found,
            "A USB camera, with its driver running",
        )
    } else if driver {
        part(
            Kind::Camera,
            Status::Limited,
            found,
            "A USB camera (UVC): its driver (webcamd) is in this image but isn't running for it",
        )
    } else {
        with_gap(
            part(
                Kind::Camera,
                Status::Unsupported,
                found,
                "A USB camera (UVC), which a camera driver would drive; this image has none",
            ),
            gap,
        )
    }
}

fn audio(pci: &[PciDevice], sndstat: &str) -> Part {
    let controllers: Vec<&PciDevice> = pci
        .iter()
        .filter(|d| d.class >> 8 == 0x0401 || d.class >> 8 == 0x0403)
        .collect();
    let mut found: Vec<String> = controllers.iter().map(|d| d.describe()).collect();
    let (mut play, mut record) = (false, false);
    for line in sndstat.lines().filter(|l| l.starts_with("pcm")) {
        let name = line
            .split_once('<')
            .and_then(|(_, rest)| rest.split_once('>'))
            .map_or(line, |(name, _)| name);
        found.push(name.to_owned());
        let (p, r) = channels(line);
        play |= p;
        record |= r;
    }
    let unattached: Vec<&&PciDevice> = controllers.iter().filter(|d| !d.attached()).collect();
    // Intel's audio DSP (Smart Sound, SOF) shows as an audio device rather
    // than HD Audio when the firmware turns it on.
    let dsp = unattached
        .iter()
        .any(|d| d.vendor_id == 0x8086 && d.class >> 8 == 0x0401);
    let (status, note) = match (play, record) {
        (true, true) => (Status::Works, "Speakers and a microphone".to_owned()),
        (true, false) => (
            Status::Limited,
            "Sound plays, but no microphone was found: a meeting needs one (CF-21)".to_owned(),
        ),
        (false, _) if !unattached.is_empty() => (
            Status::Unsupported,
            format!(
                "No driver for {}{}",
                unattached
                    .iter()
                    .map(|d| d.name())
                    .collect::<Vec<_>>()
                    .join(", "),
                if dsp {
                    ": Intel's audio DSP has none on this OS"
                } else {
                    ""
                }
            ),
        ),
        (false, _) if controllers.is_empty() => (Status::Absent, "No sound found".to_owned()),
        (false, _) => (
            Status::Unsupported,
            "A sound controller with nothing to play through".to_owned(),
        ),
    };
    part(Kind::Audio, status, found, note)
}

/// Whether a `/dev/sndstat` line's device plays and records: `(play/rec)`,
/// or the counts `(1p:1v/1r:1v)` in older and verbose forms.
fn channels(line: &str) -> (bool, bool) {
    let Some(spec) = line
        .rsplit_once('(')
        .map(|(_, s)| s.split(')').next().unwrap_or(s))
    else {
        return (false, false);
    };
    let count = |suffix: char| {
        spec.split(['/', ' '])
            .filter_map(|p| p.split(':').next()?.strip_suffix(suffix))
            .any(|n| n.parse::<u32>().is_ok_and(|n| n > 0))
    };
    (
        spec.contains("play") || count('p'),
        spec.contains("rec") || count('r'),
    )
}

/// Drivers of touchpads and touchscreens (HID multitouch over USB or I²C,
/// Elan's and Apple's).
const TOUCHPAD_DRIVERS: [&str; 5] = ["hmt", "ietp", "wsp", "atp", "bcm5974"];
/// Drivers of mice, and of touchpads that act as one (PS/2).
const MOUSE_DRIVERS: [&str; 3] = ["hms", "ums", "psm"];

fn pointer(devices: &[Device]) -> Part {
    let pointers: Vec<&Device> = devices
        .iter()
        .filter(|d| TOUCHPAD_DRIVERS.contains(&d.driver()) || MOUSE_DRIVERS.contains(&d.driver()))
        .collect();
    let found: Vec<String> = pointers.iter().map(|d| d.describe()).collect();
    // HID over I²C (PNP0C50): how most laptops since about 2018 connect
    // their touchpad and touchscreen.
    let unattached = devices
        .iter()
        .filter(|d| d.name == "unknown" && d.pnp_says("PNP0C50"))
        .count();
    if unattached > 0 {
        return part(
            Kind::Pointer,
            Status::Unsupported,
            found,
            format!(
                "{unattached} touchpad or touchscreen on I²C with no driver (iichid and ig4 \
                 load it)"
            ),
        );
    }
    let touchpad = pointers
        .iter()
        .any(|d| TOUCHPAD_DRIVERS.contains(&d.driver()));
    match (found.is_empty(), touchpad) {
        (true, _) => part(Kind::Pointer, Status::Absent, found, "No touchpad or mouse"),
        (false, true) => part(Kind::Pointer, Status::Works, found, "A touchpad, driven"),
        (false, false) => part(
            Kind::Pointer,
            Status::Works,
            found,
            "A mouse or a PS/2 touchpad, driven",
        ),
    }
}

fn sleep(states: &str) -> Part {
    let states: Vec<&str> = states.split_whitespace().collect();
    let gap = "modern standby (S0ix), which PM-1 asks for and this OS version lacks";
    if states.is_empty() {
        return part(
            Kind::Sleep,
            Status::Unknown,
            vec![],
            "The firmware names no sleep states",
        );
    }
    let found = vec![states.join(" ")];
    if states.contains(&"S3") {
        with_gap(
            part(
                Kind::Sleep,
                Status::Works,
                found,
                "Sleeps (S3) when the lid closes, agents paused first (PM-5)",
            ),
            gap,
        )
    } else {
        with_gap(
            part(
                Kind::Sleep,
                Status::Unsupported,
                found,
                "No S3: the firmware offers only modern standby, so the computer can't sleep",
            ),
            gap,
        )
    }
}

fn battery(info: Option<&str>) -> Part {
    let Some(info) = info else {
        return part(Kind::Battery, Status::Unknown, vec![], "Needs root");
    };
    let field = |key: &str| {
        info.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .map(str::trim)
    };
    let number =
        |key: &str| field(key).and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok());
    let (Some(design), Some(full)) = (number("Design capacity"), number("Last full capacity"))
    else {
        return part(Kind::Battery, Status::Absent, vec![], "No battery");
    };
    let health = (full * 100).checked_div(design).unwrap_or(0);
    let mut note = format!("Holds {health}% of what it was made to");
    if let Some(cycles) = number("Cycle Count") {
        note += &format!(", {cycles} cycles");
    }
    if let (Some(charge), Some(state)) = (field("Remaining capacity"), field("State")) {
        note += &format!("; {charge}, {state}");
    }
    let status = if health >= 80 {
        Status::Works
    } else {
        note += ": worn, battery life will fall short (PM-6)";
        Status::Limited
    };
    part(Kind::Battery, status, vec![format!("{design} mWh")], note)
}

/// The TPM drivers in `tpm.ko`: a TPM 2.0 behind either interface.
const TPM_DRIVERS: [&str; 2] = ["tpmcrb", "tpmtis"];

fn tpm(devices: &[Device], dev: &[String]) -> Part {
    let gap = "disk encryption and keys in the TPM; nothing uses it yet (PA-6)";
    if dev.iter().any(|n| n == "tpm0") {
        let found = devices
            .iter()
            .filter(|d| TPM_DRIVERS.contains(&d.driver()))
            .map(Device::describe)
            .collect();
        return with_gap(part(Kind::Tpm, Status::Works, found, "A TPM, driven"), gap);
    }
    if devices
        .iter()
        .any(|d| d.name == "unknown" && d.pnp_says("MSFT0101"))
    {
        return with_gap(
            part(
                Kind::Tpm,
                Status::Unsupported,
                vec!["MSFT0101".into()],
                "A TPM 2.0 its driver didn't take (tpm)",
            ),
            gap,
        );
    }
    part(Kind::Tpm, Status::Absent, vec![], "No TPM found")
}

fn kenv(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
        .map(|v| v.trim().trim_matches('"').trim().to_owned())
        .unwrap_or_default()
}

/// A PCI function, as `pciconf -lv` lists it.
#[derive(Debug, Clone, PartialEq)]
struct PciDevice {
    /// The driver and unit that took it (`iwlwifi0`), or `noneN`.
    driver: String,
    /// Class, subclass and interface: `0x028000`.
    class: u32,
    vendor_id: u16,
    device_id: u16,
    vendor: String,
    device: String,
}

impl PciDevice {
    fn attached(&self) -> bool {
        !self.driver.starts_with("none")
    }

    fn name(&self) -> String {
        match (self.vendor.is_empty(), self.device.is_empty()) {
            (false, false) => format!("{} {}", short_vendor(&self.vendor), self.device),
            _ => format!("{:04x}:{:04x}", self.vendor_id, self.device_id),
        }
    }

    fn describe(&self) -> String {
        if self.attached() {
            format!("{} ({})", self.name(), self.driver)
        } else {
            self.name()
        }
    }
}

/// "Intel Corporation" → "Intel", "Red Hat, Inc." → "Red Hat".
fn short_vendor(vendor: &str) -> &str {
    const SUFFIXES: [&str; 8] = [
        " Corporation",
        " Corp.",
        " Inc.",
        " Co.",
        " Ltd.",
        " Semiconductor",
        " Technology",
        " Technologies",
    ];
    let mut short = vendor.split(',').next().unwrap_or(vendor).trim();
    while let Some(rest) = SUFFIXES.iter().find_map(|s| short.strip_suffix(s)) {
        short = rest.trim_end();
    }
    if short.is_empty() { vendor } else { short }
}

fn parse_pciconf(text: &str) -> Vec<PciDevice> {
    let mut out: Vec<PciDevice> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(char::is_whitespace) {
            let Some((driver, rest)) = line.split_once('@') else {
                continue;
            };
            let hex = |key: &str| {
                rest.split_whitespace()
                    .find_map(|f| f.strip_prefix(key)?.strip_prefix("0x"))
                    .and_then(|v| u32::from_str_radix(v, 16).ok())
                    .unwrap_or(0)
            };
            out.push(PciDevice {
                driver: driver.to_owned(),
                class: hex("class="),
                vendor_id: hex("vendor=") as u16,
                device_id: hex("device=") as u16,
                vendor: String::new(),
                device: String::new(),
            });
        } else if let (Some(last), Some((key, value))) = (out.last_mut(), line.split_once('=')) {
            let value = value.trim().trim_matches('\'').to_owned();
            match key.trim() {
                "vendor" => last.vendor = value,
                "device" => last.device = value,
                _ => {}
            }
        }
    }
    out
}

/// A device in `devinfo -v`'s tree.
#[derive(Debug, Clone, PartialEq)]
struct Device {
    /// `hmt0`, or `unknown` when no driver took it.
    name: String,
    description: String,
    pnpinfo: String,
}

impl Device {
    /// The driver's name without its unit: `hmt0` → `hmt`.
    fn driver(&self) -> &str {
        self.name.trim_end_matches(|c: char| c.is_ascii_digit())
    }

    fn pnp_says(&self, id: &str) -> bool {
        self.pnpinfo
            .split_whitespace()
            .any(|f| f == format!("_HID={id}") || f == format!("_CID={id}"))
    }

    fn describe(&self) -> String {
        if self.description.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.description, self.name)
        }
    }
}

fn parse_devinfo(text: &str) -> Vec<Device> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, rest) = line.split_once(' ').unwrap_or((line, ""));
            if name.is_empty() {
                return None;
            }
            let description = rest
                .strip_prefix('<')
                .and_then(|r| r.split_once('>'))
                .map_or("", |(d, _)| d);
            let pnpinfo = rest
                .split_once("pnpinfo ")
                .map_or("", |(_, p)| p.split(" at ").next().unwrap_or(p));
            Some(Device {
                name: name.to_owned(),
                description: description.to_owned(),
                pnpinfo: pnpinfo.to_owned(),
            })
        })
        .collect()
}

/// A USB device and its interfaces' (class, subclass, protocol), from
/// `usbconfig dump_curr_config_desc`.
#[derive(Debug, Clone, PartialEq)]
struct UsbDevice {
    name: String,
    interfaces: Vec<(u16, u16, u16)>,
}

fn parse_usb(text: &str) -> Vec<UsbDevice> {
    let mut out: Vec<UsbDevice> = Vec::new();
    let mut pending: [Option<u16>; 3] = [None; 3];
    for line in text.lines() {
        if line.starts_with("ugen") {
            let name = line
                .split_once('<')
                .and_then(|(_, rest)| rest.split_once('>'))
                .map_or(line, |(name, _)| name);
            out.push(UsbDevice {
                name: name.to_owned(),
                interfaces: Vec::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match key.trim() {
            "bInterfaceClass" => 0,
            "bInterfaceSubClass" => 1,
            "bInterfaceProtocol" => 2,
            _ => continue,
        };
        pending[slot] = value
            .split_whitespace()
            .next()
            .and_then(|v| v.strip_prefix("0x"))
            .and_then(|v| u16::from_str_radix(v, 16).ok());
        if let ([Some(c), Some(s), Some(p)], Some(device)) = (pending, out.last_mut()) {
            device.interfaces.push((c, s, p));
            pending = [None; 3];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(name: &str) -> Sources {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/hardware");
        Sources::read_dir(&dir.join(name)).unwrap()
    }

    /// The dev VM (QEMU's q35, as `make vm-up` starts it), captured by
    /// `tests/hardware/capture.sh` on 2026-10-24.
    fn dev_vm() -> Sources {
        captured("dev-vm")
    }

    fn status(report: &Report, kind: Kind) -> Status {
        report.parts.iter().find(|p| p.kind == kind).unwrap().status
    }

    fn part_of(report: &Report, kind: Kind) -> &Part {
        report.parts.iter().find(|p| p.kind == kind).unwrap()
    }

    /// The dev VM (2026-10-25): the hardware agent built snd_hda.ko for a
    /// kernel with snd_hda in it, and the start check took `kldload -n`'s
    /// "already loaded" for the patch's module.
    #[test]
    fn adr0057_a_driver_built_into_the_kernel_is_seen() {
        let kldstat = "Id Refs Address                Size Name\n \
             1   53 0xffffffff80200000  2351bd0 kernel (/boot/kernel/kernel)\n\
             \tContains modules:\n\
             \t\t Id Name\n\
             \t\t401 cubic\n\
             \t\t256 pci/snd_hda\n\
             \t\t257 hdac/snd_hda\n";
        assert!(kernel_has(kldstat, "snd_hda"));
        assert!(kernel_has(kldstat, "cubic"));
        assert!(!kernel_has(kldstat, "if_iwlwifi"));
        assert!(
            !kernel_has(kldstat, "kernel"),
            "the file's own line isn't a module"
        );
        assert!(!kernel_has(kldstat, "hda"));
    }

    #[test]
    fn dk12_the_dev_vm_has_nothing_the_os_cant_drive() {
        let report = assess(&dev_vm());
        assert_eq!(report.maker, "QEMU");
        assert_eq!(report.memory_mb, 4041);
        assert!(report.complete);
        assert_eq!(report.blockers().count(), 0, "{report:#?}");
        let expect = [
            (Kind::Graphics, Status::Limited),
            (Kind::Wifi, Status::Absent),
            (Kind::Ethernet, Status::Works),
            (Kind::Bluetooth, Status::Absent),
            (Kind::Camera, Status::Absent),
            (Kind::Audio, Status::Absent),
            (Kind::Pointer, Status::Works),
            (Kind::Sleep, Status::Works),
            (Kind::Battery, Status::Absent),
            (Kind::Tpm, Status::Absent),
        ];
        for (kind, want) in expect {
            assert_eq!(status(&report, kind), want, "{kind:?}: {report:#?}");
        }
        assert_eq!(
            part_of(&report, Kind::Graphics).devices,
            ["1234:1111 (vgapci0)"]
        );
        assert_eq!(
            part_of(&report, Kind::Ethernet).devices,
            ["Red Hat Virtio network device (virtio_pci0)"]
        );
    }

    /// A 2022 business laptop as its tools describe it: Wi-Fi 6E on Intel's
    /// CNVi with no firmware for it, the camera behind Intel's image
    /// processor, a touchpad on I²C with nothing loaded for it, DSP audio
    /// with no driver, and firmware with only modern standby.
    fn laptop_2022() -> Sources {
        Sources {
            kenv: "smbios.system.maker=\"LENOVO\"\nsmbios.system.product=\"21AH00FGUS\"\n".into(),
            sysctl: "12th Gen Intel(R) Core(TM) i7-1265U\n16849977344\n".into(),
            pciconf: "\
vgapci0@pci0:0:2:0:\tclass=0x030000 rev=0x0c hdr=0x00 vendor=0x8086 device=0x46a8 subvendor=0x17aa subdevice=0x22e6
    vendor     = 'Intel Corporation'
    device     = 'Alder Lake-UP3 GT2 [Iris Xe Graphics]'
    class      = display
    subclass   = VGA
none0@pci0:0:5:0:\tclass=0x048000 rev=0x04 hdr=0x00 vendor=0x8086 device=0x465d subvendor=0x17aa subdevice=0x22e6
    vendor     = 'Intel Corporation'
    device     = 'Alder Lake Imaging Signal Processor'
    class      = multimedia
iwlwifi0@pci0:0:20:3:\tclass=0x028000 rev=0x01 hdr=0x00 vendor=0x8086 device=0x51f0 subvendor=0x8086 subdevice=0x0090
    vendor     = 'Intel Corporation'
    device     = 'Alder Lake-P PCH CNVi WiFi'
    class      = network
none1@pci0:0:31:3:\tclass=0x040100 rev=0x01 hdr=0x00 vendor=0x8086 device=0x51c8 subvendor=0x17aa subdevice=0x22e6
    vendor     = 'Intel Corporation'
    device     = 'Alder Lake PCH-P High Definition Audio Controller'
    class      = multimedia
    subclass   = audio
"
            .into(),
            devinfo: "\
nexus0
  acpi0 <LENOVO TP-R1Y  >
    unknown pnpinfo _HID=ELAN0678 _UID=1 _CID=PNP0C50 at handle=\\_SB_.PC00.I2C1.TPD0
    unknown pnpinfo _HID=MSFT0101 _UID=1 _CID=none at handle=\\_SB_.TPM_
    atkbdc0 <Keyboard controller (i8042)> pnpinfo _HID=PNP0303 _UID=0 _CID=none at handle=\\_SB_.PC00.LPCB.KBD_
      atkbd0 <AT Keyboard>
              ubt0 <Intel product 0x0033, class 224/1, rev 2.00/0.01, addr 3> pnpinfo vendor=0x8087 product=0x0033 intclass=0xe0 intsubclass=0x01 intprotocol=0x01 at bus=0 hubaddr=1 port=10 devaddr=3 interface=0 ugen=ugen0.3
"
            .into(),
            usb: Some(
                "\
ugen0.3: <Intel product 0x0033> at usbus0, cfg=0 md=HOST spd=FULL (12Mbps) pwr=ON (100mA)
    Interface 0
      bInterfaceClass = 0x00e0  <Wireless>
      bInterfaceSubClass = 0x0001
      bInterfaceProtocol = 0x0001
"
                .into(),
            ),
            wlan: "\n".into(),
            sndstat: "No devices installed.\n".into(),
            sleep: "S4 S5\n".into(),
            battery: Some(
                "Design capacity:\t57000 mWh\nLast full capacity:\t52440 mWh\nCycle Count:\t\
                 87\nState:\t\t\tdischarging\nRemaining capacity:\t87%\n"
                    .into(),
            ),
            graphics_driver: false,
            intel_gpu_driver: false,
            camera_driver: false,
            brightness_levels: String::new(),
            dev: vec![],
            dmesg: "iwlwifi0: <iwlwifi> mem 0x603d1cc000-0x603d1cffff at device 20.3 on pci0\n\
                    iwlwifi0: Direct firmware load for iwlwifi-so-a0-gf-a0-89.ucode failed with \
                    error 2\niwlwifi0: no suitable firmware found!\n"
                .into(),
            intel_bt_firmware: false,
        }
    }

    #[test]
    fn dk12_a_2022_laptop_shows_what_keeps_it_off_the_list() {
        let report = assess(&laptop_2022());
        assert_eq!(
            (report.maker.as_str(), report.model.as_str()),
            ("LENOVO", "21AH00FGUS")
        );
        // What a kernel patch for it names (ADR 0057).
        assert!(
            report.fits(&["pci:8086:51f0".into()]),
            "{:?}",
            report.targets
        );
        assert!(report.fits(&["model:LENOVO 21AH00FGUS".into()]));
        assert!(!report.fits(&["pci:8086:2725".into()]));
        let blockers: Vec<Kind> = report.blockers().map(|p| p.kind).collect();
        assert_eq!(
            blockers,
            [
                Kind::Wifi,
                Kind::Bluetooth,
                Kind::Camera,
                Kind::Audio,
                Kind::Pointer,
                Kind::Sleep,
                Kind::Tpm
            ],
            "{report:#?}"
        );
        let wifi = part_of(&report, Kind::Wifi);
        assert!(
            wifi.note.contains("firmware isn't in this image"),
            "{}",
            wifi.note
        );
        assert!(
            wifi.note.contains("iwlwifi-so-a0-gf-a0-89.ucode"),
            "{}",
            wifi.note
        );
        assert!(
            part_of(&report, Kind::Camera)
                .note
                .contains("image processor")
        );
        assert!(part_of(&report, Kind::Bluetooth).note.contains("firmware"));
        assert!(
            part_of(&report, Kind::Pointer)
                .note
                .starts_with("1 touchpad")
        );
        let audio = &part_of(&report, Kind::Audio).note;
        assert!(
            audio.contains("High Definition Audio") && audio.contains("DSP"),
            "{audio}"
        );
        assert!(
            part_of(&report, Kind::Sleep)
                .note
                .contains("only modern standby")
        );
        assert_eq!(status(&report, Kind::Battery), Status::Works);
        assert_eq!(
            part_of(&report, Kind::Battery).note,
            "Holds 92% of what it was made to, 87 cycles; 87%, discharging"
        );
    }

    /// A laptop the image drives: Wi-Fi on firmware it carries, a USB
    /// camera with its driver running, HDA sound with a microphone, a HID
    /// touchpad, S3, a TPM.
    fn laptop_ready() -> Sources {
        Sources {
            kenv: "smbios.system.maker=\"Framework\"\nsmbios.system.product=\"Laptop\"\n".into(),
            sysctl: "Intel(R) Core(TM) i5-1135G7 @ 2.40GHz\n17000000000\n".into(),
            pciconf: "\
vgapci0@pci0:0:2:0:\tclass=0x030000 rev=0x01 hdr=0x00 vendor=0x8086 device=0x9a49 subvendor=0x8086 subdevice=0x3001
    vendor     = 'Intel Corporation'
    device     = 'TigerLake-LP GT2 [Iris Xe Graphics]'
iwm0@pci0:170:0:0:\tclass=0x028000 rev=0x1a hdr=0x00 vendor=0x8086 device=0x2526 subvendor=0x8086 subdevice=0x0014
    vendor     = 'Intel Corporation'
    device     = 'Wireless-AC 9260'
hdac0@pci0:0:31:3:\tclass=0x040300 rev=0x20 hdr=0x00 vendor=0x8086 device=0xa0c8 subvendor=0xf111 subdevice=0x0001
    vendor     = 'Intel Corporation'
    device     = 'Tiger Lake-LP Smart Sound Technology Audio Controller'
"
            .into(),
            devinfo: "\
          hmt0 <PIXA3854:00 093A:0274 TouchPad> pnpinfo page=0x000d usage=0x0005 bus=0x18 at index=1
    tpmcrb0 <Trusted Platform Module 2.0, CRB mode> pnpinfo _HID=MSFT0101 _UID=1 _CID=none at handle=\\_SB_.TPM_
"
            .into(),
            usb: Some(
                "\
ugen0.2: <Realtek Laptop Camera> at usbus0, cfg=0 md=HOST spd=HIGH (480Mbps) pwr=ON (500mA)
    Interface 0
      bInterfaceClass = 0x000e  <Video>
      bInterfaceSubClass = 0x0001
      bInterfaceProtocol = 0x0000
    Interface 1
      bInterfaceClass = 0x000e  <Video>
      bInterfaceSubClass = 0x0002
      bInterfaceProtocol = 0x0000
"
                .into(),
            ),
            wlan: "iwm0\n".into(),
            sndstat: "Installed devices:\npcm0: <Realtek ALC295 (Analog 2.0+HP/2.0)> (play/rec) \
                      default\npcm1: <Intel Tiger Lake (HDMI/DP 8ch)> (play)\n"
                .into(),
            sleep: "S3 S4 S5\n".into(),
            battery: Some("Design capacity:\t55000 mWh\nLast full capacity:\t38500 mWh\n".into()),
            graphics_driver: true,
            dev: vec![
                "backlight".into(),
                "tpm0".into(),
                "tpmrm0".into(),
                "video0".into(),
            ],
            dmesg: "iwm0: hw rev 0x320, fw ver 46.6b541b68.0, address f8:e4:e3:00:00:00\n".into(),
            intel_bt_firmware: false,
            intel_gpu_driver: true,
            camera_driver: true,
            brightness_levels: String::new(),
        }
    }

    #[test]
    fn adr0057_a_watchdog_restart_is_this_start_s_alone() {
        let ichwd = "ichwd0: <Intel ICH9 watchdog timer> on isa0\n\
                     ichwd0: resuming after hardware watchdog timeout\n";
        assert!(restarted_by_watchdog(ichwd));
        // The last start's, kept in the kernel's buffer, then this one's.
        let kept = format!("{ichwd}---<<BOOT>>---\nichwd0: <Intel ICH9 watchdog timer> on isa0\n");
        assert!(!restarted_by_watchdog(&kept));
        assert!(!restarted_by_watchdog(""));
    }

    #[test]
    fn dk12_a_laptop_the_image_drives_has_no_blockers() {
        let report = assess(&laptop_ready());
        assert_eq!(report.blockers().count(), 0, "{report:#?}");
        for (kind, want) in [
            (Kind::Wifi, Status::Works),
            (Kind::Camera, Status::Works),
            (Kind::Audio, Status::Works),
            (Kind::Pointer, Status::Works),
            (Kind::Sleep, Status::Works),
            (Kind::Tpm, Status::Works),
            (Kind::Graphics, Status::Limited),
            (Kind::Ethernet, Status::Absent),
        ] {
            assert_eq!(status(&report, kind), want, "{kind:?}: {report:#?}");
        }
        assert_eq!(
            part_of(&report, Kind::Tpm).devices,
            ["Trusted Platform Module 2.0, CRB mode (tpmcrb0)"]
        );
        // PM-6: a battery holding 70% of its design is worn.
        let battery = part_of(&report, Kind::Battery);
        assert_eq!(battery.status, Status::Limited);
        assert!(battery.note.starts_with("Holds 70%"), "{}", battery.note);
        // Every part the OS doesn't finish yet says which requirement.
        assert!(
            part_of(&report, Kind::Wifi)
                .gap
                .as_deref()
                .unwrap()
                .contains("CF-10")
        );
        // ADR 0060: the graphics driver's backlight sets the brightness.
        assert!(
            part_of(&report, Kind::Graphics)
                .note
                .contains("brightness set from Settings and its keys (the graphics driver's"),
            "{}",
            part_of(&report, Kind::Graphics).note
        );
        assert!(
            part_of(&report, Kind::Sleep)
                .gap
                .as_deref()
                .unwrap()
                .contains("PM-1")
        );
        assert!(
            part_of(&report, Kind::Graphics)
                .gap
                .as_deref()
                .unwrap()
                .contains("CF-32")
        );
    }

    /// ADR 0058: the image carries Intel's graphics driver, off until a model
    /// is checked with it, and the USB camera driver, which devd starts as a
    /// camera attaches.
    #[test]
    fn adr0058_the_image_s_drivers_are_named_when_they_aren_t_running() {
        let mut s = laptop_ready();
        s.graphics_driver = false;
        s.dev.retain(|d| d != "video0");
        let report = assess(&s);
        let note = |kind| {
            report
                .parts
                .iter()
                .find(|p| p.kind == kind)
                .map(|p| p.note.clone())
                .unwrap()
        };
        assert!(note(Kind::Graphics).contains("off until this model is checked"));
        assert_eq!(status(&report, Kind::Camera), Status::Limited);
        assert!(note(Kind::Camera).contains("isn't running"));
        s.camera_driver = false;
        s.intel_gpu_driver = false;
        let report = assess(&s);
        assert_eq!(status(&report, Kind::Camera), Status::Unsupported);
        assert!(
            report
                .parts
                .iter()
                .any(|p| p.kind == Kind::Graphics && p.note.contains("No driver for these"))
        );
    }

    #[test]
    fn dk12_without_root_usb_and_the_battery_read_as_unknown() {
        let mut sources = dev_vm();
        sources.usb = None;
        sources.battery = None;
        let report = assess(&sources);
        assert!(!report.complete);
        for kind in [Kind::Bluetooth, Kind::Camera, Kind::Battery] {
            assert_eq!(status(&report, kind), Status::Unknown, "{kind:?}");
        }
    }

    #[test]
    fn sound_without_a_microphone_is_limited() {
        for (line, play, rec) in [
            (
                "pcm0: <Realtek ALC257 (Analog)> (play/rec) default",
                true,
                true,
            ),
            ("pcm1: <Intel (HDMI/DP 8ch)> (play)", true, false),
            (
                "pcm0: <USB audio> on uaudio0 (1p:1v/0r:0v) default",
                true,
                false,
            ),
            (
                "pcm0: <HDA Codec> on hdaa0 (1p:1v/1r:1v) default",
                true,
                true,
            ),
            ("pcm2: <mic array> (rec)", false, true),
        ] {
            assert_eq!(channels(line), (play, rec), "{line}");
        }
        let report = assess(&Sources {
            sndstat: "pcm0: <USB audio> on uaudio0 (1p:1v/0r:0v) default\n".into(),
            ..dev_vm()
        });
        assert_eq!(status(&report, Kind::Audio), Status::Limited);
    }

    #[test]
    fn wifi_on_usb_counts_and_its_firmware_is_checked() {
        let mut sources = dev_vm();
        sources.wlan = "rtwn0\n".into();
        assert_eq!(status(&assess(&sources), Kind::Wifi), Status::Works);
        sources.dmesg = "rtwn0: could not load firmware image rtwn-rtl8192cfwU\n".into();
        assert_eq!(status(&assess(&sources), Kind::Wifi), Status::Unsupported);
    }

    /// What the matrix's emulated devices read as, from what
    /// `tests/hardware/capture.sh` saved in each (2026-10-24).
    #[test]
    fn dk12_emulated_devices_read_as_the_matrix_expects() {
        let expect: [(&str, Kind, Status, &str); 7] = [
            ("hda-mic", Kind::Audio, Status::Works, "(hdac0)"),
            ("hda-out", Kind::Audio, Status::Limited, "(hdac0)"),
            (
                "usb-audio",
                Kind::Audio,
                Status::Limited,
                "QEMU QEMU USB Audio",
            ),
            (
                "intel-nic",
                Kind::Ethernet,
                Status::Works,
                "82576 Gigabit Network Connection (igb0)",
            ),
            (
                "realtek-nic",
                Kind::Ethernet,
                Status::Works,
                "8139 PCI Fast Ethernet Adapter (re0)",
            ),
            (
                "tpm-crb",
                Kind::Tpm,
                Status::Works,
                "Trusted Platform Module 2.0, CRB mode (tpmcrb0)",
            ),
            ("tpm-unattached", Kind::Tpm, Status::Unsupported, "MSFT0101"),
        ];
        for (name, kind, want, device) in expect {
            let report = assess(&captured(name));
            let part = part_of(&report, kind);
            assert_eq!(part.status, want, "{name}: {part:#?}");
            assert!(
                part.devices.iter().any(|d| d.contains(device)),
                "{name}: no {device} in {:?}",
                part.devices
            );
        }
        let intel = assess(&captured("intel-nic"));
        assert!(
            part_of(&intel, Kind::Ethernet)
                .devices
                .iter()
                .any(|d| d.ends_with("(em0)"))
        );
    }

    #[test]
    fn adr0057_modules_run_on_their_release_at_any_patch_level() {
        assert!(release_takes("15.1-RELEASE", "15.1-RELEASE"));
        assert!(release_takes("15.1-RELEASE-p3", "15.1-RELEASE"));
        assert!(!release_takes("15.2-RELEASE", "15.1-RELEASE"));
        assert!(!release_takes("15.1-STABLE", "15.1-RELEASE"));
        assert!(!release_takes("15.1-RELEASE-px", "15.1-RELEASE"));
        assert!(module_name("if_iwlwifi") && !module_name("../x") && !module_name("-v"));
    }

    #[test]
    fn report_round_trips_as_json() {
        let report = assess(&laptop_2022());
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"kind\":\"wifi\""), "{json}");
        assert!(json.contains("\"status\":\"unsupported\""), "{json}");
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report);
    }
}
