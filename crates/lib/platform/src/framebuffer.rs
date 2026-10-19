//! The console framebuffer (ADR 0016): the screen as memory, for the shell
//! where there is no GPU driver, as in the development VM, or the driver
//! failed.
//!
//! [`Framebuffer::open`] opens a virtual terminal such as `/dev/ttyv0`,
//! reads the screen's size and pixel layout, maps it, and switches the
//! terminal to graphics mode so the console stops drawing text over it.
//! Dropping it switches the terminal back and has it redraw. The efifb (UEFI) and vbefb (BIOS
//! VBE) consoles answer these requests; Xorg's scfb driver uses the same
//! ones. FreeBSD only.

use std::io;
use std::ops::Range;
use std::path::Path;

/// The screen's size and how a pixel is laid out in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    /// Bytes from the start of one row to the next.
    pub stride: u32,
    /// Where each 8-bit channel starts within a 32-bit pixel.
    pub red_shift: u32,
    pub green_shift: u32,
    pub blue_shift: u32,
}

/// Bytes per pixel. Only 32-bit layouts are supported: what UEFI firmware
/// and QEMU's VGA give.
const BYTES_PER_PIXEL: usize = 4;

/// The byte order of the pixels handed to the framebuffer: straight RGBA,
/// or B, G, R, A as software renderers commonly draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Rgba,
    Bgra,
}

impl Geometry {
    /// Packs one row of 4-byte pixels in `order` into this screen's layout.
    /// A row longer than the screen is cut; alpha is dropped. B, G, R, A on
    /// a screen laid out that way (blue lowest, as UEFI's usually is) is
    /// copied as it is.
    pub fn pack_row(&self, pixels: &[u8], order: Order, out: &mut [u8]) {
        let n = (self.width as usize * BYTES_PER_PIXEL)
            .min(pixels.len() - pixels.len() % 4)
            .min(out.len());
        if order == Order::Bgra && (self.red_shift, self.green_shift, self.blue_shift) == (16, 8, 0)
        {
            out[..n].copy_from_slice(&pixels[..n]);
            return;
        }
        let (r, b) = match order {
            Order::Rgba => (0, 2),
            Order::Bgra => (2, 0),
        };
        for (src, dst) in pixels[..n]
            .chunks_exact(4)
            .zip(out[..n].chunks_exact_mut(BYTES_PER_PIXEL))
        {
            let value = (u32::from(src[r]) << self.red_shift)
                | (u32::from(src[1]) << self.green_shift)
                | (u32::from(src[b]) << self.blue_shift);
            dst.copy_from_slice(&value.to_le_bytes());
        }
    }
}

/// A mapped console framebuffer.
pub struct Framebuffer {
    geometry: Geometry,
    #[cfg(target_os = "freebsd")]
    inner: sys::Mapping,
}

impl Framebuffer {
    /// Opens and maps the framebuffer behind the virtual terminal `tty`, and
    /// puts that terminal in graphics mode.
    pub fn open(tty: &Path) -> io::Result<Self> {
        #[cfg(target_os = "freebsd")]
        {
            let inner = sys::Mapping::open(tty)?;
            Ok(Self {
                geometry: inner.geometry,
                inner,
            })
        }
        #[cfg(not(target_os = "freebsd"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{}: no console framebuffer on this system", tty.display()),
            ))
        }
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Closes the framebuffer but leaves the last frame up and the terminal
    /// in graphics mode, for the next program to draw over (the sign-in
    /// screen handing over to the desktop and back) without the text
    /// console showing in between. Dropping it instead puts the console
    /// back.
    pub fn hand_over(self) {
        #[cfg(target_os = "freebsd")]
        {
            let mut inner = self.inner;
            inner.restore = false;
        }
    }

    /// Makes this framebuffer's virtual terminal the one on screen, and
    /// returns once it is. The console repaints a terminal it switches to
    /// (its text, here none, and a cursor), so draw after this; from then
    /// on the console's messages, which go to the first terminal, don't
    /// show, and nothing repaints this one while nothing writes to it.
    pub fn show(&self) -> io::Result<()> {
        #[cfg(target_os = "freebsd")]
        {
            self.inner.show()
        }
        #[cfg(not(target_os = "freebsd"))]
        {
            Ok(())
        }
    }

    /// Copies rows `rows` of a straight RGBA frame `width` pixels wide onto
    /// the screen, from its top left corner. Rows and columns past the
    /// screen's edge are left out.
    pub fn write_rgba(&mut self, rgba: &[u8], width: u32, rows: Range<u32>) {
        let stride = width as usize * 4;
        let band = rows.start as usize * stride..(rows.end as usize * stride).min(rgba.len());
        self.write_rows(
            rgba.get(band).unwrap_or_default(),
            Order::Rgba,
            width,
            rows.start,
        );
    }

    /// Copies whole rows of 4-byte pixels in `order`, `width` wide, onto the
    /// screen from row `top` down, such as a redrawn area or the band a
    /// moving pointer crosses. Rows and columns past the screen's edge are
    /// left out.
    pub fn write_rows(&mut self, pixels: &[u8], order: Order, width: u32, top: u32) {
        let g = self.geometry;
        let src_stride = width as usize * 4;
        if src_stride == 0 {
            return;
        }
        let screen = self.pixels_mut();
        for (y, src) in (top as usize..g.height as usize).zip(pixels.chunks_exact(src_stride)) {
            let dst = y * g.stride as usize;
            let Some(dst) = screen.get_mut(dst..dst + g.stride as usize) else {
                break;
            };
            g.pack_row(src, order, dst);
        }
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        #[cfg(target_os = "freebsd")]
        {
            self.inner.bytes()
        }
        #[cfg(not(target_os = "freebsd"))]
        {
            &mut []
        }
    }
}

#[cfg(target_os = "freebsd")]
mod sys {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;
    use std::ptr::NonNull;

    use libc::{c_int, c_uint, c_ulong};

    use super::{BYTES_PER_PIXEL, Geometry};
    use crate::ioctl::{self, check};

    /// `struct fbtype` from `<sys/fbio.h>`.
    #[repr(C)]
    #[derive(Default)]
    struct FbType {
        fb_type: c_int,
        fb_height: c_int,
        fb_width: c_int,
        fb_depth: c_int,
        fb_cmsize: c_int,
        fb_size: c_int,
    }

    /// `struct fb_rgboffs` from `<sys/fbio.h>`.
    #[repr(C)]
    #[derive(Default)]
    struct RgbOffsets {
        red: c_int,
        green: c_int,
        blue: c_int,
    }

    pub(super) const FBIOGTYPE: c_ulong = ioctl::read(b'F', 0, size_of::<FbType>());
    pub(super) const FBIO_GETLINEWIDTH: c_ulong = ioctl::read(b'F', 111, size_of::<c_uint>());
    pub(super) const FBIO_GETRGBOFFS: c_ulong = ioctl::read(b'F', 116, size_of::<RgbOffsets>());
    // `<sys/consio.h>`.
    pub(super) const KDSETMODE: c_ulong = ioctl::int(b'K', 10);
    const KD_TEXT: c_int = 0;
    const KD_GRAPHICS: c_int = 1;
    pub(super) const VT_ACTIVATE: c_ulong = ioctl::int(b'v', 5);
    pub(super) const VT_WAITACTIVE: c_ulong = ioctl::int(b'v', 6);
    pub(super) const VT_GETACTIVE: c_ulong = ioctl::read(b'v', 7, size_of::<c_int>());
    pub(super) const VT_GETINDEX: c_ulong = ioctl::read(b'v', 8, size_of::<c_int>());

    pub(super) struct Mapping {
        tty: File,
        base: NonNull<u8>,
        len: usize,
        pub(super) geometry: Geometry,
        /// Put the terminal back to text when closing.
        pub(super) restore: bool,
    }

    impl Mapping {
        pub(super) fn open(path: &Path) -> io::Result<Self> {
            let named = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
            // Never become the terminal's controlling process.
            let tty = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOCTTY)
                .open(path)
                .map_err(named)?;
            let fd = tty.as_raw_fd();

            let mut fb = FbType::default();
            // SAFETY: FBIOGTYPE writes one `struct fbtype` to the pointer,
            // which points to an `FbType` of that layout.
            check(unsafe { libc::ioctl(fd, FBIOGTYPE, &mut fb as *mut FbType) }).map_err(named)?;
            let mut stride: c_uint = 0;
            // SAFETY: FBIO_GETLINEWIDTH writes one `u_int` to the pointer.
            check(unsafe { libc::ioctl(fd, FBIO_GETLINEWIDTH, &mut stride as *mut c_uint) })
                .map_err(named)?;
            let mut offsets = RgbOffsets::default();
            // SAFETY: FBIO_GETRGBOFFS writes one `struct fb_rgboffs` to the
            // pointer, which points to an `RgbOffsets` of that layout.
            let rgb =
                check(unsafe { libc::ioctl(fd, FBIO_GETRGBOFFS, &mut offsets as *mut RgbOffsets) });

            let (width, height) = (fb.fb_width.max(0) as u32, fb.fb_height.max(0) as u32);
            if width == 0
                || height == 0
                || stride < width * BYTES_PER_PIXEL as u32
                || fb.fb_depth <= 16
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "{}: a {width}x{height} framebuffer at depth {} with {stride}-byte rows; \
                         only 32-bit pixels are supported",
                        path.display(),
                        fb.fb_depth
                    ),
                ));
            }
            // Consoles that don't say are 8-8-8 with blue lowest, as UEFI's
            // usual layout.
            let (red_shift, green_shift, blue_shift) = match rgb {
                Ok(()) if offsets.red | offsets.green | offsets.blue != 0 => (
                    offsets.red as u32,
                    offsets.green as u32,
                    offsets.blue as u32,
                ),
                _ => (16, 8, 0),
            };
            let geometry = Geometry {
                width,
                height,
                stride,
                red_shift,
                green_shift,
                blue_shift,
            };
            let len = stride as usize * height as usize;

            // SAFETY: maps `len` bytes of the device behind `fd`, shared, at
            // an address the kernel picks; nothing else in this process
            // aliases it. The result is checked before use.
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            if base == libc::MAP_FAILED {
                return Err(named(io::Error::last_os_error()));
            }
            let base = NonNull::new(base.cast::<u8>())
                .ok_or_else(|| named(io::Error::other("null mapping")))?;
            let mapping = Self {
                tty,
                base,
                len,
                geometry,
                restore: true,
            };
            // SAFETY: KDSETMODE takes the mode by value.
            check(unsafe { libc::ioctl(fd, KDSETMODE, KD_GRAPHICS) }).map_err(named)?;
            Ok(mapping)
        }

        pub(super) fn bytes(&mut self) -> &mut [u8] {
            // SAFETY: `base` is a live mapping of `len` writable bytes until
            // drop, and `&mut self` makes this the only reference to it.
            unsafe { std::slice::from_raw_parts_mut(self.base.as_ptr(), self.len) }
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            let fd = self.tty.as_raw_fd();
            // SAFETY: `base` and `len` are the mapping made in `open`,
            // unmapped once, here; KDSETMODE takes the mode by value.
            unsafe {
                libc::munmap(self.base.as_ptr().cast(), self.len);
                if self.restore {
                    libc::ioctl(fd, KDSETMODE, KD_TEXT);
                }
            }
            if self.restore {
                redraw(fd);
            }
        }
    }

    impl Mapping {
        pub(super) fn show(&self) -> io::Result<()> {
            let fd = self.tty.as_raw_fd();
            let mut ours: c_int = 0;
            // SAFETY: VT_GETINDEX writes one `int` to the pointer;
            // VT_ACTIVATE takes the terminal's number by value.
            check(unsafe { libc::ioctl(fd, VT_GETINDEX, &mut ours as *mut c_int) })?;
            // No cursor in the repaint (DECTCEM off).
            (&self.tty).write_all(b"\x1b[?25l")?;
            check(unsafe { libc::ioctl(fd, VT_ACTIVATE, ours) })?;
            // SAFETY: VT_WAITACTIVE takes the terminal's number by value.
            check(unsafe { libc::ioctl(fd, VT_WAITACTIVE, ours) })?;
            // The repaint itself comes on the console's next flush.
            std::thread::sleep(std::time::Duration::from_millis(150));
            Ok(())
        }
    }

    /// Text mode alone leaves the last frame up until the console next
    /// draws. Switching to another terminal and back has the console redraw
    /// all of this one now, if it's the one showing.
    fn redraw(fd: c_int) {
        let (mut showing, mut ours): (c_int, c_int) = (0, 0);
        // SAFETY: VT_GETACTIVE and VT_GETINDEX each write one `int` to the
        // pointer; VT_ACTIVATE takes the terminal's number by value.
        unsafe {
            if libc::ioctl(fd, VT_GETACTIVE, &mut showing as *mut c_int) < 0
                || libc::ioctl(fd, VT_GETINDEX, &mut ours as *mut c_int) < 0
                || showing != ours
            {
                return;
            }
            let other: c_int = if ours == 1 { 2 } else { 1 };
            if libc::ioctl(fd, VT_ACTIVATE, other) == 0 {
                libc::ioctl(fd, VT_ACTIVATE, ours);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(red_shift: u32, blue_shift: u32) -> Geometry {
        Geometry {
            width: 2,
            height: 1,
            stride: 8,
            red_shift,
            green_shift: 8,
            blue_shift,
        }
    }

    #[test]
    fn packs_rgba_into_the_screens_channel_order() {
        let rgba = [0x11, 0x22, 0x33, 0xff, 0xaa, 0xbb, 0xcc, 0x00];
        let mut bgrx = [0u8; 8];
        geometry(16, 0).pack_row(&rgba, Order::Rgba, &mut bgrx);
        assert_eq!(bgrx, [0x33, 0x22, 0x11, 0, 0xcc, 0xbb, 0xaa, 0]);
        let mut rgbx = [0u8; 8];
        geometry(0, 16).pack_row(&rgba, Order::Rgba, &mut rgbx);
        assert_eq!(rgbx, [0x11, 0x22, 0x33, 0, 0xaa, 0xbb, 0xcc, 0]);
        // B, G, R, A onto blue-lowest is the same bytes; onto red-lowest it
        // swaps.
        let bgra = [0x33, 0x22, 0x11, 0xff, 0xcc, 0xbb, 0xaa, 0x00];
        let mut same = [0u8; 8];
        geometry(16, 0).pack_row(&bgra, Order::Bgra, &mut same);
        assert_eq!(&same[..3], &[0x33, 0x22, 0x11]);
        geometry(0, 16).pack_row(&bgra, Order::Bgra, &mut rgbx);
        assert_eq!(rgbx, [0x11, 0x22, 0x33, 0, 0xaa, 0xbb, 0xcc, 0]);
    }

    #[test]
    fn a_row_wider_than_the_screen_is_cut() {
        let rgba = [0xff; 12];
        let mut out = [0u8; 12];
        geometry(16, 0).pack_row(&rgba, Order::Rgba, &mut out);
        assert_eq!(&out[8..], &[0, 0, 0, 0]);
    }

    #[test]
    #[cfg(not(target_os = "freebsd"))]
    fn other_systems_have_no_console_framebuffer() {
        let err = Framebuffer::open(Path::new("/dev/ttyv0"))
            .err()
            .expect("unsupported");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    #[cfg(target_os = "freebsd")]
    fn request_numbers_match_the_system_headers() {
        // `<sys/fbio.h>` and `<sys/consio.h>`, FreeBSD 15.
        assert_eq!(sys::FBIOGTYPE, 0x4018_4600);
        assert_eq!(sys::FBIO_GETLINEWIDTH, 0x4004_466f);
        assert_eq!(sys::FBIO_GETRGBOFFS, 0x400c_4674);
        assert_eq!(sys::KDSETMODE, 0x2004_4b0a);
        assert_eq!(sys::VT_ACTIVATE, 0x2004_7605);
        assert_eq!(sys::VT_WAITACTIVE, 0x2004_7606);
        assert_eq!(sys::VT_GETACTIVE, 0x4004_7607);
        assert_eq!(sys::VT_GETINDEX, 0x4004_7608);
    }
}
