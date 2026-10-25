//! The screen's brightness (ADR 0060), through the base system.
//!
//! - **backlight(8)**: the backlight a graphics driver gives, under
//!   `/dev/backlight` (i915kms, once the model's graphics are on, ADR 0058).
//! - **acpi_video(4)**: the firmware's own levels, `hw.acpi.video.lcd0`,
//!   on laptops whose firmware dims the screen without the graphics driver.
//!
//! Brightness is a percent. [`LEAST`] is the least it's set to from here,
//! so no slip of a slider or key turns the screen off: what agents are
//! doing on it stays visible.

use std::path::{Path, PathBuf};

use crate::{PlatformError, Result, cmd};

/// The dimmest the screen is set from here, in percent.
pub const LEAST: u8 = 5;
/// The brightness keys' step, in percent, on a backlight with no levels of
/// its own.
pub const STEP: u8 = 10;
/// backlight(8)'s devices.
const DEVICES: &str = "/dev/backlight";
/// acpi_video(4)'s panel.
const PANEL: &str = "hw.acpi.video.lcd0";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// A backlight(8) device.
    Device(PathBuf),
    /// acpi_video(4), with the levels the firmware offers, in percent.
    Firmware(Vec<u8>),
    /// A file holding a percent: for a computer with no screen of its own
    /// to dim (development, tests).
    StandIn(PathBuf),
}

/// The screen's backlight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    source: Source,
}

impl Backlight {
    /// The computer's: a backlight(8) device first, then the firmware's.
    /// `None` when the screen can't be dimmed from here (a desktop
    /// monitor, a VM, a laptop whose graphics are still off and whose
    /// firmware offers no levels).
    pub fn find() -> Option<Self> {
        if let Some(device) = device(Path::new(DEVICES)) {
            return Some(Self {
                source: Source::Device(device),
            });
        }
        let levels = cmd::run("sysctl", &["-n", &format!("{PANEL}.levels")]).ok()?;
        let levels = firmware_levels(&levels);
        (!levels.is_empty()).then_some(Self {
            source: Source::Firmware(levels),
        })
    }

    /// A stand-in kept in `path`: 100 until set.
    pub fn stand_in(path: impl Into<PathBuf>) -> Self {
        Self {
            source: Source::StandIn(path.into()),
        }
    }

    /// Which it is, for admin logs: `backlight0`, `acpi_video` or
    /// `stand-in`.
    pub fn name(&self) -> String {
        match &self.source {
            Source::Device(path) => path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            Source::Firmware(_) => "acpi_video".into(),
            Source::StandIn(_) => "stand-in".into(),
        }
    }

    /// The brightness now, in percent.
    pub fn percent(&self) -> Result<u8> {
        let text = match &self.source {
            Source::Device(path) => cmd::run("backlight", &["-f", &path.to_string_lossy(), "-q"])?,
            Source::Firmware(_) => cmd::run("sysctl", &["-n", &format!("{PANEL}.brightness")])?,
            Source::StandIn(path) => match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "100".into(),
                Err(e) => return Err(PlatformError::io(path.display().to_string(), e)),
            },
        };
        text.trim()
            .parse::<u32>()
            .map(|p| p.min(100) as u8)
            .map_err(|_| PlatformError::Unsupported(format!("a brightness of {:?}", text.trim())))
    }

    /// Sets it to `percent`, or the firmware's nearest level, never below
    /// [`LEAST`]; what it is then.
    pub fn set(&self, percent: u8) -> Result<u8> {
        let to = match &self.source {
            Source::Firmware(_) => nearest(&self.levels(), percent),
            _ => percent.clamp(LEAST, 100),
        };
        match &self.source {
            Source::Device(path) => {
                cmd::run(
                    "backlight",
                    &["-f", &path.to_string_lossy(), &to.to_string()],
                )?;
            }
            Source::Firmware(_) => {
                cmd::run("sysctl", &[&format!("{PANEL}.brightness={to}")])?;
            }
            Source::StandIn(path) => std::fs::write(path, format!("{to}\n"))
                .map_err(|e| PlatformError::io(path.display().to_string(), e))?,
        }
        Ok(to)
    }

    /// One step brighter or dimmer, as the brightness keys go; what it is
    /// then.
    pub fn step(&self, up: bool) -> Result<u8> {
        let now = self.percent()?;
        self.set(step(&self.levels(), now, up))
    }

    /// The levels the keys step through, dimmest first: the firmware's
    /// (from [`LEAST`]), or every [`STEP`].
    fn levels(&self) -> Vec<u8> {
        match &self.source {
            Source::Firmware(levels) => {
                let usable: Vec<u8> = levels.iter().copied().filter(|l| *l >= LEAST).collect();
                if usable.is_empty() {
                    levels.clone()
                } else {
                    usable
                }
            }
            _ => std::iter::once(LEAST)
                .chain((STEP..=100).step_by(usize::from(STEP)))
                .collect(),
        }
    }
}

/// The first backlight(8) device in `dir`: `backlight0` and its kind,
/// such as `intel_backlight0`, name the same one.
fn device(dir: &Path) -> Option<PathBuf> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            n.strip_prefix("backlight")
                .is_some_and(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    names.sort();
    names.first().map(|n| dir.join(n))
}

/// acpi_video's `levels`: ACPI's `_BCL`, whose first two are the levels
/// for mains power and for the battery, then each level the panel has.
/// The panel's, dimmest first.
fn firmware_levels(text: &str) -> Vec<u8> {
    let all: Vec<u8> = text
        .split_whitespace()
        .filter_map(|w| w.parse::<u32>().ok())
        .filter(|l| *l <= 100)
        .map(|l| l as u8)
        .collect();
    let mut levels = if all.len() > 2 {
        all[2..].to_vec()
    } else {
        all
    };
    levels.sort_unstable();
    levels.dedup();
    levels
}

/// The level nearest `percent`, not below [`LEAST`] where one is.
fn nearest(levels: &[u8], percent: u8) -> u8 {
    let want = percent.clamp(LEAST, 100);
    levels
        .iter()
        .copied()
        .min_by_key(|l| (l.abs_diff(want), *l < LEAST))
        .unwrap_or(want)
}

/// The next level up or down from `now`; the brightest or dimmest when
/// there's none.
fn step(levels: &[u8], now: u8, up: bool) -> u8 {
    let mut found = if up {
        levels.iter().copied().find(|l| *l > now)
    } else {
        levels.iter().rev().copied().find(|l| *l < now)
    };
    if found.is_none() {
        found = if up { levels.last() } else { levels.first() }.copied();
    }
    found.unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0060_the_firmware_s_levels_leave_out_its_two_defaults() {
        // A ThinkPad's: mains 100, battery 60, then the panel's own.
        assert_eq!(
            firmware_levels("100 60 0 1 3 6 10 15 21 28 36 45 55 66 78 90 100\n"),
            vec![0, 1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 66, 78, 90, 100]
        );
        assert_eq!(firmware_levels(""), Vec::<u8>::new());
    }

    #[test]
    fn adr0060_the_screen_never_goes_dark_from_here() {
        let firmware = Backlight {
            source: Source::Firmware(firmware_levels("100 60 0 1 3 6 10 15 21 100")),
        };
        let levels = firmware.levels();
        assert_eq!(levels, vec![6, 10, 15, 21, 100], "0, 1 and 3 aren't used");
        assert_eq!(nearest(&levels, 0), 6);
        assert_eq!(nearest(&levels, 18), 15, "the nearer of 15 and 21");
        assert_eq!(step(&levels, 6, false), 6, "the dimmest stays the dimmest");
        assert_eq!(step(&levels, 3, true), 6, "up from below the least");
        assert_eq!(step(&levels, 21, true), 100);
        assert_eq!(step(&levels, 100, true), 100);
        // Levels that are all below the least are used as they are.
        assert_eq!(nearest(&[1, 2, 3], 50), 3);
    }

    #[test]
    fn adr0060_a_stand_in_steps_by_ten() {
        let dir = tempfile::tempdir().unwrap();
        let screen = Backlight::stand_in(dir.path().join("brightness"));
        assert_eq!(screen.percent().unwrap(), 100, "100 until set");
        assert_eq!(screen.step(false).unwrap(), 90);
        assert_eq!(screen.set(2).unwrap(), LEAST);
        assert_eq!(screen.step(false).unwrap(), LEAST);
        assert_eq!(screen.step(true).unwrap(), 10);
        assert_eq!(screen.set(45).unwrap(), 45, "a slider's percent as it is");
        assert_eq!(screen.step(true).unwrap(), 50, "the next step up");
        assert_eq!(screen.percent().unwrap(), 50);
    }

    #[test]
    fn adr0060_backlight_devices_by_number() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(device(dir.path()), None);
        for name in ["intel_backlight0", "backlight1", "backlight0", "backlightx"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        assert_eq!(device(dir.path()), Some(dir.path().join("backlight0")));
    }
}
