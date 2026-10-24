# 0057. Pilot laptops, and hardware fixes as kernel patches

Status: **Accepted** (2026-10-24); slice 1 in progress. The owner,
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
- **Built off the computer** (BS-4). The build service builds the modules
  from the image's exact source, reproducibly. It signs the bundle (the
  patch, the manifest, the modules and their SHA-256s) with `glass-managed`'s
  envelope, as a document kind of its own:
  - with our key, for patches we reviewed;
  - with the org's key, for patches IT approved.

  Under ADR 0026 the modules join the signed manifest of what may run.
- **Tried in a boot environment of its own.** glassd checks:
  - the signature;
  - that the bundle was built for this kernel;
  - each module's hash.

  Then it makes a new boot environment from the running one, puts the
  modules in it, and boots it once (`bectl activate -t`). After that start,
  glassd confirms: it's running from the new environment, the modules are
  the bundle's and loaded, and nothing panicked. Only then is the new
  environment permanent.

  Otherwise the next start goes back to the previous one by itself. A panic
  restarts the computer at once: the image turns off the kernel debugger on
  panic. A start that hangs needs the chipset's watchdog (`watchdogd`) on
  the certified models.
- **Undo** is starting the previous environment, kept until the person
  removes it. Every step is on the audit record.

### 3. Who writes them
- **Us**, for the certified models: backports and fixes, through the same
  pipeline, into the image and the store. The first is the LinuxKPI 802.11
  suspend and resume fix: Wi-Fi works again after sleep on Intel's and
  Realtek's cards.
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
- **The image turns off the kernel debugger on panic**
  (`debug.debugger_on_panic=0`).
