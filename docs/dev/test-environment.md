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
| Boot the way laptops do (UEFI) | `make vm-down`, then `VM_FIRMWARE=uefi make vm-up vm-wait` (needs `ovmf`) |
| See what the VM's screen shows | `os/freebsd/scripts/vm.sh screendump shot.png`; `tools/screenshot/bootshots.py --out <dir>` reboots and records every boot screen |

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
| `glassctl: is glassd running?` | `make vm-run CMD="service glassd status; tail /var/log/glassd.log"` |
| `glassd` refuses to start | It validates config and policy first: `make vm-run CMD="glassd --check"` prints why. |
| Containment probe fails in the VM | That's a real finding. Look at the `FAIL` lines in `/var/db/glasshouse/tasks/<id>/agent.log` before anything else. |
| Out of disk | `make clean`, then `make vm-reset` (the VM disk grows to 20 GB). |

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
