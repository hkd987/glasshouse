//! Request numbers for FreeBSD's `ioctl(2)`, built the way `<sys/ioccom.h>`
//! builds them, for the device modules (`framebuffer`, `input`). Each module
//! checks its numbers against the system headers in a test.

use std::io;

use libc::{c_int, c_ulong};

const IOC_VOID: c_ulong = 0x2000_0000;
const IOC_OUT: c_ulong = 0x4000_0000;

/// `_IOC`.
pub(crate) const fn ioc(dir: c_ulong, group: u8, num: u8, len: usize) -> c_ulong {
    dir | ((len as c_ulong & 0x1fff) << 16) | ((group as c_ulong) << 8) | num as c_ulong
}

/// `_IOR`: the kernel writes a `len`-byte value to the pointer passed.
pub(crate) const fn read(group: u8, num: u8, len: usize) -> c_ulong {
    ioc(IOC_OUT, group, num, len)
}

/// `_IOWINT`: an `int` passed by value.
pub(crate) const fn int(group: u8, num: u8) -> c_ulong {
    ioc(IOC_VOID, group, num, size_of::<c_int>())
}

/// `ioctl(2)`'s result as an `io::Result`.
pub(crate) fn check(rc: c_int) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
