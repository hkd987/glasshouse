# 0056. Checking hardware, and a soak test

Status: **Accepted, built** (2026-10-24). The owner approved the soak test
and the run on real hardware proposed after ADR 0054, and asked: "do we need
to figure out if there is a way to simulate hardware support for different
devices and should we verify WiFi support, camera, etc…"

## Context
- V1 runs on a short certified list from one or two makers (DK-12). A
  model is certified only once sleep, wake and battery meet PM-1 to PM-6
  (PM-10), and webcams, headsets and docks work with no setup (DK-11).
- Everything so far ran in the dev VM. QEMU emulates sound (HD Audio, USB),
  Ethernet cards, a TPM, USB devices, and any amount of memory or disk.
- It has no Wi-Fi radio, camera, Bluetooth controller, battery, graphics
  card with a driver, dock or Thunderbolt, and its firmware sleeps only
  with S3.
- The base system's simulated radio (`wtap`) does ad hoc and mesh
  networks only (`ic_caps = IEEE80211_C_MBSS | IEEE80211_C_IBSS` in its
  source), so it can't stand in for a laptop joining an access point.
- Nothing had run agents for hours in a row.

## Decision
### 1. A hardware report: `glassctl hardware`
`glass_platform::hardware` reads the base system's tools: `pciconf`,
`devinfo`, `usbconfig`, `/dev/sndstat`, `net.wlan.devices`, the ACPI sleep
states, `acpiconf` and the kernel's messages. From them it says, for each
part, what this OS makes of it:
- works;
- limited, saying how;
- not supported;
- not there;
- unknown, because USB devices and the battery need root.

The parts are graphics, Wi-Fi, Ethernet, Bluetooth, camera, sound,
touchpad, sleep, battery and TPM. Each one says what the OS doesn't do
with it yet, with the requirement's id ("the Wi-Fi pane in Settings,
CF-12"). The report also names what keeps the computer off the list.

What it recognizes:
- Wi-Fi whose firmware didn't load, from the driver's own message;
- cameras behind Intel's image processor (IPU6 and later, by the PCI ids
  Linux's drivers take), and USB cameras with no driver running;
- touchpads on I²C that no driver took;
- Intel's audio DSP with no driver;
- Intel Bluetooth without its firmware;
- firmware that offers only modern standby;
- a worn battery.

`--json` gives a model's record for the certified list. Names a device
sets for itself are printed as text, never as terminal commands. It runs
on its own, without glassd. Its tests decide from output captured in the
dev VM (`testdata/hardware`) and from laptops as their tools describe
them.

### 2. Emulated devices in the VM: `tests/hardware/matrix.sh`
`vm.sh` takes `VM_DEVICES`, more QEMU arguments. The matrix boots the dev
VM once per profile:

| Profile | Emulates | The report must say |
|---|---|---|
| hda-mic | HD Audio, speaker and microphone | sound works |
| hda-out | HD Audio, line out only | sound limited, no microphone |
| usb-audio | USB speakers | sound limited |
| intel-nic | Intel 82574L and 82576 | Ethernet works (em, igb) |
| realtek-nic | Realtek 8139 | Ethernet works (rl) |
| tpm | a TPM 2.0 (swtpm), CRB | the TPM works |
| small | 2 GB of memory | 2 GB, and an agent runs |

Sound has to play: what the VM plays lands in a WAV file on the host,
which must hold more than silence, and the microphone has to record.

### 3. A soak test: `tests/soak/soak.sh`
Agents work without a break for hours, as demo, in rounds:
- Invoice demo reconciles and its emails are approved;
- the IT agent checks the disk;
- every third round the invoice task is undone;
- every tenth, ended tiles are dismissed.

glassd is killed outright every 20 minutes, and the VM loses power every
45. Each round samples into `metrics.csv`:
- glassd's memory, open files and threads;
- the tasks it keeps, jails, sandbox entries and snapshots;
- the audit log's size and the pool's free space.

The audit log is verified every half hour. It fails on:
- a round that doesn't finish;
- a log that doesn't verify;
- a sandbox left once no task is live;
- memory or open files growing within one run of glassd.

A failure carries the command's error and exit status.

**glassd's memory under the soak.** In the VM, glassd's resident memory
rose from about 25 MB to 34 MB over its first 25 rounds, then by 35 to
50 KB a round, slowing, and the same again after each restart. A heap
profile on the dev host (heaptrack, the soak's rounds, everything
dismissed, then glassd killed so that all it held counts) found no leak:

| | After 100 rounds | After 500 rounds |
|---|---|---|
| Heap at its peak | 2.48 MB | 2.69 MB |
| Held at the end | 1,111 KB | 1,313 KB |
| Answered asks the desk remembers (`HISTORY`, 200) | 151 KB | 300 KB, full |
| Ended tasks it remembers (PA-2, `ENDED_KEPT`, 4,096) | 8 KB | 50 KB |

Everything else held stays the same from round 100 to 500: action
definitions, policy, the event channel's buffer, and OpenSSL's random
state per thread. Resident memory on the host grew by 24 KB in the last
50 rounds. The rest of the resident memory is the allocator's free pages,
thread stacks and the program's code, not data.

### 4. Laptops: `docs/dev/hardware.md`
How to put the image on a laptop and in as root:
- the dev VM's seed on a second stick;
- Ethernet over a USB adapter.

Then the report, and what only a person can check: joining WPA2-Enterprise,
a meeting with the camera and headset, the lid, a dock, battery life. For
PM-4's 1,000 sleep cycles, the base system can't set a wake alarm, so
something has to wake the laptop each time: Wake on LAN, or a key presser.

## What the image and the base system say before any laptop is tried
| Part | Finding | What it takes |
|---|---|---|
| Sleep | 15.1 sleeps with S3 only. Suspend to idle is in the base system's development branch (`POWER_STYPE_SUSPEND_TO_IDLE`), not in 15.0 or 15.1. A laptop with only modern standby can't sleep, and PM-1 asks for modern standby. | Certify models that still offer S3 for the pilot, and backport suspend to idle or wait for the next release (PM-10 already plans upstream work) |
| Wi-Fi | The image carries firmware for Intel's older cards (`iwm`, up to the 9000 series). Wi-Fi 6, 6E and 7 cards (AX200 to BE200, `iwlwifi`) and Realtek's `rtw88`/`rtw89` find none and don't come up. | The firmware packages for the certified models' chips; the Wi-Fi pane (CF-12) |
| Camera | No camera driver in the image (`webcamd`). Cameras behind Intel's image processor (most Intel laptops since 2021) have none on this OS at all. | Models with a USB camera, and `webcamd` in the image |
| Graphics | No graphics driver (`drm-kmod`): one screen at the firmware's resolution, no brightness control, no external screens. | `drm-kmod`, and screens beyond the first in the shell (CF-32) |
| Sound | HD Audio and USB sound are checked in the VM by the matrix. A microphone wired through Intel's audio DSP has no driver. | Check each model's built-in microphone |
| Bluetooth | Intel's controllers need firmware (`iwmbt`) that isn't in the image. | The firmware; the Bluetooth pane (CF-13) |
| Touchpad | I²C touchpads need `iichid` and `ig4`, which the base system loads by its device list (`devmatch`); not tried. | The report on each model |

What each would add, from the release's package index (quarterly,
2026-10-24):

| Package | Size | License |
|---|---|---|
| `wifi-firmware-iwlwifi-kmod-22000` (AX200, AX201) | 9.6 MB | Intel's, redistributable |
| `wifi-firmware-iwlwifi-kmod-ax210` (AX210, AX211) | 19.5 MB | Intel's, redistributable |
| `wifi-firmware-iwlwifi-kmod-bz` (BE200) | 14.9 MB | Intel's, redistributable |
| `webcamd` | 5.9 MB | GPL-2.0: unmodified, under ADR 0027's source offer |
| `drm-latest-kmod` | 22.5 MB | BSD-2-Clause, MIT, GPL-2.0 parts |
| `gpu-firmware-intel-kmod-<generation>` | 1.6 to 3.6 MB | Intel's |
| `iwmbt-firmware` | 15.5 MB | Intel's |

An Intel model with AX211 Wi-Fi comes to about 65 MB more.

## Consequences
- Certifying a model is: the report shows nothing unsupported, and a
  person has checked the list in `docs/dev/hardware.md`.
- Four of the findings change the image (firmware, `webcamd`, `drm-kmod`)
  or the base (suspend to idle). They wait for the models to be chosen,
  so the image carries only what those models need.
- `glassctl` depends on `glass-platform`.

## Not covered
- No laptop has run the image yet.
- The report doesn't count screens, docks or Thunderbolt.
- glassd doesn't send the report to the console. A fleet's hardware isn't
  in IT's inventory.
