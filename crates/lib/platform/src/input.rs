//! Keyboard and pointer input from the kernel's evdev devices
//! (`/dev/input/event*`), for the shell until the compositor takes input
//! over through libinput (ADR 0016).
//!
//! Events are `struct input_event` records read with `read(2)`; naming the
//! device, reading an absolute axis's range and taking the device for this
//! process alone are `ioctl(2)` requests. Taking a keyboard also keeps its
//! keys from the console, so typing doesn't reach the login prompt behind
//! the shell. FreeBSD only, apart from [`parse`].

use std::fs::File;
use std::io::{self, Read};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

/// Event types and codes from `<dev/evdev/input-event-codes.h>`, the same
/// numbers Linux uses.
pub mod codes {
    pub const EV_SYN: u16 = 0x00;
    pub const EV_KEY: u16 = 0x01;
    pub const EV_REL: u16 = 0x02;
    pub const EV_ABS: u16 = 0x03;
    /// The end of one batch of changes, such as an X and Y move together.
    pub const SYN_REPORT: u16 = 0;
    pub const REL_X: u16 = 0x00;
    pub const REL_Y: u16 = 0x01;
    pub const REL_WHEEL: u16 = 0x08;
    pub const ABS_X: u16 = 0x00;
    pub const ABS_Y: u16 = 0x01;
    pub const BTN_LEFT: u16 = 0x110;
    pub const BTN_RIGHT: u16 = 0x111;
    pub const BTN_MIDDLE: u16 = 0x112;
}

/// One evdev event: a key or button (`value` 1 down, 0 up, 2 repeat), a
/// relative motion, an absolute position, or the end of a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEvent {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// `struct input_event` on 64-bit systems: a 16-byte `struct timeval`, then
/// type, code and value.
const RECORD: usize = 24;

/// The events in `buf`, whole records only.
pub fn parse(buf: &[u8]) -> Vec<RawEvent> {
    buf.chunks_exact(RECORD)
        .map(|r| RawEvent {
            kind: u16::from_ne_bytes([r[16], r[17]]),
            code: u16::from_ne_bytes([r[18], r[19]]),
            value: i32::from_ne_bytes([r[20], r[21], r[22], r[23]]),
        })
        .collect()
}

/// The evdev devices on this machine, in name order.
pub fn devices() -> io::Result<Vec<PathBuf>> {
    let mut found: Vec<PathBuf> = std::fs::read_dir("/dev/input")?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .collect();
    found.sort();
    Ok(found)
}

/// An open evdev device.
pub struct InputDevice {
    file: File,
    path: PathBuf,
}

impl InputDevice {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The device's name, such as "QEMU QEMU USB Tablet".
    pub fn name(&self) -> io::Result<String> {
        sys::name(&self.file)
    }

    /// The range an absolute axis (`codes::ABS_X`, `codes::ABS_Y`) reports.
    pub fn abs_range(&self, axis: u16) -> io::Result<RangeInclusive<i32>> {
        sys::abs_range(&self.file, axis)
    }

    /// Takes the device for this process alone until it's closed. For a
    /// keyboard, the console stops getting its keys too.
    pub fn grab(&self) -> io::Result<()> {
        sys::grab(&self.file)
    }

    /// Waits for events and returns them.
    pub fn read(&mut self) -> io::Result<Vec<RawEvent>> {
        let mut buf = [0u8; RECORD * 64];
        let n = self.file.read(&mut buf)?;
        Ok(parse(&buf[..n - n % RECORD]))
    }
}

#[cfg(target_os = "freebsd")]
mod sys {
    use std::fs::File;
    use std::io;
    use std::ops::RangeInclusive;
    use std::os::fd::AsRawFd;

    use libc::{c_int, c_ulong};

    use crate::ioctl::{self, check};

    /// `struct input_absinfo` from `<dev/evdev/input.h>`.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct AbsInfo {
        value: i32,
        minimum: i32,
        maximum: i32,
        fuzz: i32,
        flat: i32,
        resolution: i32,
    }

    const NAME_LEN: usize = 256;
    pub(super) const EVIOCGNAME: c_ulong = ioctl::read(b'E', 0x06, NAME_LEN);
    pub(super) const fn eviocgabs(axis: u8) -> c_ulong {
        ioctl::read(b'E', 0x40 + axis, size_of::<AbsInfo>())
    }
    pub(super) const EVIOCGRAB: c_ulong = ioctl::int(b'E', 0x90);

    pub(super) fn name(file: &File) -> io::Result<String> {
        let mut buf = [0u8; NAME_LEN];
        // SAFETY: EVIOCGNAME(len) writes at most `len` bytes, the size of
        // `buf`, to the pointer.
        check(unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGNAME, buf.as_mut_ptr()) })?;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
    }

    pub(super) fn abs_range(file: &File, axis: u16) -> io::Result<RangeInclusive<i32>> {
        let axis = u8::try_from(axis)
            .ok()
            .filter(|a| *a < 0x40)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut info = AbsInfo::default();
        // SAFETY: EVIOCGABS writes one `struct input_absinfo` to the
        // pointer, which points to an `AbsInfo` of that layout.
        check(unsafe {
            libc::ioctl(file.as_raw_fd(), eviocgabs(axis), &mut info as *mut AbsInfo)
        })?;
        Ok(info.minimum..=info.maximum)
    }

    pub(super) fn grab(file: &File) -> io::Result<()> {
        // SAFETY: EVIOCGRAB takes 1 (grab) by value.
        check(unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB, 1 as c_int) })
    }
}

#[cfg(not(target_os = "freebsd"))]
mod sys {
    use std::fs::File;
    use std::io;
    use std::ops::RangeInclusive;

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "evdev requests are only built for FreeBSD",
        ))
    }

    pub(super) fn name(_: &File) -> io::Result<String> {
        unsupported()
    }

    pub(super) fn abs_range(_: &File, _: u16) -> io::Result<RangeInclusive<i32>> {
        unsupported()
    }

    pub(super) fn grab(_: &File) -> io::Result<()> {
        unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u16, code: u16, value: i32) -> Vec<u8> {
        let mut r = vec![0u8; 16];
        r.extend(kind.to_ne_bytes());
        r.extend(code.to_ne_bytes());
        r.extend(value.to_ne_bytes());
        r
    }

    #[test]
    fn reads_whole_input_event_records() {
        let mut buf = record(codes::EV_ABS, codes::ABS_X, 16_384);
        buf.extend(record(codes::EV_KEY, codes::BTN_LEFT, 1));
        buf.extend(record(codes::EV_SYN, codes::SYN_REPORT, 0));
        buf.extend([0u8; 5]);
        assert_eq!(
            parse(&buf),
            [
                RawEvent {
                    kind: codes::EV_ABS,
                    code: codes::ABS_X,
                    value: 16_384
                },
                RawEvent {
                    kind: codes::EV_KEY,
                    code: codes::BTN_LEFT,
                    value: 1
                },
                RawEvent {
                    kind: codes::EV_SYN,
                    code: codes::SYN_REPORT,
                    value: 0
                },
            ]
        );
    }

    #[test]
    #[cfg(target_os = "freebsd")]
    fn request_numbers_match_the_system_headers() {
        // `<dev/evdev/input.h>`, FreeBSD 15.
        assert_eq!(sys::EVIOCGNAME, 0x4100_4506);
        assert_eq!(sys::eviocgabs(0), 0x4018_4540);
        assert_eq!(sys::EVIOCGRAB, 0x2004_4590);
    }
}
