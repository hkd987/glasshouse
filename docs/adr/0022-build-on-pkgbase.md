# 0022. Build on pkgbase: images are package lists, updates are packages

Status: **Accepted** (the PRD makes it a core decision; the work is
staged below). PRD BS-1 to BS-6, PL-1, PL-2. Builds on ADRs 0001, 0007,
0011.

## Context
FreeBSD's packaged base (pkgbase) ships as a technology preview in 15 and
replaces freebsd-update in 16. The PRD commits us to it: a thin fork of
documented patches (BS-1), rebuilding only the base packages we change and
re-signing FreeBSD's for the rest into our own repository (BS-2), each
image a list of packages from that repository with no compilers, test
suites, debug files or 32-bit libraries (BS-3), whole signed images to
devices through boot environments (BS-4), an advisory-to-release pipeline
(BS-5), and every component a versioned package for inventory (BS-6).

Where we stand:

- The dev VM and `make image` start from the official 15.1 VM image, whose
  base is already pkgbase: 499 `FreeBSD-*` packages, debug packages
  included. Its `FreeBSD-base` repository is configured but disabled, so
  nothing updates it, and nothing removes what images don't need.
- Our binaries, overlay and branding are staged as files (`stage-dist.sh`)
  and unpacked over that image, not packaged.
- The cross-compile sysroot and the base notices come from `base.txz`.

## Decision
1. **An image is a package list.** `os/freebsd/images/<image>.pkgs` names
   the packages of each product image (desktop first; the developer
   workspace host and the cloud agent computer later). The image build
   installs exactly those into a fresh root (`pkg --rootdir`) on a new ZFS
   pool, rather than editing the official image, so nothing unlisted
   ships: no `-dbg`, `-tests`, toolchain or `lib32` packages.
2. **One repository.** Images install only from our repository. Until it
   exists it's a pinned mirror of FreeBSD's `base_release_N` for the
   release we ship; then FreeBSD's packages are re-signed into it with our
   key (BS-2), next to the base packages we rebuild.
3. **Our code ships as packages too:** `glasshouse-*` packages (glassd and
   the tools, the shell, the overlay and branding), built by our pipeline,
   so the image list and the inventory (BS-6) cover everything and a fix
   to one of them is one package.
4. **Base changes are a patch set** on FreeBSD's stable branch (BS-1):
   only the packages it touches are rebuilt (the kernel configuration,
   branding the overlay can't reach, security defaults). Until the first
   patch lands we ship FreeBSD's binaries unmodified.
5. **Devices never run `pkg` against the base.** Updating an untouched
   module means taking FreeBSD's newer package into our repository and
   building, testing and signing a new image; devices get the whole image
   in a new boot environment (BS-4, EP-1). That's what makes untouched
   modules quick: no rebuild, just a new list resolution and the image
   tests.
6. **The sysroot and notices come from the same packages** (`FreeBSD-clibs`
   and friends, and each package's license metadata) instead of `base.txz`,
   so what we compile against and what we credit is what we ship.
7. **Portability stays in one crate (PL-1, PL-2).** FreeBSD facilities
   (jails, ZFS, Capsicum, framebuffer, evdev, PAM, rctl) are reached only
   through `glass-platform`; everything above it builds and tests on Linux
   (`make test` on the Linux host), and `make lint` refuses
   `target_os = "freebsd"` code outside it.

## Consequences
- The first slice is item 1 with a mirrored FreeBSD repository: a
  package-list image that boots into sign-in and passes `make e2e-vm`,
  measured against today's image for size and boot time (fewer files to
  load and check helps the boot budget).
- Item 3 changes `stage-dist.sh` and `deploy.sh` into package builds; the
  dev loop must stay as fast as `make freebsd vm-deploy` is now.
- Our own repository, signing keys and the advisory pipeline (BS-5) need
  release infrastructure that doesn't exist yet.
- FreeBSD 16 makes pkgbase the only way; nothing here has to be undone
  then.
