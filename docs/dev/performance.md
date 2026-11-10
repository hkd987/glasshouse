# Keeping the OS fast for the people using it

Build speed is in [build-performance.md](build-performance.md). This page is
about the other kind: how long a person waits on Glasshouse.

## The budgets

`glass-e2e perf` and `glass-e2e load` (part of `make e2e-vm`) measure each
of these on a live glassd in the dev VM and fail when a 95th percentile
goes over its budget:

| What the person waits for | Budget (p95) | Source | VM today (p50 / p95) |
|---|---|---|---|
| Handoff card (`task.plan`) | 200 ms | ceiling | 3 / 4 ms; 8–9 / 15–32 ms on 2026-10-23 (ADR 0051), the old build 12 / 15 ms the same hour |
| Tiles (`task.list`) | 100 ms | ceiling | 3 / 4 ms; 4.4–4.6 / 5.7–8.0 ms on 2026-10-23, the old build 7.6 / 11.8 ms |
| Expanded view (`task.get`) | 200 ms | AW-10 | 4 / 4 ms; 3.6–4.0 / 4.4–4.9 ms on 2026-10-23 |
| Take over | 1 s | AW-20 | 203 / 245 ms; 226–260 / 361–656 ms on 2026-10-23, the old build 504 / 776 ms |
| An action that needs no snapshot (reads, lookups) | 100 ms | ceiling | 3 / 12 ms; 3.5–4.0 / 6.0–9.8 ms on 2026-10-23 |
| An action that snapshots first (writes) | 750 ms | ceiling | 229–240 / 452–525 ms on 2026-10-23 (ADR 0051: steps that write take turns now, which costs nothing here), with 733 snapshots on the home dataset and a snapshot at 0.24 s; the old build 267 / 563 ms. 243 / 391 ms earlier; 2026-10-19: 293–533 / 555–977 ms, over in 3 of 7 runs. Cause: the VM's home dataset held 2,058 snapshots from test runs, all younger than AU-20's 30 days (glassd now removes older ones), and a snapshot there took 0.31 s against 0.17 s. Cleared on 2026-10-19: 294–349 / 431–627 ms, take over p95 265–288 ms. A long-lived dev VM gathers them again; `vm.sh clear-snapshots` clears them (2026-10-21: 942 again, p95 936 ms; cleared, 359 ms) |
| glassd's check of the system's files at its start, before the first agent (ADR 0064) | none yet; 2 s on a laptop | EP-1 | 7.0–7.6 s for 4,195 files under emulation (`tests/system/verify-vm.sh`, the files in memory; 48 s listing them from disk the first time). No laptop number yet |
| Start to the agent's first step | 4 s | ceiling | 1.7 / 2.6 s; 1.8–2.0 / 2.2–2.4 s on 2026-10-23, the old build 2.1 / 2.4 s. The first run after the VM boots is slower (3.7 / 5.7 s): its emulated CPU translates every program afresh |
| Undo preview (`task.undo` dry run) | 150 ms | ceiling | 6–9 / 58–94 ms (0.8–1.3 s before ADR 0017) |
| Tiles while two busy agents run (`load`) | 100 ms | ceiling | 5–6 / 13–43 ms |
| Expanded view while two busy agents run (`load`) | 200 ms | AW-10 | 5–8 / 19–117 ms |

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

### Apps (UI-7)

`make apps-test` holds each first-party web app to the PRD's budgets in
headless Chromium on the development host (`apps/e2e/perf.spec.ts`) and
fails when one goes over:

| What the person waits for | Budget | Source | Dev host today |
|---|---|---|---|
| Mail, cold launch | 1 s | UI-7 | 176 ms (median of 3: 197, 176, 173) |
| Calendar, cold launch | 1 s | UI-7 | 184 ms (178, 184, 185) |
| Mail, memory | 150 MB | UI-7 | 116 MB |
| Calendar, memory | 150 MB | UI-7 | 117 MB |
| Chat, cold launch | 1 s | UI-7 | 127 ms (132, 127, 118) |
| Chat, memory | 150 MB | UI-7 | 106 MB |
| Meet, cold launch | 1 s | UI-7 | 108 ms (104, 115, 108); with M2's grid and screen sharing, 142 ms (135, 142, 143) page alone, its bridge played by `meet.e2e.ts` |
| Meet, memory (before joining) | 150 MB | UI-7 | 100 MB |
| Artifacts, cold launch | 1 s | UI-7 | 150 ms (150, 175, 132); this suite's computer is unmanaged, so it shows that the shared space isn't ready |
| Artifacts, memory | 150 MB | UI-7 | 114 MB |
| Docs, cold launch | 1 s | UI-7 | 135 ms (145, 135, 127); this suite's computer is unmanaged, so it has no docs to list |
| Docs, memory | 150 MB | UI-7 | 108 MB |
| Joining a meeting: Join to the other person on screen, through the computer, the console and the relay | 2 s | UI-7, CF-21 | 1427 ms (`meetings.e2e.ts`, before the page stopped waiting up to 1 s for its ICE candidates; not measured since); page to relay alone (`meet.e2e.ts`) 1311 ms then, 258–355 ms now |
| Joining a three-person meeting: Join to both others on screen, one of them after a renegotiation over the data channel; page to relay alone | 2 s | UI-7, CF-21 | 392–658 ms (`meet.e2e.ts`, five runs); the two already there see the newcomer at 413–711 ms |
| A page in a two-person call | 250 MB | UI-7, CF-21 | 169 MB |
| A page in a three-person call, decoding two videos | 250 MB | UI-7, CF-21 | 168 MB (167–169, `meet.e2e.ts`) |
| A page in a transcribed call, its speech model loaded (ADR 0045 T2) | 500 MB | UI-7, CF-21 | 360–435 MB (`meet.e2e.ts`); the model and its runtime are about 250 MB of it, for as long as the call lasts |
| What the person said, as a line of the transcript: from their pause to its text, on the computer | none yet | ADR 0045 T2 | 0.8–1.0 s for 2.3–4.4 s of speech; the model loads in 1.5–1.7 s, while they join (`speech.spec.ts`) |
| Saving an app someone remixed: the checks, a bundle in a jail of its own, a try of it in another (ADR 0049), the version kept | 15 s (10 s before the try) | GA-4, ADR 0032 | 5.7–6.5 s for Mail on the VM (scenario `studio`); 7.4–10.2 s when the emulated CPU translates the try-out's code afresh, after a deploy or a quiet spell. The try is 3–5 s of it, 0.15 s natively, typing into Mail's compose form and sending it included (5.7–6.2 s and 9.6–10.2 s since). 2.0 s before the try |

*Cold launch* is a fresh browser context (no cache, no service worker, no
stored data), from navigation to the app drawn with its data (the
`glass:ready` mark), through the bridge and glassd. *Memory* is the resident
memory of the app's renderer process (its page and its service worker)
after it has loaded and been used; the browser and GPU processes are shared
by every app, so they aren't counted. Each app is one script of about 13 KB
and one stylesheet of about 6 KB.

#### On the device: from the click to the app's window

The desktop logs how long each app it starts takes to show a window
(`apps: mail opened in … ms`, in the sign-in screen's log). A web app's
window or the Browser comes from the browser engine (ADR 0027), which the
desktop starts with no window when the person signs in and keeps running,
so a window is handed to a running engine over its socket instead of
waiting for one to start (`glass_shell::engine`).

| Mail, click to window | Emulated VM | This host's CPU, natively |
|---|---|---|
| Engine not running (before) | 19.5 s | 0.33 s (median of 3, two runs) |
| Engine running, window handed over (now) | 5.8–12 s | 0.07 s |
| Starting a second engine to hand it over | 1.1 s | |
| Handing it over on the socket (now) | 0.07 s | |

The VM can't show the hardware number. The engine compiles JavaScript as
it runs, which an emulated CPU handles worst of all: the same steps take
40 to 60 times longer there than natively, against 10 to 20 times for our
own programs. The native column is `node tools/perf/engine-launch.cjs`:
the full engine (the dev host's Chromium build, in its headless mode)
loading the same Mail page, cold and in a running engine, with no
compositor in between. A person's first start, which creates their
engine data, takes about 0.4 s natively, and happens at their first sign-in,
before they open anything.

The running engine costs about 90 MB with no window (the VM's free memory
before and after it starts); each app window adds its page's renderer (about
115 MB for Mail, above).

### Admin console (AD-3, AD-4, ADR 0023)

`make console-test` drives the console in headless Chromium on the
development host against a fixture of 500 devices and 50 catalog items,
with a dev-host glassd enrolled as a pilot device
(`apps/e2e/console.e2e.ts`), and fails when one goes over:

| What IT waits for | Budget | Source | Dev host today |
|---|---|---|---|
| Policies page interactive | 1 s | AD-3 | 109–141 ms (median of 3) |
| Catalog page interactive | 1 s | AD-5 | 133–135 ms |
| A catalog item interactive | 1 s | AD-5 | 117–118 ms |
| Devices page interactive (500 rows) | 1 s | AD-4 | 294–326 ms |
| Publish to in force on an online pilot device | 5 s | AD-3 | 121–176 ms |
| An approval to the catalog on that device | 5 s | AD-5 | under 5 ms after the page confirms it |
| A keystroke in Mail's To to the colleague suggested, from the org's directory | 500 ms | AD-1 | 374 ms (a 120 ms pause in typing included) |
| Chat open with a conversation, sealed messages opened by glassd | 1 s | UI-7, CF-91 | `chat.e2e.ts` asserts it |
| A colleague's chat message, posted on their computer, on screen on this one | 1 s | CF-91 (ADR 0030) | 113 ms (loopback) |
| Open an artifact: the click in Artifacts' list to its page painted in its sealed frame (its own origin, opened by glassd through the bridge, beside the read of its comments) | 1 s | AR-2 (ADR 0033) | 104 ms (median of 3: 110, 104, 104; `artifacts.e2e.ts`) |
| Open a doc: the click in Docs' list to the doc drawn in the editor (`doc.open`: its whole state as one Yjs update, merged into the page's) | 1 s | DC-1 (ADR 0035 §4) | 138 ms (median of 3: 138, 81, 159; `docs.e2e.ts`, a short doc; a doc of 10,000 updates isn't measured yet) |
| An edit in one Docs window on screen in another: the last key typed to the line drawn (`/_bridge/live`; pushes go at most every 400 ms) | 1 s | DC-3 (ADR 0035 §3) | 362 ms (median of 3: 111, 869, 362; `docs.e2e.ts`, two windows on one bridge) |

*Interactive* is a fresh browser context with the session cookie, from
navigation to the page drawn with its data (`html[data-ready]`). *In force*
is from the Publish click to `glassctl status` naming the new version: the
device long-polls, so the console answers the moment it signs a release,
and glassd verifies, validates, writes and swaps it in. A device that is
offline gets it on its next sync (backing off to 5 minutes), and a device in
a later ring when IT promotes the version. These are host numbers; nothing
here runs in the VM.

### Every feature, on a person's worth of data (ADR 0051)

`crates/svc/runtime/tests/it/speed.rs` times each feature an agent or app
calls through the whole path (the task socket, policy, the audit record,
the snapshot a write takes, the provider) on this host's CPU, with data the
size a person has. That's a 500-message inbox, a 2,000-file folder, a
300-meeting month, a 2,000-person directory and an audit log of 20,000
records. The VM's budgets above hold the emulated numbers; this finds what
grows with the data. Run it with the shipping optimization:
`cargo test --profile vm -p glass-runtime --test it speed:: -- --ignored --nocapture --test-threads 1`.

| Feature (2026-10-23, `vm` profile) | p50 | p95 |
|---|---|---|
| Start a task, to its socket | 12.0 ms | 12.5 ms |
| `task.list`, `task.get` | under 0.1 ms | under 0.1 ms |
| `files.list`, 2,000 files | 16.9 ms | 20.3 ms |
| `files.read`, 2 KB / 1 MB | 0.6 / 8.2 ms | 0.6 / 12.1 ms |
| `files.write`, a snapshot first | 2.3 ms | 2.7 ms |
| Undo preview / undo, 22 writes | 0.7 / 4.3 ms | |
| `mail.list`, 500-message inbox, 50 listed / a count | 4.7 / 2.3 ms | 5.1 / 3.2 ms |
| `mail.read` | 0.6 ms | 0.7 ms |
| `mail.mark` / `mail.draft`, a snapshot first | 3.0 / 1.1 ms | 3.2 / 1.3 ms |
| `calendar.list`, a week of a 300-meeting month / `calendar.read` | 4.2 / 0.5 ms | 5.7 / 0.7 ms |
| `people.search`, 2,000 people | 2.7 ms | 4.5 ms |
| `apps.actions`, the list / 12 in full | 4.7 / 7.0 ms | 5.8 / 7.5 ms |
| A task's audit, in a log of 20,387 records | 2.4 ms | 2.4 ms |
| Verifying the whole log, 20,387 records | 76.8 ms | |

What it found growing, and fixed:

- **Mail's listing** read the start of every message in the folder,
  whatever it was asked for. For 500 messages that was 100 ms in the debug
  build, for 50 listed or for one.
  - It keeps what it parsed while each file's size and time stay the same,
    so a listing reads the names and parses what's new: 10.9 and 5.6 ms in
    debug, 4.7 and 2.3 ms above.
  - The Mail app lists four folders on every refresh.
- **A task's audit** (`glassctl audit`, the replay of a task's calls) read,
  parsed and hash-checked the whole log, every user's, from the first day:
  as long as verifying it, 76.8 ms at 20,000 records, and growing. An index
  of each task's records, kept up by every append, makes it 2.4 ms.
- **glassd's report to the console**, every minute, counted the last day's
  blocked actions by reading the whole log. It reads from the end back, and
  stops at the first record older than a day.
- **Each step of the desktop agent waits on the model**, so the steps it
  takes are what a person waits on. Asked to read four files and say what
  each is, Claude Haiku 5.5 already made the four reads in one step (three
  model calls, 3.5 to 3.9 s). An instruction to do so changed nothing, and
  isn't in its prompt.

What takes turns now: a person's reversible steps across their tasks, and
undo, take turns (ADR 0051, found by the Undo model). A step waits for
another task's write to the person's files to finish. Reads don't wait.

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
  the first snapshot make up most of "start to first step". The jail's
  resource limits add one `rctl` call at its start and one at its end (run
  by jail(8) through `sh`), 0.1 s for the pair in the VM.
- **Audit.** Records are written with `fsync` (`Durability::Sync`), a few
  milliseconds each in the VM when the disk is quiet, much longer behind a
  busy agent's writes; see "glassd under a burst" below.
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
  | Whole screen (first frame, opening the command bar) | 191 ms | 21 ms | 3 s | 0.7 s |
  | Launcher opens (dims the whole screen), closes | 37 ms, 30 ms | 18 ms, 7 ms | 1.0 s, 1.0 s | 0.2 s, 55 ms |

  A whole-screen redraw spent half its time laying the UI over the
  wallpaper and windows through tiny-skia's general image pipeline, and a
  sixth recomputing every drop shadow on the screen pixel by pixel. The
  layers are now blended in integers, two bytes per multiply, skipping
  clear pixels and copying opaque ones (`render::blend`), and the vendored
  `iced_tiny_skia` keeps each shadow it computed (keyed by everything that
  shapes it, so the pixels are the same) and blends in only the part being
  redrawn. Laptop numbers here are the shipping `vm` profile
  (`cargo test --profile vm …`).

  Coming up after signing in (`desktop up in` in
  `/var/log/glass-greeter.log`) took 5.3 s in the VM, most of it the
  wallpaper backdrop: tiny-skia's float pipeline scaling it to full screen
  took 2 s under emulation. It's drawn in B, G, R, A order from the start
  and scaled with integer arithmetic now (0.27 s), and the desktop is up in
  1.8 s with the optimized `vm` build (`SHELL_PROFILE=vm
  os/freebsd/scripts/shell-vm.sh greeter`): glassd 0.2 s, the screen 0.6 s,
  the first frame 1 s. The sign-in screen stays up until then (ADR 0019).
  With the faster whole-screen redraw it is up in 1.7 s: the screen 0.9 s,
  the first frame 0.74 s. The desktop's start now logs where the screen step
  goes (`screen:` in the same log): About's details took six programs (one
  `sysctl` now), and the wallpaper is drawn on its own thread while the apps'
  compositor starts. Up in 0.7 to 1.0 s (the screen step 0.3 to 0.45 s).

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

The image now boots into the sign-in screen (ADR 0019): `glasshouse_greeter`
starts right after glassd, so the screen is up with the last few startup
scripts, a little before the serial console's login prompt the tool times.
One cold boot with it on came out at 57.6 s to the login prompt (glassd
ready at 56.3 s), no slower than the runs above.

The package-list image (ADR 0022), carrying the desktop's libraries since
2026-10-19, against the dev VM on the official image, the same hour on the
same host with nothing else building (medians; this host ran slower than
on the days above, so compare the two columns, not with the tables before):

| | Dev VM (official image) | Package-list image |
|---|---|---|
| Power on to login prompt | 76.1 s (75.3–76.9) | 60.9 s (60.2–61.7) |
| glassd ready, after power on | 75.5 s | 60.2 s |
| Signing in to the desktop, first time after boot | | 2.3 s (the screen step 1.6 s, cold caches) |

Traced, the package-list image runs 100 startup scripts and none stands
out (netif 5.8 s, devfs 4.4 s traced; ours under 1 s each): what's left is
their number, which the product image's list (no sshd, no nuageinit)
brings down further.

## Where the time goes, and what we changed

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
- **Resource accounting for agents' limits** (`kern.racct.enable=1`,
  "Under load" below) costs about 30 KB of memory (a 232-byte record per
  process, user, jail and login class: 136 allocated at the login prompt)
  and no boot time we could measure: 60.3 and 65.3 s with it, 63.4 and
  68.2 s without, within the VM's run-to-run noise.

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
at boot, resource accounting is on, and our identity script stays cheap.
A whole boot is measured with `make vm-boot-timeline` when boot changes;
treat more than 10% on "kernel and startup scripts" (median of three, on a
quiet host) as a regression to explain.

## Under load

Agents work while the person does, so the person's work comes first: each
agent starts below their CPU priority, and each task's jail gets resource
limits for as long as it exists (`[sandbox.limits]` in
`glassd.toml`, applied by `JailSandbox`, removed with the jail, PA-2):

| Setting | Default | How | Why |
|---|---|---|---|
| `nice` | 10 | `nice -n 10 jexec …`; the agent, an unprivileged user, can't lower it | FreeBSD's scheduler (ULE) runs the person's threads first when both want a CPU; an agent still gets every idle cycle. Not idle priority (`idprio`): an idle-class process holding a file system lock can stall the person's processes behind it, since the kernel doesn't lend it priority |
| `cpu_percent` | 50 (of the machine, per task) | rctl `pcpu:deny` | No single agent takes more than half the machine, however many threads it runs |
| `memory_percent` | 25 (of physical memory, per task) | rctl `swapuse:deny` and `memoryuse:deny` | Committing memory past it fails at once (the agent gets an allocation error) instead of the machine swapping; resident pages over it are the agent's to give back first |
| `read_mb_per_sec`, `write_mb_per_sec` | 64, 32 MiB/s | rctl `readbps`/`writebps:throttle` | Enough for any document work; leaves the disk to the person (and to glassd's audit log, which syncs every record) |
| `read_iops`, `write_iops` | off | rctl `readiops`/`writeiops:throttle` | Configurable; off until we know how ZFS counts operations for agents' workloads |
| `max_processes` | 256 per task | rctl `maxproc:deny` | A runaway agent can't fill the process table |

The limits need resource accounting, which the kernel turns on only at boot
(`kern.racct.enable=1` in `loader.conf`, set by `post-install.sh`; restart
once after deploying). Without it glassd logs a warning and applies only
the priority. Accounting costs about 30 KB of memory and no measurable boot
time ("Boot" above).

`glass-e2e load` (in `make e2e-vm`) starts two busy agents (`glass-probe`'s
load mode: a spinning thread per CPU and a writer syncing every 16 MB) and
measures the shell's reads while they run. Two agents keep both of the
VM's emulated CPUs busy and the disk writing at its maximum (about 70 MB/s
together without limits).

| `glass-e2e load` in the VM, three runs each (n=20 per run) | Tiles p50 / p95 | Expanded view p50 / p95 | Each agent wrote |
|---|---|---|---|
| Idle (`glass-e2e perf`, for comparison) | 3.6 / 7.6 ms | 4.0 / 6.1 ms | — |
| Two busy agents, no priority or limits | 5.1–5.2 / 46–79 ms | 4.9–7.7 / 10–61 ms | 32–42 MB/s |
| Two busy agents, with them | 4.6–6.3 / 13–43 ms | 5.3–7.7 / 19–117 ms | 16–29 MB/s |

What this shows: glassd's control API was never the problem. Its threads
sleep between requests, so the scheduler already runs them ahead of a
CPU-bound agent, and `task.list` and `task.get` answer from memory; the
medians are the same either way and the 95th percentiles move with the
host's own load (the VM shares four cores with other builds). What the
priority and limits buy is everything that has to compete: the shell's
drawing, which is all CPU, and anything that waits on the disk.

The desktop shell on the VM's screen (`make shell-vm`; the `drawn in`
lines of `/var/log/glass-shell-fb.log`), opening the command bar, typing
eight keys a second apart and closing it, while two busy agents (each
`load for 300s`) run and report progress every two seconds:

| Redraw, emulated VM | Idle | Two busy agents, no priority or limits | With them |
|---|---|---|---|
| Opening the command bar (most of the screen) | 1.8 s | 6.4 and 7.5 s (12.3 s in a trial run) | 2.3 and 2.5 s |
| A key typed in the command bar | 29 to 69 ms | 30 to 380 ms | 30 to 160 ms |
| An agent's progress in the bar's list (180 to 210 rows) | — | 140 ms to 2 s | 130 to 400 ms (once 830 ms) |
| Closing the bar | 0.8 s | 0.8 and 1.9 s | 0.8 and 1.7 s |

(Two rounds each, alternating, on a host shared with other builds.)
Without them a busy agent counts the same as the person: the big redraw
took four times as long, and once a redraw ran long the scheduler stopped
treating the shell as interactive, so keys and progress updates queued
behind the agents' threads. With them the big redraw stays within 40% of
idle and keys mostly at their idle times, and the agents still use every
cycle the person doesn't (each wrote 16 to 29 MB/s instead of 32 to 42).
The rest is the emulated VM: two CPUs shared by the shell, glassd and four
spinning threads.

### Agent computers going to sleep

A host being replaced has twelve minutes for every computer on it (ADR
0066), so a save has a budget: a second for every 10 MB and thirty at
least for a computer's first save, all of it new to the store, held in
`make aws-local-test`. Against moto a 300 MB computer takes about 4 s.
What keeps it there: a save asks S3 only about chunks its computer's last
version doesn't name, sends new chunks four at a time, hashes each once,
and a leaving host sleeps four computers at once. Each chunk is still its
own TLS connection.

### glassd under a burst

- **Audit records were written on the async workers.** Each record is
  synced to disk before the action continues (fail closed), and an fsync
  waits behind whatever else is being written, so with agents writing a lot
  one record could hold a worker, and two could hold both of the VM's,
  stalling every reply. `Runtime::audit` now hands the worker's other work
  to another thread while it waits (`tokio::task::block_in_place`), and so
  do the control calls that read or write files (`task.plan`, `task.start`,
  layers, the audit log reads). `task.list` and `task.get` stay in memory.
  Since the audit of 2026-11 (9.1) records also share syncs: whoever finds
  nobody writing writes everything waiting as one run with one fsync, and
  the wait for the queue is inside that hand-off too (it was a plain lock
  taken before it, which parked the worker unannounced). Eight writers'
  200 records took 49 syncs where they took 200 (ADR 0053, amended).
- **Every tile update went to the shell as its own message.** An agent
  calling actions quickly, many tasks updating at once, or new settings
  applied to all of them, sent one notification per change, each a whole
  tile. glassd now coalesces per subscriber: an update after a quiet spell
  goes at once; what follows within 50 ms goes at the end of that window,
  the latest per task (each carries the whole tile, so nothing is lost).
  The shell settles for 50 ms itself before it redraws.

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

- **Snapshot retention (AU-20, 30 days).** glassd removes undo snapshots
  older than 30 days at its start and daily, a hundred of a dataset at a
  time, and keeps those a task in memory could still undo from. A task
  stays in memory until it is dismissed, so on a computer nobody dismisses
  tasks on, snapshots still gather while glassd is up.
- **Hand back still runs `zfs diff`** (above). Hand back has no budget yet;
  give it one before it grows.
- **GUI budgets** (cold launch under 1 s, UI-7; resume under 2 s, PM-1)
  wait for the GUI and real hardware.
