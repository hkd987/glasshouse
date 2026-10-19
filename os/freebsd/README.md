# FreeBSD VM dev loop

Cross-compile glasshouse for FreeBSD on a Linux host and run it in a local FreeBSD 15.1 VM
(QEMU, ZFS root, provisioned with cloud-init via nuageinit).

## Layout

| Path | Purpose |
|---|---|
| `release.env` | Pinned release: version, mirror URLs, image names, SHA256s. Everything reads it. |
| `os.mk` | Make targets, included from the top-level Makefile. |
| `scripts/lib.sh` | Shared paths, VM knobs, logging, SSH helpers. |
| `scripts/fetch.sh` | Downloads, verifies and caches the VM image and `base.txz`, then decompresses the image. |
| `scripts/sysroot.sh` | Extracts `lib/`, `usr/lib/` and `usr/include/` from `base.txz` into `build/freebsd/sysroot/`. |
| `scripts/vm.sh` | `up` `down` `status` `wait` `ssh` `run` `scp-in` `exec` `reset` `console-log` |
| `scripts/stage-dist.sh` | Builds the staging root `build/dist/freebsd/` from `overlay/` plus `dist.manifest`. |
| `scripts/deploy.sh` | Installs the staging root into the running VM, then runs the post-install hook and restarts glassd. |
| `scripts/bake-image.sh` | Bakes a fresh VM (the official image, or `VM_BASE_IMAGE`) with the payload deployed into a flat qcow2 in `build/images/`. |
| `images/<image>.pkgs` | Package lists: the base packages each image is made of (ADR 0022). `desktop.pkgs` is the desktop's. |
| `scripts/bake-pkgbase.sh` | Builds an image from a package list: the pinned repository, a build VM running `pkgbase-root.sh`, then the bake. |
| `scripts/pkgbase-root.sh` | Runs in the build VM: partitions a new disk, makes a ZFS pool, installs exactly the list with `pkg --rootdir`, boot code, inventory. |
| `scripts/pkg-mirror.py` | Serves the pinned base repository to the build VM from `build/cache`, downloading each file once. |
| `scripts/check-host-deps.sh` | Checks host tools (`--install` runs apt-get). |
| `toolchain/x86_64-unknown-freebsd-cc` | clang wrapper (`--target`, `--sysroot`, lld). `-c++` is a symlink to it. Cargo links with it, and openssl-sys's build script reads the base system's OpenSSL headers through it (`.cargo/config.toml`, ADR 0018), so FreeBSD checks need the sysroot too. A git worktree without its own sysroot uses the main checkout's. |
| `vm/user-data.in` | cloud-config template for first boot. |
| `dist.manifest` | `<binary> <install-path> <mode>`: which cargo binaries go where. |
| `overlay/` | Non-binary payload (rc.d scripts, configs, `post-install.sh`), laid out like the guest's `/`. |

Build outputs (all under the gitignored `build/`):

- `cache/freebsd/<release>/` holds downloads and the pristine base disk, and survives `make clean`.
  Its `pkgbase/` is the mirrored base package repository: the catalog and every package an image
  build has used.
- `freebsd/sysroot/` is the cross-compile sysroot.
- `vm/` holds `disk.qcow2` (overlay), `seed.iso`, `ssh/id_ed25519` (dev key), `console.log` and `qemu.pid`.
- `dist/freebsd/` is the staging root.
- `images/` holds baked images, and beside each package-list image its inventory (`*.packages.txt`).

## Host dependencies (Ubuntu 24.04)

```sh
apt-get install -y qemu-system-x86 qemu-utils openssh-client xorriso xz-utils curl clang lld shellcheck
rustup target add x86_64-unknown-freebsd   # rust-toolchain.toml already lists it
os/freebsd/scripts/check-host-deps.sh       # or: make freebsd-host-deps
```

## Dev loop

```sh
make freebsd-fetch freebsd-sysroot   # once per release (about 2 min, cached)
make vm-up vm-wait                   # first boot about 100 s under TCG
make freebsd                         # cross-compile (fast `vm` profile; ~3 s after an edit)
make vm-deploy                       # stage + deploy, about 8 s
make vm-run CMD="glassctl status"    # or: make vm-ssh
make vm-reset                        # throw the disk away; the next vm-up is a fresh first boot
```

Other useful commands:

- `os/freebsd/scripts/vm.sh exec <host-binary> [args]` copies a binary into the VM and runs it. It also works as a cargo runner.
- `vm.sh console-log -f` follows the serial console.
- `vm.sh scp-in <files...> <dest>` copies files into the VM.

SSH is `root@127.0.0.1:2222` with `build/vm/ssh/id_ed25519`. Host keys are never recorded.

VM knobs (environment variables): `VM_CPUS` (host cores, at most 4), `VM_MEM` (4096), `VM_DISK_SIZE` (20G),
`VM_SSH_PORT` (2222), `VM_BOOT_TIMEOUT` (900 s), `VM_CPU_MODEL`, `VM_DIR` (run a second VM),
`VM_BASE_IMAGE` (boot a baked image instead of the stock one), `VM_EXTRA_DISK` (a second qcow2 the
guest sees as `vtbd2`; the package-list build writes the new image there).

## Deploy contract

1. `stage-dist.sh` wipes `build/dist/freebsd/` and copies `overlay/` into it.
2. It then installs each `dist.manifest` binary from
   `${CARGO_TARGET_DIR:-target}/x86_64-unknown-freebsd/${DIST_PROFILE:-release}/` (the shell's
   from `${SHELL_TARGET_DIR:-shell/target}/...`). If any binary
   is missing, it fails and lists them all. Through `make`, `DIST_PROFILE` follows
   `FREEBSD_PROFILE` (`vm` by default; `make image-release` uses `release`).
3. `deploy.sh` tars the staging root (files and symlinks only, owned by root:wheel), extracts it at
   `/` in the guest, and deletes files that a previous deploy installed but the payload no longer
   contains. The list is kept in `/var/db/glasshouse-deploy.files`.
4. It runs `/usr/local/libexec/glasshouse/post-install.sh` if present.
5. If `/usr/local/etc/rc.d/glassd` exists **and** `glassd_enable=YES`, it runs `service glassd restart`.
   Have post-install run `sysrc glassd_enable=YES`.
6. Re-running is safe. Running binaries are replaced, because tar unlinks and recreates each file.
7. New directories are created 0755. Set special directory modes in `post-install.sh`.

## Provisioning (first boot, `vm/user-data.in`)

- Root gets SSH login with the dev key (`PermitRootLogin prohibit-password`).
- Password SSH is off.
- `demo` is uid 1001 with shell `/bin/sh`, no password, and home `/home/demo` on its own dataset
  `zroot/home/demo`.
- `firstboot_pkg_upgrade` is disabled so the VM stays on the pinned release.
- The overlay is 20G. The image's `growfs` rc script grows the GPT partition and the pool on first
  boot (`zroot` is about 18.5G).
- The seed ISO is only read on first boot. After editing `user-data.in`, run `make vm-reset`.

## Package-list images (pkgbase, ADR 0022)

```sh
make image-pkgbase     # build/images/glasshouse-15.1-amd64-desktop.qcow2 + .packages.txt
VM_BASE_IMAGE=$PWD/build/images/glasshouse-15.1-amd64-desktop.qcow2 VM_DIR=$PWD/build/vm-desktop \
  VM_SSH_PORT=2224 os/freebsd/scripts/vm.sh up    # then wait, run, and tests/e2e/run-vm.sh
```

Instead of editing the official VM image, `bake-pkgbase.sh` installs exactly the packages in
`images/desktop.pkgs` into a new root, then bakes it like `bake-image.sh`:

1. `pkg-mirror.py` serves FreeBSD's `base_release_1` (`FREEBSD_PKGBASE_REPO`) from
   `build/cache/freebsd/<release>/pkgbase/` on a free local port; the build VM reaches it at
   `10.0.2.2`. A file it doesn't have yet it downloads once (streaming it on, and resuming a cut
   transfer), so reruns download nothing. The catalog must match `FREEBSD_PKGBASE_CATALOG_SHA256`.
2. A build VM (the official image, `BAKE_VM_DIR`/`BAKE_SSH_PORT`) gets a new empty disk
   (`VM_EXTRA_DISK`, `IMAGE_DISK_SIZE` 6G): `pkgbase-root.sh` makes EFI, BIOS boot, swap and ZFS
   partitions, a pool `zroot` (imported as `glassimage` there), installs the list with
   `pkg --rootdir`, writes rc.conf, loader.conf and fstab, checks every program's libraries,
   installs boot code and the inventory, trims and exports.
3. `bake-image.sh` boots that disk (`VM_BASE_IMAGE`): first boot provisions from the seed, the
   payload is deployed with `post-install.sh`, and the disk is flattened.

Changing the list: add or remove lines in the `.pkgs` file. The build fails if pkg installs a
package the list doesn't name (add it, with what needs it) or a program misses a library. Never
add `-dbg`, `-dev`, `-lib32`, `-tests` or toolchain packages (BS-3).

Differences from the official image besides the packages: 15.1-p5 rather than p0, lz4
compression and no atime on the pool, no `/usr/src`, `/usr/ports` or `/usr/obj` datasets, no
config-drive partition, and the `FreeBSD-base` repository left disabled.

## Timings (4-core container, no KVM, QEMU 8.2 TCG)

| Step | Time |
|---|---|
| Download (830 MB) + verify + decompress | about 1.5 min |
| `sysroot.sh` | 20 s (471 MB) |
| Fresh `vm-up` to `vm-wait` done (first boot plus provisioning) | 97–103 s |
| Warm reboot to SSH | about 87 s |
| `vm-down` (clean poweroff) | about 26 s |
| `vm-deploy` (2.4 MB payload, restart glassd) | about 8 s |
| `bake-image.sh` (fresh boot, deploy, poweroff, zlib flatten to 961 MB) | about 4 min 10 s |
| Boot a baked image (`VM_BASE_IMAGE=...`) to SSH | about 82 s |
| `bake-pkgbase.sh`, packages cached (build VM boot, 62-package install, bake, flatten to 157 MB) | about 9 min |
| The same, first run | plus downloading the desktop list's 82 MB of packages, once |
| Boot the package-list image to SSH | 47–62 s |
| Cold boot to the login prompt (`timeline.py`, median of 3): official-image bake / package list | 56.5 s / 50.0 s |
| `vm.sh exec` of a small Rust binary | about 4.5 s |

## Quirks

- **No AVX under TCG.** QEMU 8.2 mis-emulates AVX/AVX2: with `-cpu max` the 15.1 kernel panics in
  `zfs.ko` SIMD init. The TCG CPU model is `max,la57=off,avx=off,avx2=off`. Turning AVX off also
  keeps userland that picks AVX2 at runtime (memchr and friends) off the buggy path.
- **KVM is picked automatically** when `/dev/kvm` is writable (`-accel kvm -cpu host`). This path
  is untested because the dev container has no KVM. Override with `VM_ACCEL` and `VM_CPU_MODEL`.
- **Daemons started over SSH hold the session open.** A daemon inherits the SSH channel's stdout
  and stderr, so `vm.sh run 'service x start'` hangs. Redirect its output
  (`</dev/null >/dev/null 2>&1`). `deploy.sh` already runs its hooks this way.
- **Rust in jails needs `/dev/null`.** `Command::output()` and `Stdio::null()` open it, so give
  jails devfs (`mount.devfs devfs_ruleset=4`).
- **Minimal nullfs jail.** For base or Rust-std binaries, read-only `/lib` plus `/libexec` (rtld)
  plus the binary's directory is enough. `/usr/lib` isn't needed.
- **nuageinit (15.1) has no `uid` key** and runs `users` after the network comes up. That's why
  `demo` is created in `runcmd`.
- **No package installs in the guest here.** The image is pkgbase-based. Guest DNS works, but this
  sandbox intercepts outbound HTTPS, so `pkg install` fails unless the proxy CA is trusted in the guest.
- **Baked images are dev images.** They contain the dev SSH key for root and the `demo` user.
- **Host tar warnings.** GNU tar warns about `SCHILY.fflags` in FreeBSD tarballs; `sysroot.sh`
  silences this.
- **C/C++ build dependencies.** Crates that compile C or C++ (via the cc crate) need
  `CC_x86_64_unknown_freebsd` and `CXX_x86_64_unknown_freebsd` pointing at
  `toolchain/x86_64-unknown-freebsd-cc` and `-c++`, for example in an `[env]` table with
  `relative = true`.

## Bumping FreeBSD

1. Edit `FREEBSD_VERSION`, `FREEBSD_RELEASE` and `FREEBSD_TRIPLE` in `release.env`, and the
   major version and `base_release_<minor>` in `FREEBSD_PKGBASE_REPO`.
2. Run `scripts/fetch.sh --print-pins` and paste the printed values into `release.env`.
3. Run `make freebsd-fetch freebsd-sysroot vm-reset vm-up vm-wait`.
