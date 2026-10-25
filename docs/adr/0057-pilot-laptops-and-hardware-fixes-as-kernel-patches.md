# 0057. Pilot laptops, and hardware fixes as kernel patches

Status: **Accepted** (2026-10-24); slice 1 built. The owner,
after ADR 0056: "Ya let's pick laptops that already support FreeBSD then the
hardware fixes should be written as kernel patches that apply." That settles
the question ADR 0056's follow-up left open: hardware fixes may be kernel
code, written by people or agents, not only settings and packages.

## Context
- ADR 0056 found what keeps current laptops from working on 15.1:
  - **Sleep:** 15.1 sleeps with S3 only. Suspend to idle is unfinished even
    in the base system's development branch (its ACPI change, D48734, was
    still in review in January 2026). Lenovo's laptops after the X1 Carbon
    Gen 10 offer only modern standby.
  - **Cameras:** cameras behind Intel's image processor (IPU6, most Intel
    laptops since the 12th generation) have no driver.
  - **Microphones:** built-in microphones wired through Intel's audio DSP
    (Smart Sound) have no driver.
  - **Wi-Fi:** MediaTek and Qualcomm Wi-Fi score 0 of 2 in the FreeBSD
    Foundation's community tests.
- **What 15.1 gained:** `iwx` (Intel AX210 and AX211), `iwlwifi` updated to
  Linux 7.0, and `rtw88`/`rtw89`.
- **What 15.1 broke on laptops:** Wi-Fi stops working after resume. The fix
  (LinuxKPI 802.11 suspend and resume, `0c0f6654`, tested on a ThinkPad
  X270 with AX210) is in stable/15, after 15.1 was cut.
- **What the PRD already says:**
  - nothing is compiled on a person's computer (BS-4);
  - the base is signed and verified, with updates through boot environments
    that roll back by themselves when a boot fails (EP-1);
  - only signed software runs (ADR 0026, Proposed);
  - an escape from containment is a P0 bug (PA-3).

  Kernel code runs outside every sandbox, so it has to fit these.

## Decision
### 1. The pilot's laptops
Models whose parts 15.1 already drives, with S3 in their firmware:

| Model | Why | To check on the unit |
|---|---|---|
| **Lenovo ThinkPad T14 Gen 2 (Intel, 11th generation)**, first | Scores 8/10 in the Foundation's community table: Iris Xe, AX210 Wi-Fi, I219-V Ethernet, Thunderbolt 4. Its firmware offers "Linux S3". USB camera. Built-in Ethernet for setup, and Wake on LAN for PM-4's sleep cycles. Lenovo certifies it for Linux. | Its Wi-Fi needs Intel's AX210 firmware package, which the image doesn't carry yet (ADR 0056); the built-in microphone (likely through the DSP, so a headset); the touchpad after S3; Wi-Fi after resume (the first patch) |
| **Dell Latitude 7490 (8th generation)**, the second maker | Scores 8/10: UHD 620, Ethernet, HD Audio with the microphone on the codec. S3 by default. Its Intel Wi-Fi (8265, or 9560 in some) runs on firmware the image already carries. USB camera. | Older (2018), so bought refurbished; check the Wi-Fi card fitted |
| **Lenovo ThinkPad X1 Carbon Gen 9**, the thin option | The T14 Gen 2's platform. "Linux S3" in its firmware. A FreeBSD 15.1 report has Wi-Fi, graphics and S3 working. | As the T14 Gen 2, without Ethernet |

**Not for now:**
- 12th-generation Intel and later (IPU6 cameras on many, and modern
  standby only);
- Lenovo's Gen 11 and later;
- MediaTek or Qualcomm Wi-Fi;
- Framework laptops, until suspend to idle lands. The Foundation's work on
  it targets the Framework 13 AMD.

None of these models has run the image yet. Before buying for the pilot,
one unit of each runs `tests/hardware/capture.sh` from 15.1's own USB
stick, and goes through `docs/dev/hardware.md`'s checklist.

### 2. A hardware fix is a kernel patch
- **What it is:** a patch to the base system's source, at the release the
  image runs (`releng/15.1`). It comes with a manifest saying:
  - what it fixes: devices by PCI or USB id, and models;
  - which modules it rebuilds;
  - where it came from: an upstream commit, us, or an agent;
  - its requirement ids.
- **Modules, and the kernel when the code is built into it.** A patch
  rebuilds the modules it changes, and the kernel itself when it changes
  code the kernel is built with: rebuilt from the image's own
  configuration (`GENERIC`).

  At first this said "modules only", reasoning that a module can be left
  out at the next start but the kernel can't. That was wrong twice:
  - A kernel tried once in its own boot environment is left out at the
    next start just like a module.
  - The first real patch needed the kernel. 15.1 builds LinuxKPI's 802.11
    layer into the kernel (`GENERIC` has `wlan` and `COMPAT_LINUXKPI`), so
    a patched `linuxkpi_wlan.ko` can never load ("already present in the
    KLD 'kernel'"), and the patched `if_iwlwifi.ko` that needs its new
    symbol can't either.

  glassd's check caught exactly that in the VM: the patch failed its
  check, the next start went back by itself, and its environment was
  removed.
- **Built off the computer** (BS-4). The build service builds the modules,
  and the kernel when the patch needs it, from the image's exact source,
  reproducibly. It signs the bundle (the
  patch, the manifest, the modules and their SHA-256s) with `glass-managed`'s
  envelope, as a document kind of its own:
  - with our key, for patches we reviewed;
  - with the org's key, for patches IT approved.

  Under ADR 0026 the modules join the signed manifest of what may run.
- **Tried in a boot environment of its own.** glassd checks:
  - the signature;
  - that the bundle was built for this kernel;
  - each module's hash;
  - that it goes on top of the patches the computer keeps (below).

  Then it makes a new boot environment from the running one, puts the
  modules in it, and boots it once (`bectl activate -t`). After that start,
  glassd checks again: it's running from the new environment, the kernel
  and modules are the bundle's and load, and the keys and community setting
  in force now still take it (IT may have turned them off since).
- **On probation, then kept.** A patch that passes is on probation: the
  computer runs on it, but its environment isn't the permanent one until
  the computer has been in use on it for a working day
  (`probation_minutes`, 480 by default; minutes asleep don't count) and,
  on a computer that sleeps, has woken from sleep on it
  (`probation_resumes`, 1 by default; a patch for sleep asks for more in
  its manifest, `resumes`, and is refused where the computer doesn't
  sleep). Any kernel patch can break waking, and a person who didn't close
  the lid that day would otherwise keep one that does. Until
  then the next start is set to it only as the computer shuts down
  cleanly: rc.d's shutdown tells glassd (`glassctl system shutdown`). A
  crash, a panic or a power cut never gets there, so the start after one is
  the environment before, and the trial says it rolled back. That's for
  the patch that starts fine and crashes at the first sleep, exactly where
  a Wi-Fi fix works.

  A patch that panics as the computer starts costs one restart: the image
  turns off the kernel debugger on panic, and the start-once is spent. A
  start that hangs is the same once the computer is turned off and on.
- **The hardware watchdog** (Intel's chipset timer, `ichwd`, loaded at
  every start) is armed by glassd with the base system's `watchdogd`
  while the computer runs a patch not yet kept: from glassd's start check
  until the patch is kept, and again while a kept one is taken out. A
  patch that hangs the computer once it's in use restarts it within about
  two minutes, nobody at the power button, on the environment before.
  - It stays armed through a shutdown: a patch can hang that too, and a
    clean shutdown has already set the next start to the patch.
    `watchdogd` stops with every other process and turns the watchdog off
    as it exits (15.1's ignores `-x`, the timeout to leave it with), so as
    the computer shuts down glassd stops it first and leaves the watchdog
    a minute of its own (`watchdog -t 60`; Intel's restarts after two). The
    start after the watchdog's restart says so in the kernel's messages
    ("resuming after hardware watchdog timeout"), and glassd fails the
    patch then.
  - A person who holds the power button sooner, as the shutdown hangs,
    starts the patch again, as the shutdown had set: nothing tells that
    from a shutdown that finished (the power-off clears the kernel's
    messages, and nothing of the person's runs after the kernel takes
    over). Each shutdown hangs again, and the patch can be kept after its
    probation, as a patch that hangs only now and then can. The person
    takes it out in Settings.
  - It can't help a computer that hangs as it wakes: the timer doesn't run
    asleep. That one needs the power button, and the start after it is
    the environment before.
- **On top of each other.** A patch that rewrites a file a kept patch
  rewrote (any two kernel patches) must have been built on top of it
  (`includes` in its manifest), or it's refused: kept anyway, it would undo
  the first patch's fix while the record says both are kept. A patch built
  on top of one the computer doesn't keep is refused too. `glassctl
  hardware patch status` lists the patches kept in the running
  environment.
- **Taken out** with `glassctl hardware patch undo`: on probation or kept,
  the environment it was made from is the one every start uses again, and
  the next start removes the patch's. The loader's menu still lists the
  environment before until then. Every step, and every refusal, is on the
  audit record.

### 3. Who writes them
- **Us**, for the certified models: backports and fixes, through the same
  pipeline, into the image and the store. The first is the LinuxKPI 802.11
  suspend and resume fix: Wi-Fi works again after sleep on Intel's cards
  (`iwlwifi`). Realtek's (`rtw88`, `rtw89`) need their modules rebuilt with
  it too, a later revision.
- **Agents**, at a person's request, when `glassctl hardware` marks a part
  ✗. The agent reads the base system's source (read only), the report and
  the kernel's messages, and writes a patch. It never builds or installs
  one: the build service builds it, and the person (or IT) approves
  applying it.

### 4. The store's Hardware category
- A patch is shared with:
  - what it targets;
  - its source and its build;
  - how many computers confirmed a start on it.
- **Reviewed:** we reviewed it and signed it. Any computer with the
  hardware is offered it, and it goes upstream (PM-10).
- **Community:** an agent's or a person's, unreviewed. The build service
  signs it, for integrity only. It runs in the kernel, outside every
  sandbox, and the person is told so in plain words before saying yes. A
  managed computer takes one only if IT allows it; by default, it doesn't.
- Nothing is applied without a yes.

### 5. Slices
1. **Now, in the dev VM:**
   - 15.1's source;
   - the first patch built into a bundle;
   - glassd trying it in a boot environment of its own and confirming it;
   - a patch that panics, rolled back by the next start;
   - a TLA+ model of the trial through crashes and power cuts.

   The dev VM stands in for the build service: it has the compiler, and
   the host can't build the base system's modules.
2. The agent's tool to write a patch, and the build service.
3. The store's category, the review queue, and IT's setting.

## Slice 1, as built (2026-10-24)
- **The first patch builds.** Upstream's fix, applied to 15.1's source,
  rebuilds GENERIC and `if_iwlwifi.ko`: a 39 MB bundle. The build service
  stand-in builds the release's kernel once (`zroot/kpatch/kernel@15.1-RELEASE`,
  1 h 36 min in the emulated VM). On top of it the patch rebuilds three
  objects and relinks: the whole bundle, kernel and driver, in 9 min 47 s.
  The patched kernel is the same size as the one a full build of the patch
  made (27,877,976 bytes).
- **The owner asked for formal checks and safety for people.**
  `HardwarePatch.tla` follows a patch past its first start, and
  `HardwarePatchStack.tla` keeps patches on top of each other. They found
  three things the first code got wrong. Each is now an Old switch that
  fails, a test, and a fix:
  - kept at the first start (`noProbation`): a patch that panics later is
    then what every start uses;
  - trust checked only at setup (`trustOnceAtApply`);
  - a second kernel patch replacing the first (`stackBlind`).

  The models also reject two other ways to do probation, the start-once
  set at every start or whenever glassd stops: either way, a crash starts
  the patch again.
- **In the VM** (`tests/hardware/patch-vm.sh`, 2026-10-24):
  - the Wi-Fi fix started on its kernel, passed the check (the patched
    driver loads only against the patched kernel), came back after a
    clean restart on probation, and was kept after its minutes of use;
  - a second kernel patch was refused, the fix was taken out, and the next
    start was the VM's own environment, which removed the patch's;
  - a module that panics as it loads cost one restart;
  - one that panics three minutes in, on probation, restarted the VM on
    its own environment by itself;
  - one that hangs the start was rolled back after a power cut;
  - tampered, untrusted, community and wrong-release bundles were refused,
    each on the audit record;
  - the audit log verified after each.
- **Not yet like the release** (both done since, below):
  - The patched kernel was built without CTF (`MK_CTF=no`, for speed), so
    DTrace had no type information on it.
  - Probation counted minutes awake, not wakes from sleep.

## Wakes, the watchdog and CTF (2026-10-25)
- **`HardwarePatch.tla` gained sleep, hangs and the watchdog:** a patch
  that hangs once in use, and one that hangs the computer as it wakes.
  Its Old switches fail as each earlier version would:
  - `minutesOnly`: a patch that hangs every wake is kept after two ticks of
    use with no sleep;
  - `noWatchdog`: a patch that hangs leaves the computer hung;
  - `watchdogOffAtShutdown`: a patch that hangs the shutdown does too;
  - `resetUnheard`: one that hangs every shutdown, its next start already
    set to it, stays on probation and is kept.
- **The base kernel is built with CTF** now, as the release's is
  (GENERIC's `WITH_CTF`): `zroot/kpatch/kernel@15.1-RELEASE-ctf`.
- **In the VM** (`tests/hardware/patch-vm.sh`, 2026-10-25):
  - the Wi-Fi fix, rebuilt on the CTF kernel, went on probation with the
    watchdog armed, counted its two wakes and its minutes, and was kept,
    the watchdog disarmed;
  - a power cut on probation went back to the environment before;
  - a module that panics as it loads, one that panics three minutes in,
    and one that hangs the start (a power cut) each went back, and the
    patch was removed;
  - a module that hung every processor on probation: the watchdog
    restarted the VM by itself, on the environment before ("the computer
    hung on it, before its probation ended, and the hardware watchdog
    restarted it");
  - a module that hung the shutdown left the VM hung: the chipset's timer
    was halted. 15.1's `watchdogd` ignores `-x` and turns the watchdog off
    as the shutdown stops it, which is what the model's
    `watchdogOffAtShutdown` switch says fails. With glassd leaving the
    watchdog its own minute (above), it restarted the VM onto the patch,
    and glassd failed it ("the computer hung on it, and the hardware
    watchdog restarted it");
  - the audit log verified after each.
- **What emulation changes** (QEMU without KVM):
  - Its firmware can't wake this OS from S3 (OVMF faults, SeaBIOS hangs),
    so `vm.sh sleep-wake` runs the suspend and resume path without it
    (`debug.acpi.suspend_bounce`).
  - OVMF built without SMM can't take the TCO watchdog's interrupts, and
    starts stall: the dev VM boots the build with SMM.
  - A start can wait minutes on the emulated USB controller and CD drive.
    A test module that hung three minutes after it loaded went off before
    glassd ran, a hang no watchdog was armed for yet (the `hang` case: a
    power cut). The test module now hangs when the test asks.

## Slice 2, as built (2026-10-25)
- **The hardware app** gives agents five actions:
  - `hardware.report`, the computer's parts;
  - `hardware.source_search` and `hardware.source_read`, the release's
    kernel source, which the build service holds;
  - `hardware.patch_build`, the bundle or the compiler's errors;
  - `hardware.patch_propose`, at least Ask whatever a rule says, and never
    under an approval for the rest of the task
    (`glass_policy::EVERY_TIME`).
- **`glass-kbuild` is the build service.** In development it builds through
  `kernel-patch.sh` in the dev VM.
- **`glass-hardware-agent` writes the patch.** A model plans, reads and
  writes exact edits. The program turns them into a diff, builds it, and
  fixes compiler errors (three tries). The `kernel-patch` skill teaches
  it, with five evals.
- **Only a reviewer's key makes a patch reviewed.** A patch said whether it
  was reviewed, and any trusted key's signature counted, the build
  service's included. `[hardware] keys` are now reviewers' keys.
  `community_keys` sign only patches nobody reviewed: a patch one of them
  signed that says it was reviewed is refused.
- **On a real model in the dev VM** (2026-10-25, Sonnet through
  `tools/dev/real-hardware.sh`; the VM given an emulated HD Audio card):
  - Asked about sound after sleep, it declined twice. First it had read
    only the first 400 lines of `hdac.c`: the plan names its reads before
    the searches say where the code is, so the write step may now ask to
    read more. Then, having read the resume paths, it found the sequence
    sound and said it wouldn't change it without a log of a failed
    resume: the right answer on a VM whose sound works.
  - Asked to name QEMU's codec, it wrote a one-line edit to `hdacc.c`,
    built as `snd_hda.ko`. This kernel has `snd_hda` built in, so that
    module would never load, and the start check would have taken
    `kldload -n`'s "already loaded" for it and kept a patch that never
    ran. Now such a patch is refused before the person is asked, the
    start check wants the module's own file loaded, and the agent builds
    the same edit into the kernel (4½ minutes). Approved and restarted,
    the kernel named the codec ("QEMU HDA Duplex"), on probation.
  - The relay needed two fixes: policy now takes `http://` to this
    computer's loopback as an endpoint (the gateway uses it only with
    `model_relay`), and the tunnel is an ssh connection of its own.

## Slice 3, as built (2026-10-25)
- **IT's setting.** `hardware.community_patches` (a device setting, off by
  default) lets a computer take patches nobody reviewed. IT sets it on a
  managed computer, the owner on their own.
- **The person's view.** Settings' "This computer" shows:
  - the version and its OS update;
  - the patch being tried, with its probation in minutes and wakes;
  - the patches kept.

  Anyone at the screen takes a patch out (`settings.patch_remove`); only
  the owner of an unmanaged computer installs updates. Both are the
  person's own (`PERSONS_OWN`).
- **IT's review queue.** On a managed computer that doesn't take patches
  nobody reviewed, `hardware.patch_propose` sends the patch to IT instead
  (`/device/v1/hardware/submit`). It sends the patch's source and what the
  person said, never the binary built on the computer. The person's yes,
  every time, covers that, and the review says so.
  - The console has its build service build it again, one patch at a time
    (`--kbuild`; a stand-in with `--dev`). It checks the build is the
    patch it asked for, signed by the service as unreviewed.
  - The catalog page shows the patch's review:
    - who asked, on which computer, and what they said;
    - the targets and what it replaces;
    - the build and its log;
    - the diff, file by file;
    - from the computers' reports, how many kept it, are trying it, or
      went back.
  - Approving signs the console's build again with the org's key, as
    reviewed. On a managed computer the org's keys are reviewers' keys.
  - A patch is made available, never required.
- **The Store's Hardware category.** A computer is offered a reviewed patch
  only if one of its parts (`pci:<vendor>:<device>`, `model:<maker>
  <product>`, from its hardware report) is one the patch is for, and only
  if it runs the release the patch was built for.
  - `catalog.patch_install` is the person's own, after a confirm in plain
    words. glassd then fetches the bundle and keeps it only if its digest
    is the one the signed catalog names.
  - The runtime checks it as it checks any patch, and it's tried, on
    probation and kept, or goes back.
  - `catalog.search` never turns a kernel patch up for an agent.
- **Run end to end in the dev VM** (2026-10-25), the VM enrolled with
  `make console-dev` and taking only reviewed patches, the agent on a real
  model through the org's credits (`glass-console --dev-anthropic`, which
  sends credit calls to Anthropic with the host's key in development):
  - The agent wrote the codec's name into `hdacc.c` and asked to send it
    to IT. Before that, the computer refused the first build, a module
    for a driver this kernel has built in, on this route too
    (`hwpatch::fits_here`), and the agent built it into the kernel.
  - The console's build service built it again: 4 minutes. It had refused
    every real build until then, "isn't the patch it was asked to build":
    `glass-kbuild` put the summary before the diff in the bundle.
  - IT read who asked and what they said (the agent hadn't sent that
    either), the target, the console's build and its log, the diff, and
    approved it for everyone.
  - The VM's catalog took it, and the Store's Hardware category offered
    it. The person installed it after the confirm, restarted, and the
    kernel named the codec, a reviewed patch on probation. IT's page
    counted it "Being tried".
  - The agent had told the person their patch was "set up: restart to try
    it" when it had gone to IT; it says where it went now.

## Consequences
- **Community patches are a deliberate exception to containment** (PA-3):
  code a person chose to run in the kernel. It's never applied without their
  yes, and never on a managed computer unless IT allows it. A bad one costs a
  restart, not the computer.
- **Updates rebuild the patches.** A patch is built for one release. A new
  image means rebuilding every patch for it, or dropping the patch once the
  release has the fix (as 15.2 will have the first one).
- **A patch that rebuilds the kernel carries it whole** (29 MB in 15.1, a
  bundle of about 40 MB), and two such patches can't both be kept: the
  second must carry the first's change too. The build service builds
  kernel patches on top of those a computer already keeps.
- **What probation doesn't cover:** a patch that crashes only after its
  probation, days later, crashes the computer until someone takes it out
  (`undo` between crashes, or the environment before from the loader's
  menu). Probation is a working day so that sleep, the network and the
  person's own devices have all been through it; the pilot carries
  reviewed patches, upstream's first.
- **Kernel code is outside every sandbox,** so what the trial proves is
  that a bad patch is undone, not that a patch is good: a patch that
  works and does harm quietly is stopped by the signature and the review,
  never by the trial.
- **The image turns off the kernel debugger on panic**
  (`debug.debugger_on_panic=0`).
