# 0011. Branding on a FreeBSD base: overlay now, our own base build next

Status: **Accepted** for the overlay layer; the base build is **Proposed**
and has one decision open (below). PRD BR-1 … BR-5.

## Context
BR-1: users never see FreeBSD: no names, logos, boot messages or strings at
the installer, boot, login, settings, system information, errors, shell
prompts or help, and every system-identity value (OS name and version,
kernel identity, package tools) says Glasshouse. BR-2: open-source notices
are reproduced as each license requires, in Settings → Legal notices. BR-3:
GPL components ship with a source offer. BR-4: the full SBOM goes to
customer security teams under NDA, never published. We build on unmodified
release images (ADR 0002), and "Product name" is still an open question.

## Decision
**Layer 1, the overlay (done, tested by `tests/e2e/check-branding.sh` in
`make e2e-vm`).** Everything we can change without rebuilding the base:

| Surface | How |
|---|---|
| OS identity (`/etc/os-release`) | `rc.d/glasshouse_identity` writes it at boot from the staged `build-info`; the base `os-release` writer is off |
| Login message (`/etc/motd`) | same script; `update_motd=NO` (the base one prepends `uname -v`) |
| Boot loader | `loader_brand="glasshouse"` (`gfx-glasshouse.lua`), no logo or beastie |
| Kernel and rc boot output | `boot_mute=YES`, `rc_startmsgs=NO` |
| Console login banner | `gettytab` `im=` names Glasshouse instead of `%s` |
| Login tips | `fortune freebsd-tips` commented out in skel and existing homes |
| SSH version string | `VersionAddendum none` |
| Our own strings | the sandbox backend is `jail`; user-facing lines name no backend |

**The art (done).** One generator, `tools/branding/art.py`, draws every
screen that shows the mark, from one pixel grid: the boot menu's brand and
logo (ASCII, colored, because the loader can't draw Unicode on a serial
console), the graphical boot splash and shutdown splash (PNG, replacing the
base system's own, which a muted boot would otherwise show), the console's
pre-login screen (`/etc/issue`, which clears the startup messages first)
and the login message. `make lint` fails if the overlay's copies are stale.
The boot menu shows for two seconds, titled "Welcome to Glasshouse"; its
console list must match the firmware (the loader rejects the whole list if
one name is missing), so `rc.d/glasshouse_identity` sets `efi` on UEFI
machines and `comconsole,vidconsole` on BIOS ones. The logo doesn't set
`requires_color`: without color the loader would swap in its own logo.
`tools/screenshot/bootshots.py` records the real boot screens from the VM
(`VM_FIRMWARE=uefi` boots it the way laptops do).

**Notices and SBOM (done).** `tools/legal/notices.py` walks the shipped
binaries' dependency graph and writes `NOTICES.txt` (every crate's license
and texts, identical texts once, plus the base system's `COPYRIGHT`), staged
into `/usr/local/share/glasshouse/legal/` for the Legal notices page. It
fails the build on a license outside `deny.toml`'s allow list. `make sbom`
writes a CycloneDX SBOM to `build/sbom/`, which is never staged or
published (BR-4).

**Layer 2, our own base build (proposed, needed for BR-1 in full).** These
are compiled into the base system and change only with a base build from
source:

- kernel identity: `uname -s`, `kern.ostype`, `uname -v`, the kernel's
  copyright line in `dmesg`;
- loader banners: the loader prints "FreeBSD/amd64 EFI loader" (UEFI) or
  "FreeBSD/x86 bootstrap loader" (BIOS) and its boot details before it
  reads any configuration, and the base rc scripts print their own messages
  before the login screen clears them;
- `freebsd-version`, `freebsd-update` (replaced by our image updates, EP-1)
  and `pkg`'s name and repository (DV-22 brands packages);
- manual pages and other help text that names the system.

This build also produces the read-only signed base image of EP-1 and the
reproducible builds of EP-12, so it's one project, not a branding detour.

**Decision needed: the kernel's reported name.** Setting `kern.ostype` to
Glasshouse satisfies BR-1 literally, but software decides behavior on
`uname -s == FreeBSD`: package ABI strings (`FreeBSD:15:amd64`), configure
scripts, and runtimes that ask at run time (Python's `platform.system()`,
Node's `os.type()`). Either we
carry patches wherever that matters (an ongoing cost on every upgrade), or
we keep the ABI-facing identity and make sure no user-facing surface prints
it. We recommend the second option, with the e2e branding check extended
to every surface users can reach.

## Consequences
- The product name sits in a few files (`os/freebsd/overlay/.../branding/`,
  the `gettytab` edit in `post-install.sh`, `rc.d/glasshouse_identity`), so
  renaming is cheap once it's decided.
- The base system contains copyleft code beyond the BSD notice: OpenZFS
  and DTrace are under the CDDL, and a few imported components carry GNU
  licenses (BR-3). `tools/legal/base_notices.py` reads the pinned release's
  `src.txz` and writes every imported component's license texts and a
  source offer listing the copyleft ones (`os/freebsd/legal/`, checked in,
  regenerated by `make legal-base` when the release changes); both go into
  `NOTICES.txt`. The offer names the unmodified source release by its
  SHA-256. It still needs a legal contact and legal review: `make
  image-release` refuses to build until the contact is set.
- A muted boot still prints some base rc messages (network setup) on the
  serial console; a graphical splash covers them on real hardware.
- BR-5 (contributing upstream through contractors or the Foundation) is a
  process choice and doesn't affect the code.
