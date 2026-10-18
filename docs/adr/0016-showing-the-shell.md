# 0016. Showing the shell: framebuffer in the VM, DRM on hardware

Status: **Accepted** (the framebuffer output is verified in the VM; the
compositor is next). PRD UI-1, UI-3, UI-4, DK-1. Builds on ADR 0009.

## Context
The shell (`shell/`, iced on the tiny-skia software renderer) renders the
desktop, sign-in, boot and command-bar screens to PNG today (M1). The next
milestone (M2) puts it on a screen: our Smithay compositor owns the display
and input, and the shell draws into it.

Development happens in a QEMU VM without KVM, so everything is emulated.
Findings on FreeBSD 15.1, from the drm-kmod and FreeBSD source trees, the
niri port and the 15.1 package catalog:

- **There is no DRM device in the VM.** drm-kmod builds only the Intel,
  AMD and Radeon drivers; it has no virtio-gpu, bochs, QXL or simpledrm.
  FreeBSD's own `virtio_gpu(4)` is console-only and refuses mmap. So
  Smithay's DRM/GBM path can't run under QEMU.
- **The console framebuffer can be mapped.** With the efifb (UEFI) or
  vbefb (BIOS VBE) console, `/dev/ttyvN` answers `FBIOGTYPE`,
  `FBIO_GETLINEWIDTH`, `FBIO_GETRGBOFFS` and mmap. Xorg's scfb driver uses
  exactly this, and `KDSETMODE KD_GRAPHICS` keeps the console from drawing
  over it. QEMU's `-device VGA,xres=…,yres=…` gives OVMF a linear
  framebuffer; `virtio-gpu-pci` does not.
- **Smithay runs on FreeBSD** (the niri port: libudev-devd, seatd, libinput,
  GBM), with two fixes now upstream in drm-rs. Its pixman renderer needs no
  GPU. Input in the VM is `usb-tablet` and `usb-kbd` (no virtio-input on
  FreeBSD), read through evdev and libinput.
- Every needed package (seatd, libinput, libudev-devd, libxkbcommon,
  pixman) exists for 15.1 amd64.

## Decision
- **One compositor, two outputs.** The compositor is Smithay with the
  pixman renderer, libinput and libseat. It has a DRM/KMS output for
  hardware (drm-kmod, `i915kms` or `amdgpu`) and a **framebuffer output**:
  map the console framebuffer, composite into a RAM buffer, and copy only
  the damaged rectangles, paced by a timer since there is no vblank. The
  framebuffer output is also the fallback on hardware whose GPU driver
  fails.
- **The VM boots with a framebuffer console:** UEFI (OVMF) with
  `-device VGA,xres=1280,yres=800`, plus `usb-tablet` and `usb-kbd` on
  xhci, and `efi_max_resolution` set in loader.conf. Screenshots keep using
  the QEMU monitor's `screendump`.
- **The shell is the compositor's first client**, drawing with tiny-skia
  into shared-memory buffers. The headless renderer stays for tests and
  previews.
- **Unsafe code stays in `glass-platform`'s FFI modules** (ADR 0007): the
  framebuffer ioctls and the mmap.
- **Cross-compiling:** the C libraries (libinput, pixman, libudev-devd,
  libseat, xkbcommon) are extracted from their packages into the sysroot.
  Only the compositor links them.

## Consequences
- The compositor and shell can be seen and screenshotted in the VM, but
  everything is software-rendered and emulated: fine for screenshots and
  tests, slow for animation. Keep the VM at 1280x800 and redraw only what
  changed.
- The DRM path can only be tested on hardware. drm-kmod must match the
  FreeBSD minor release, and has broken on -STABLE before.
- seatd doesn't hand out the framebuffer, so the compositor owns the VT
  (it runs as a session started by the greeter).
- The VM boots UEFI by default: under BIOS (SeaBIOS) the console stays in
  VGA text mode even with `vbe_max_resolution`, so there is nothing to map.
  OVMF's stored boot entries name PCI slots, so the VM starts from fresh
  UEFI variables each time rather than keep entries for a device layout
  that changed.

## What the first step showed (M2a, `glass-shell-fb`)
Run in the VM under UEFI with `-device VGA,xres=1280,yres=800`:

- The console comes up as `VT(efifb): resolution 1280x800`, and
  `FBIOGTYPE`, `FBIO_GETLINEWIDTH` (5120-byte rows) and mmap work on
  `/dev/ttyv0` for the user who owns the terminal; the pixel layout is
  blue lowest (B, G, R, X).
- `KDSETMODE KD_GRAPHICS` stops the console drawing over the shell. Back
  in `KD_TEXT` the console doesn't redraw by itself, and `CONS_CLRHIST`
  would redraw it but erases the visible text, so on exit the shell
  switches to another terminal and back, which redraws it intact.
- devmatch loads hms(4) for `usb-tablet` by itself.
- Drawing the full desktop takes 1.6 to 2 s in the emulated VM (debug
  build); copying the changed rows to the screen takes 10 to 40 ms. A
  clock tick changes 22 rows. Real hardware with an optimized build is
  orders of magnitude faster; the compositor will draw only damaged
  regions.
