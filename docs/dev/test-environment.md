# Setting up a test environment

Everything you need to build Glasshouse, run it, and test it for real in a
FreeBSD VM, from a fresh checkout. One command does the full setup; the
steps below explain what it does and how to fix it when it doesn't.

## What you need

| | |
|---|---|
| Host | Linux x86_64. Ubuntu 24.04 is what we test; other distros work with the same packages. macOS can build and run `make dev-host`, but the VM tooling is Linux-only for now. |
| CPU / RAM | 4+ cores, 8 GB+ RAM (the VM takes 4 GB) |
| Disk | ~15 GB free: FreeBSD downloads and images ~4 GB, sysroot ~0.5 GB, VM disk up to 20 GB sparse, `target/` ~2–4 GB |
| KVM | Optional. With `/dev/kvm` the VM boots in seconds; without it QEMU emulates (TCG) and boots in ~100 s. Everything works either way. |
| Network | download.freebsd.org and crates.io |

## One command

```sh
git clone <repo> glasshouse && cd glasshouse
make test-env
```

That runs, in order (each step is also a make target you can rerun):

1. **Host tools** (`os/freebsd/scripts/check-host-deps.sh`): Rust (via
   `rust-toolchain.toml`, so `rustup` installs the pinned version and the
   FreeBSD target), clang and lld, QEMU, xorriso, an SSH client, shellcheck.
   As root it installs missing packages with apt; otherwise it prints the
   `apt-get install` line to run.
2. **FreeBSD images** (`make freebsd-fetch`, ~1.5 min): the pinned 15.1
   ZFS VM image and `base.txz`, checksum-verified, cached in `build/cache/`.
3. **Sysroot** (`make freebsd-sysroot`, ~20 s): FreeBSD's libraries and
   headers, so we can cross-compile from Linux.
4. **Boot the VM** (`make vm-up vm-wait`, ~100 s without KVM): QEMU with a
   copy-on-write disk over the pristine image. First boot provisions SSH
   for root and a `demo` user (uid 1001) whose home is its own ZFS dataset.
5. **Build and deploy** (`make freebsd vm-deploy`): cross-compiles the
   binaries, installs them with the config and rc.d script, seeds demo
   invoices into `/home/demo`, and starts `glassd`.

Then check it:

```sh
make e2e-vm                 # the full end-to-end suite, ~2 min
make vm-ssh                 # a root shell in the VM
su demo                     # become the demo user
glassctl agents
```

## Daily loop

```sh
# edit code, then:
make test lint              # host checks, seconds
make freebsd vm-deploy      # ~12 s: rebuild (vm profile) and redeploy
make e2e-vm                 # before pushing anything that touches agents
```

The VM keeps running between sessions (until the machine reboots or you run
`make vm-down`). `make vm-status` tells you whether it's up.

### A managed VM

To try what IT does in the admin console on the VM (policy, the catalog,
the Store), enroll it with a development console on this machine:

```sh
make console-dev &                  # the console on http://127.0.0.1:7400
os/freebsd/scripts/enroll-dev.sh    # the VM syncs with it over HTTPS
os/freebsd/scripts/enroll-dev.sh --undo
```

The VM only talks to a console over HTTPS, so the script puts
`tools/dev/console-tls.py` in front of the console (as our web servers do in
production) with a development certificate for 10.0.2.2, the host as the VM
sees it, and tells the VM to trust it. `--undo` takes the VM back to its
shipped settings and default policy; `make e2e-vm` refuses to run on an
enrolled VM. Restart `make console-dev` after changing what devices and
the console say to each other (`glass-managed`): an older console turns a
newer device away.

## Without the VM

For work on the runtime, CLI or agents that doesn't need real containment:

```sh
make dev-host               # glassd on this machine, insecure host sandbox
# in another terminal:
export GLASS_CONTROL_SOCKET=$PWD/build/dev/run/control.sock
target/debug/glassctl agents
```

Nothing is contained in this mode (`glassctl status` says so in red), and
the containment probe is expected to fail there.

## Resetting

| To | Run |
|---|---|
| Restart glassd | `make vm-run CMD="service glassd restart"` |
| Start the VM from a clean first boot | `make vm-reset vm-up vm-wait vm-deploy` |
| Rebuild everything from scratch | `make clean` (keeps the FreeBSD downloads) |
| Bake a bootable image of the current build | `make image` (`make image-release` for a shippable one) |
| Build the image from its package list instead (pkgbase, ADR 0022) | `make image-pkgbase` (about 9 min, packages cached in `build/cache`): `build/images/glasshouse-<ver>-amd64-desktop.qcow2`, exactly the packages in `os/freebsd/images/desktop.pkgs`, plus its inventory beside it. Boot it as a second VM: `VM_BASE_IMAGE=$PWD/build/images/glasshouse-15.1-amd64-desktop.qcow2 VM_DIR=$PWD/build/vm-desktop VM_SSH_PORT=2224 os/freebsd/scripts/vm.sh up`, then `vm.sh wait` and `tests/e2e/run-vm.sh` with the same variables. Each worktree builds into its own `build/`; `GLASS_E2E_BIN`, `CARGO_TARGET_DIR` and `SHELL_TARGET_DIR` let it use another checkout's binaries |
| Boot with a BIOS text console instead of UEFI | `make vm-down`, then `VM_FIRMWARE=bios make vm-up vm-wait`. UEFI (the default, `ovmf`) gives the 1280x800 framebuffer the shell draws on (`VM_SCREEN`, ADR 0016) |
| Put the desktop shell on the VM's screen | `make shell-vm`, then `os/freebsd/scripts/vm.sh screendump shot.png`; `os/freebsd/scripts/shell-vm.sh stop` gives the console back |
| Boot into the sign-in screen, as a machine does | Every `make freebsd vm-deploy` turns it on (password `demo`); `make greeter-vm` puts debug builds in; Shift Super Q (`vm.sh key shift-meta_l-q`) signs out; `os/freebsd/scripts/shell-vm.sh greeter-off` gives `ttyv0` back to the console login |
| Drive the screen step by step, each step waiting for the redraw | `os/freebsd/scripts/desk.sh signin`, `click <x> <y>`, `key <keys>`, `shot <png>`, `signout`, `log` |
| See only the summary of a long run | `tools/quiet make e2e-vm` (failures with context; the whole output in `build/logs/`) |
| Free disk, resume a VM paused by a full disk | `tools/cleanup.sh` (report), `--apply` (the `cleanup` skill) |
| Click, press keys or type on the VM's screen | `os/freebsd/scripts/vm.sh click <x> <y>`, `vm.sh point <x> <y>`, `vm.sh key ctrl-spc`, `vm.sh type "text"` (QEMU's USB tablet and keyboard, through QMP and the monitor) |
| See what the VM's screen shows | `os/freebsd/scripts/vm.sh screendump shot.png`; `tools/screenshot/bootshots.py --out <dir>` reboots and records every boot screen |
| See where boot time goes | `make vm-boot-timeline` (`tools/boot/timeline.py`): power-cycles the VM three times and prints each phase, firmware to login prompt and glassd ready. `TRACE=1` adds the kernel's milestones and each rc script's time through boottrace(4), which slows every script down; `RUNS=1` for one boot. See docs/dev/performance.md, "Boot" |
| Run a second VM next to the default one | Give it its own directory and port: `VM_DIR=$PWD/build/vm2 VM_SSH_PORT=2223 VM_CPUS=2 VM_MEM=3072` before every `vm.sh` and `make vm-*` command. It shares the read-only base image and has its own disk |
| Measure the desktop's responsiveness with busy agents | `su demo -c '/usr/local/libexec/glasshouse-tests/glass-e2e load'` in the VM, or start one yourself: `glassctl run --yes containment-probe "load for 30s"` keeps every CPU busy and writes as fast as its limits allow |

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| VM panics during boot, mentions ZFS or SIMD | QEMU 8.2's emulator mis-handles AVX. The scripts already default to `-cpu max,avx=off,...` under TCG; if you set `VM_CPU_MODEL`, keep AVX off. |
| `vm-wait` times out | First boot without KVM takes up to ~2 min. `make vm-console` shows the serial console. |
| A reboot inside the VM hangs after "Loading configured modules" (QEMU at 100% of one core) | An occasional warm-reboot hang with several emulated CPUs. Power-cycle: `os/freebsd/scripts/vm.sh down && os/freebsd/scripts/vm.sh up && os/freebsd/scripts/vm.sh wait`. Cold boots haven't hung; `VM_CPUS=2` avoids it at the cost of a slower e2e suite. |
| `vm.sh run` hangs right after the VM rebooted | The shared SSH connection still points at the old boot: `os/freebsd/scripts/vm.sh ssh-close`. |
| The console shows nothing while the VM boots | Kernel output is muted for branding (BR-1). To debug boot: `make vm-run CMD="sysrc -f /boot/loader.conf boot_mute=NO"`, then reboot the VM. |
| `vm.sh run 'service x start'` never returns | A daemon inherited the SSH session's output. Redirect it: `service x start >/dev/null 2>&1`. (`vm-deploy` already does this.) |
| `linker x86_64-unknown-freebsd-cc not found` / no sysroot | Run `make freebsd-sysroot`. |
| `make image-pkgbase`: "installed but not in desktop.pkgs" or "missing libraries" | The list must name every package the image gets: add the dependencies it names (with what needs them), or drop the package that pulls them in. |
| `make image-pkgbase`: "the base package catalog ... is X, release.env pins Y" | FreeBSD published a new patch level and your cache had no catalog yet. Review the new versions, then set `FREEBSD_PKGBASE_CATALOG_SHA256` (`os/freebsd/scripts/fetch.sh --print-pins`). |
| `glassctl: is glassd running?` | `make vm-run CMD="service glassd status; tail /var/log/glassd.log"` |
| `glassd` refuses to start | It validates config and policy as it starts and says why in `/var/log/glassd.log` (and on the console at boot); `make vm-run CMD="glassd --check"` prints it too. A policy file that doesn't validate doesn't stop it: it enforces the last good policy and `glassctl status` says so (ADR 0021). |
| Containment probe fails only "limits: below the person's priority, memory capped" right after a deploy | The VM hasn't restarted since the deploy turned resource accounting on (`kern.racct.enable`, read at boot): `vm.sh down && vm.sh up && vm.sh wait`. `check-boot.sh` in the e2e suite says the same. |
| Containment probe fails in the VM | That's a real finding. Look at the `FAIL` lines in `/var/db/glasshouse/tasks/<id>/agent.log` before anything else (the log is there until the task is dismissed, and for a week after a restart). |
| Out of disk | `make vm-run CMD="zpool trim zroot"` gives the space ZFS freed back to the host (the VM's disk is sparse; deploys turn `autotrim` on so it stays small). Then `make clean`, or `make vm-reset` (the VM disk grows to 20 GB). If the host disk filled while the VM was writing, QEMU pauses it: `info status` on its monitor says `paused (io-error)`; free space, then `cont`. |

## In Claude Code (web or cloud)

Cloud sessions start from a fresh container. The repo's SessionStart hook
(`.claude/hooks/session-start.sh`) installs the host tools and FreeBSD Rust
target, so `make test lint` works immediately; run `make test-env` when the
task needs the VM. The project skills in `.claude/skills/` describe the
workflows (test environment, manual testing with screenshots, adding actions
and agents, syncing with the PRD).

## Sharing what you tested

`tools/screenshot/termshot.py` runs commands (on the host or in the VM as
any user) and saves their real, colored output as a PNG, so reviews can see
exactly what was run. See `.claude/skills/manual-test/SKILL.md`.
