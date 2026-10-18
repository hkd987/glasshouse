# Keeping the OS fast for the people using it

Build speed is in [build-performance.md](build-performance.md). This page is
about the other kind: how long a person waits on Glasshouse.

## The budgets

`glass-e2e perf` (part of `make e2e-vm`) measures each of these on a live
glassd in the dev VM and fails when a 95th percentile goes over its budget:

| What the person waits for | Budget (p95) | Source | VM today (p50 / p95) |
|---|---|---|---|
| Handoff card (`task.plan`) | 200 ms | ceiling | 3 / 4 ms |
| Tiles (`task.list`) | 100 ms | ceiling | 3 / 4 ms |
| Expanded view (`task.get`) | 200 ms | AW-10 | 4 / 4 ms |
| Take over | 1 s | AW-20 | 203 / 245 ms |
| An action that needs no snapshot (reads, lookups) | 100 ms | ceiling | 3 / 12 ms |
| An action that snapshots first (writes) | 750 ms | ceiling | 243 / 391 ms |
| Start to the agent's first step | 4 s | ceiling | 1.7 / 2.6 s |
| Undo preview (`task.undo` dry run) | 150 ms | ceiling | 6–9 / 58–94 ms (0.8–1.3 s before ADR 0017) |

The VM emulates its CPU (no KVM in the cloud containers), so it's several
times slower than a laptop, and starting a process alone costs ~70 ms in it.
Read the table this way:

- **PRD targets** (AW-10, AW-20) must hold even in the VM, which leaves room
  to spare on real hardware.
- **Ceilings** are about 1.5× what the VM measures today (the start of a
  range when it varies run to run). They're there to catch a change that
  makes something slower. When you make something
  faster, lower its ceiling in `tests/e2e/src/perf.rs` and update this table.

Action times come from the audit trail: every action record carries
`detail.elapsed_ms`, the OS's time from the agent's call to the outcome
(`crates/svc/runtime/src/execute.rs`). For an Ask it includes the person's
decision, so the perf scenario leaves Asks out.

## Where the time goes

- **Snapshots.** Every write takes a ZFS snapshot first (AU-20), and so do
  task start and take over. A snapshot is one `zfs snapshot` call: the
  snapshotter remembers which dataset holds each granted folder (checked
  against the folder's device), so it doesn't run `zfs list` per folder
  each time. That cut take over from 355 to 203 ms and writes from 367 to
  243 ms in the VM.
- **Undo** reads only the files the task changed (ADR 0017): a few `stat`
  calls per file, and a read of the file and its snapshot copy when their
  metadata differs. It used to ask `zfs diff`, which walks every change in
  the whole home dataset since the snapshot and took 0.8 to 1.3 s per
  preview in the VM; now 6 to 9 ms at the median.
- **`zfs diff`** is left in hand back only, which asks what the user changed
  while driving (the broker doesn't see those edits). It costs what the
  home changed during the take over, so it grows with long take overs on a
  busy machine.
- **Sandbox start.** Creating the jail, mounting the agent's directories and
  the first snapshot make up most of "start to first step".
- **Audit.** Records are written with `fsync` (`Durability::Sync`), a few
  milliseconds each in the VM.
- **The desktop shell's redraws.** A frame redraws only the areas whose
  content changed (`render::Screen`: iced's layer diff, extended to drop
  shadows), and the wallpaper stays drawn as the screen's backdrop rather
  than being scaled again for every area. A vendored patch to
  `iced_tiny_skia` (`shell/vendor/README.md`) computes a drop shadow only
  inside the area being redrawn. The desktop has the same layers every
  frame, filled or empty, since frames are compared layer by layer and a
  layer going away made the full-screen dock layer read as changed; areas
  that overlap are joined, so nothing is drawn twice. Moving the pointer
  redraws only when a widget changes under it. Measured with `cargo test -p glass-shell
  --test redraw_cost -- --ignored --nocapture` (laptop) and the
  `drawn in` lines of `/var/log/glass-shell-fb.log` (VM):

  | Redraw | Laptop before | Laptop now | VM before | VM now |
  |---|---|---|---|---|
  | Menu bar clock turns over | 122 ms | 1.8 ms | 1.1 s | 47 ms |
  | A key typed in the command bar | 42 ms | 5 ms | 1.7 s | 28 to 60 ms |
  | Nothing changed | 190 ms | 0.4 ms | 2 s | 3 to 7 ms |
  | A notification banner comes or goes | | 23 ms, 3 ms | 0.4 to 0.9 s | 60 to 220 ms |

  Coming up after signing in (`desktop up in` in
  `/var/log/glass-greeter.log`) took 5.3 s in the VM, most of it the
  wallpaper backdrop: tiny-skia's float pipeline scaling it to full screen
  took 2 s under emulation. It's drawn in B, G, R, A order from the start
  and scaled with integer arithmetic now (0.27 s), and the desktop is up in
  1.8 s with the optimized `vm` build (`SHELL_PROFILE=vm
  os/freebsd/scripts/shell-vm.sh greeter`): glassd 0.2 s, the screen 0.6 s,
  the first frame 1 s. The sign-in screen stays up until then (ADR 0019).
  | Whole screen (first frame, opening the command bar) | 191 ms | 60 ms | 3 s | 1.5 to 1.9 s |

## Boot

`tools/boot/timeline.py` (`make vm-boot-timeline`) power-cycles the VM and
prints each phase from power on to the login prompt, timestamping what the
VM prints on its serial console, plus when glassd said it was ready (its
log, mapped onto the same clock). `TRACE=1` adds the kernel's milestones and
every rc script's time through boottrace(4); tracing runs each script
through an extra program, so traced runs are slower and good for comparing
scripts with each other, not totals.

Cold boots of the emulated VM (`VM_CPUS=2`), medians with the range, on a
host shared with other builds (three boots before; four after, two each
with and without resource accounting); the traced rows come from one
boottrace(4) run each:

| Phase | Before | After |
|---|---|---|
| Firmware: power on to the boot loader (not ours) | 3.3 s (3.3–3.6) | 3.5 s (3.4–3.7) |
| Boot loader: disks, config, menu drawn | 1.0 s (0.9–1.0) | 1.0 s (1.0–1.0) |
| Boot menu wait | 1.6 s (1.5–1.8) | 0.1 s (0.0–0.1) |
| Loading the kernel and modules | 0.4 s (0.3–0.4) | 0.4 s (0.4–0.5) |
| Kernel and startup scripts | 86.2 s (85.8–87.9) | 59.3 s (55.5–63.2) |
| **Power on to login prompt** | **92.7 s (92.2–94.3)** | **64.3 s (60.3–68.2)** |
| glassd ready, after power on | 91.0 s (90.8–92.7) | 63.6 s (59.3–66.6) |
| Traced: kernel, start to `init` | 7.3 s | 6.7 s |
| Traced: startup scripts | 141.7 s | 119.9 s |
| Traced: devd / netif / defaultroute | 19.5 / 10.0 / 0.6 s | 0.7 / 7.4 / 0.7 s |
| Traced: our scripts (identity / glassd) | 2.5 / 0.9 s | 0.5 / 0.6 s |

### Where the time goes, and what we changed

- **The boot menu waited two seconds on every boot** (1.6 s measured).
  `autoboot_delay=0`: the menu is still ours (brand, logo, title) and still
  opens if a key other than Enter is held as the machine starts, but it no
  longer waits.
- **The kernel waited for USB before mounting the root file system**
  ("Root mount waiting for: usbus0"). The root is on an internal disk, so
  `hw.usb.no_boot_wait=1`; `vfs.mountroot.timeout=10` keeps a copy booted
  from a USB stick working (the kernel retries until its disk shows up).
  On a laptop with hubs, USB enumeration takes one to three seconds.
- **devd held up the rest of startup** while it ran a handler for every
  device found at boot, among them a `service devmatch` per unmatched
  device that `rc.d/devmatch` had already handled (19.5 s traced in the VM,
  the slowest script). `devd_flags=-n` lets it do that in the background;
  hot-plug works as before.
- **Network setup waited for a DHCP lease** (`ifconfig_DEFAULT=SYNCDHCP`
  from the cloud image; 10 s traced). Now `DHCP`: the lease arrives in the
  background, as nothing in startup needs the network (ADR 0021). Without
  a cable or before Wi-Fi associates, the synchronous form waits for its
  timeout. With DHCP in the background, `rc.d/defaultroute` waits instead,
  up to 30 s for the default route the lease brings (16.6 s traced in the
  VM), so `defaultroute_delay=0` too.
- **Ours.** `rc.d/glassd` ran `glassd --check` before starting glassd, a
  second whole glassd on the way to the login prompt; glassd now checks its
  config and policy once, as it starts (ADR 0021), and says on the console
  why if it refuses. `rc.d/glasshouse_identity` ran about ten programs
  (`sed`, `mktemp`, `install`, `sysrc`, a heavy shell script) and was the
  fourth slowest script traced (2.5 s); it now uses shell builtins and one
  `sysctl`, and `/etc/motd` points straight at our login message instead of
  being copied on every boot.
- **What's left is the base system's startup scripts.** About 160 rc
  scripts run whether their service is enabled or not, and each costs about
  half a second in the emulated VM (a new shell that reads its script, the
  rc configuration and often `network.subr`), so the VM's startup is
  CPU-bound by their count: making waits asynchronous moves
  them off the critical path, but the work still competes for the VM's two
  emulated CPUs. On hardware each script costs a few milliseconds and the
  waits (DHCP, USB, devd) are what we removed.

### The budget

The PRD names no cold-boot number. The nearest are PM-1 (opening the lid
gives a usable desktop in under 2 seconds: resume, not boot), UX-2 (a fresh
install reaches a working desktop in under 5 minutes) and UX-22 (budgets on
every build, including wake to a usable desktop). The target we derive:

| On a certified laptop | Target | Why |
|---|---|---|
| Boot loader start to the sign-in screen (ours) | **5 s** | The loader takes well under a second on hardware without the menu wait, the kernel 1.5 to 2.5 s once it no longer waits for USB, and ~160 rc scripts at a few milliseconds each about 1 to 2 s with nothing waiting on the network or devd. 5 s leaves room for the greeter's first frame |
| Power on to the sign-in screen | **10 s**, with OEM firmware of 5 s or less | What people get from Windows, macOS and ChromeOS laptops of the same class (8 to 15 s); firmware is the OEM's, and the certified list (DK-12) can require its fast-boot settings |
| glassd ready | within 1 s of the sign-in screen | glassd starts just before the greeter and, checking itself as it starts, no longer holds it up; nobody signs in faster than that, and the desktop's tiles need it only after sign-in. In the VM it's ready about a second before the login prompt |
| Managed settings | 0 s on the boot path | read from disk with the policy (ADR 0021); fetching happens after the desktop is up |

In the emulated VM the same phases take ten to twenty times longer, so it
can't check the hardware target directly. What it checks on every build is
that nothing slow comes back (`tests/e2e/check-boot.sh` in `make e2e-vm`,
which doesn't reboot): the menu doesn't wait, the root mount doesn't wait
for USB, devd and DHCP run in the background, glassd checks its config once
at boot, and our identity script stays cheap.
A whole boot is measured with `make vm-boot-timeline` when boot changes;
treat more than 10% on "kernel and startup scripts" (median of three, on a
quiet host) as a regression to explain.

## Rules for new code

1. **Anything a person waits on gets a budget** in `glass-e2e perf` before
   it ships, with the PRD id when the PRD names a number (AW-10, AW-20, PM-1,
   UI-7).
2. **No process spawns on hot paths** when a cached answer is safe. Starting
   `zfs`, `jexec` or `service` is cheap on a laptop and slow in the VM, which
   makes the VM a good place to notice them.
3. **Keep blocking work off the async runtime**: file system, ZFS and
   process calls go through `spawn_blocking`, as `execute.rs` does, so one
   slow call doesn't stall every other task's tile.
4. **Read paths stay in memory.** `task.list` and `task.get` answer from the
   runtime's state without touching disk; keep it that way.
5. **Measure before optimizing**: run `glass-e2e perf` in the VM
   (`su demo -c '/usr/local/libexec/glasshouse-tests/glass-e2e perf'`) before
   and after, and say both numbers in the commit message.

## Known gaps

- **Snapshot retention (AU-20, 30 days).** Snapshots are never destroyed
  yet. They cost nothing until data diverges, but they will hold disk space
  on a busy machine.
- **Hand back still runs `zfs diff`** (above). Hand back has no budget yet;
  give it one before it grows.
- **GUI budgets** (cold launch under 1 s, UI-7; resume under 2 s, PM-1)
  wait for the GUI and real hardware.
