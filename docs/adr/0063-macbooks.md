# 0063. MacBooks

Status: **Proposed** (2026-10-25); the owner decides. The owner: "What
would it take to get things working on intel and M type MacBooks? We
should see if that's possible."

## Context
- V1 has one target, `x86_64-unknown-freebsd` (`docs/architecture.md`).
  The pilot runs on laptops 15.1 already drives (ADR 0057).
- What the base system makes of Macs, as of October 2026:

  | Macs | The base system today | What it would take |
  |---|---|---|
  | Intel, 2013–2015 | Mostly works: graphics, keyboard, trackpad, brightness, sleep. Audio needs hints | Wi-Fi: Broadcom chips with no driver in the base system |
  | Intel, 2016–2017 | The keyboard and trackpad are on SPI: `atopcase` drives them, in review, wanting testers | `atopcase` checked on the machines, and the same Wi-Fi work |
  | Intel with the T2 chip, 2018–2020 | Keyboard, trackpad and audio go through the T2. No driver | Porting Linux's out-of-tree T2 drivers: months |
  | Apple Silicon (M1 to M5) | Doesn't start: no drivers | Not realistic. Asahi Linux, a team of its own, took years over M1 and M2; M3 arrived in September 2026 and M4 is under way |

- Broadcom Wi-Fi has two ways in: `wifibox` (Linux in a small VM, passing
  the card through) and a native `brcmfmac` port, one of them written by an
  AI agent in February 2026, which scans, joins and does WPA2 on a 2016
  MacBook Pro and is called experimental. Our kernel patches (ADR 0057)
  and the agents that write them are made for this kind of work.
- On Apple Silicon, the base system runs in a virtual machine under macOS
  (Apple's Virtualization framework, UTM), as an arm64 guest.

## What we checked (2026-10-25)
- **Our code builds for ARM.** All of the image's programs, the desktop
  shell included, built for `aarch64-unknown-freebsd` with no code
  changes. Rust has that target in tier 3, so the build needs a nightly
  compiler that builds the standard library itself
  (`-Zbuild-std=std,panic_abort`). The sysroot is the arm64 base system's
  plus its xkbcommon and pixman packages.
- **x86-only parts:** the Intel watchdog handling (`ichwd`), the kernel
  patch build script's target, and the dev VM scripts. None is deep.
- **It runs.** FreeBSD 15.1 arm64 under QEMU's emulated ARM (`-M virt`,
  UEFI), with the ARM build installed by `post-install.sh` as on x86:
  - the sign-in banner and branding on the console;
  - glassd running with the jail sandbox, agents' resource limits on;
  - the end-to-end scenarios, as on x86 (`glass-e2e all`, as demo), the
    containment probe passing all 30 checks. Results below.
- **An emulator trap.** With QEMU's `-cpu max` (every ARM feature, MTE,
  SVE, SME and MOPS among them), files copied in arrived with runs of
  zeros replaced by garbage: 3,825 bytes of an 18 MB program, and a
  232 MB archive. Each file read back the same every time, and ZFS saw no
  error. As a Neoverse-N1, an ordinary ARMv8.2 server core, every copy
  matched. Emulated ARM tests use a real CPU's model.

## Doing it again
The check's files were scratch and are gone. To build and run it again:
1. **A sysroot:** the arm64 base system's `base.txz` for the release
   `release.env` pins, with the arm64 `libxkbcommon` and `pixman` packages
   unpacked into it.
2. **A compiler wrapper:** `clang --target=aarch64-unknown-freebsd15.1
   --sysroot=<sysroot> -fuse-ld=lld`, with `-L`/`-I` for the sysroot's
   `usr/local`, as `CC_aarch64_unknown_freebsd` and the target's linker.
   OpenSSL's headers and libraries come from `os/freebsd/toolchain`
   (`AARCH64_UNKNOWN_FREEBSD_OPENSSL_{INCLUDE,LIB}_DIR`).
3. **The build:** `cargo +nightly build -Zbuild-std=std,panic_abort
   --profile vm --target aarch64-unknown-freebsd`.
4. **The install tree:** `DIST_PROFILE=vm` stage-dist's tree with the x86
   programs swapped for these, copied in and set up by
   `post-install.sh`.
5. **The computer:** the base system's arm64 VM image under
   `qemu-system-aarch64 -M virt -cpu neoverse-n1` with UEFI (edk2's
   aarch64 firmware), virtio disk and network, and a restart once
   installed, for resource limits.

## Decision (recommended)
1. **Apple Silicon: Glasshouse as a virtual machine on macOS**, not on the
   metal. The ARM build makes it possible. Inside, agents are contained,
   visible and undoable as anywhere. But macOS owns the hardware and can
   see everything the VM does: it's the Glasshouse workspace, not the
   Glasshouse computer. For a person who must keep a Mac, or to try
   Glasshouse, not for the managed fleet the PRD describes.
2. **Intel Macs, 2013–2017: worth a pilot after the PC pilot**, Wi-Fi the
   main work: `wifibox` first, since it works today, then a native driver
   as kernel patches. 2016–2017 models also need `atopcase` checked.
3. **T2 Macs (2018–2020): not supported.** Months of driver work for
   machines Apple stopped selling in 2020.
4. **V1 stays x86-only.** ARM becomes a target only with (1): a pinned
   nightly, an arm64 image (pkgbase), CI building and testing it on an ARM
   runner or emulator, and the VM packaged for macOS.

## Results (2026-10-25)
On the emulated ARM computer (3 cores, 3 GB, emulated on x86, so slow),
`glass-e2e all` as demo: 13 of 15 scenarios passed.
- **Passed:** scope, containment (30/30 checks), stop, decline, labels,
  takeover, harness, changes, it, invoices, tailor, perf and load.
- **Failed, for want of an ARM build of a tool**, not of our code:
  - plan: the test's stand-in for Claude Code (`glass-fake-claude`) wasn't
    built for ARM;
  - studio: App Studio's bundler is esbuild, a Go program the image
    carries for x86. esbuild publishes an arm64 build for this OS.
- After the run, no jail was left behind.

## Consequences
- Nothing changes in V1 until the owner picks (1), (2) or neither.
- (1) costs a second architecture everywhere: images, OS updates and
  their deltas, kernel patches (per architecture), CI, and the store's
  native code. A tier-3 compiler target can break with any nightly.
- (2) is hardware work of the kind ADR 0057 set up for, on machines that
  are 8 to 12 years old: their batteries and Apple's end of support for
  them are the pilot's real limits.
- Sources: the Asahi Linux progress reports (August 2026), the base
  system's 15.1 hardware notes and Q1 2026 LinuxKPI wireless report,
  `atopcase` (review D39863), t2linux's device support page, and the
  `brcmfmac` port's notes (February 2026).
