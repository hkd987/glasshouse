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
  included. The image enables its `FreeBSD-base` repository (our
  provisioning only turns off its first-boot upgrade), and nothing removes
  what images don't need.
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

## What's built (item 1, first slice)
`make image-pkgbase` (`os/freebsd/scripts/bake-pkgbase.sh`) builds
`build/images/glasshouse-<ver>-amd64-desktop.qcow2` from
`os/freebsd/images/desktop.pkgs`:

1. **The repository** is FreeBSD's `base_release_1` for 15.1, served to the
   build from `build/cache/freebsd/<release>/pkgbase/` by
   `scripts/pkg-mirror.py`, which downloads each file once (resuming a cut
   transfer) and then only ever serves its copy. `release.env` pins the
   catalog's SHA256 and the build checks it first; pkg checks the catalog's
   signature (the base system's `pkgbase-15` keys) and every package's
   checksum against it. FreeBSD updates `base_release_N` in place with
   each patch level, so the cache is the only copy of a pinned catalog's
   packages until our own repository exists (item 2): a fresh cache after
   FreeBSD moves on fails the build until the new catalog is reviewed and
   pinned. The pinned catalog is 15.1-p5 (kernel and runtime p5; the
   official VM image is 15.1-p0).
2. **The root**: a throwaway build VM (the official image) gets a new
   empty disk; `scripts/pkgbase-root.sh` partitions it (EFI, BIOS boot,
   swap, then ZFS last so `growfs` can grow it), creates a pool `zroot`
   (imported as `glassimage` beside the VM's own) with bsdinstall's
   defaults (lz4 compression, no atime), and runs `pkg --rootdir` with
   exactly the list. The build fails if pkg installs anything the list
   doesn't name, or if any program in the image is missing a library, so
   the list is the whole truth: 62 packages, every dependency listed with
   what needs it. The ESP holds only `EFI/BOOT/BOOTX64.EFI` (no
   directory names the base system, BR-1). The pkg repository catalog and
   downloads don't stay in the image.
3. **The bake** is `bake-image.sh` on that disk: first boot (growfs, a new
   pool id, the VM seed's provisioning), deploy of the staged payload and
   `post-install.sh` (branding, loader.conf, services), power off,
   flatten.
4. **Inventory (BS-6)**: the image carries `pkg query '%n %v'` in
   `/usr/local/share/glasshouse/packages.txt` (root only), a copy goes
   beside the image, and `make sbom` lists those packages as the SBOM's
   base components.

Excluded: every `-dbg`, `-dev`, `-lib32` and `-tests` package, the
toolchain (clang, lld, lldb, toolchain), sources and manual pages, and
what the desktop doesn't use today (rescue, bhyve, dtrace, pf, OpenBSM's
auditd, bluetooth, sound, NFS, ppp, csh, ...). Development-only and
marked so in the list: sshd and Kerberos libraries (the dev loop and e2e
suite use SSH) and nuageinit with flua (first-boot provisioning from the
VM seed); a product image drops them.

Measured against today's image, both baked by this repo the same way
(4-core container, QEMU TCG, no KVM; boot times are medians of three
`tools/boot/timeline.py` runs each, alternating the two images on a shared
host, so take them as a direction: the kernel and startup scripts phase is
where the difference is, 44 s against 51 s). The package-list image
boots with UEFI and with BIOS.

| | Today (`make image`) | Package list (`make image-pkgbase`) |
|---|---|---|
| Image file (compressed qcow2) | 978 MiB | 157 MiB |
| Base packages installed | 501 (2428 MiB) | 62 (342 MiB; 206 MiB on disk with lz4) |
| Cold boot to login prompt | 56.5 s (53.2–58.3) | 50.0 s (44.9–58.0) |
| Cold boot to glassd ready | 55.7 s | 49.6 s |
| e2e suite | passes (the dev VM, same base) | passes (containment 20/20) |
| Build | ~4 min | ~9 min with packages cached (two VM boots) |

Not yet: images still install FreeBSD's signed packages (no repository or
key of ours, item 2); our code ships as files over the root, not
packages (item 3); `make image` stays on the official image until the
package-list one has been used for a while. The dev VM (`make vm-up`)
also still starts from the official image. `periodic security` runs
`pkg audit`, which fetches FreeBSD's vulnerability list from the network;
devices shouldn't (advisories are our pipeline's job, BS-5), so a product
image turns it off.
