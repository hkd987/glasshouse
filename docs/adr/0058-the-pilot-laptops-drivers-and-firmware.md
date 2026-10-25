# 0058. The pilot laptops' drivers and firmware

Status: **Accepted, built** (2026-10-25). The owner, after the gaps ADR 0056
and 0057 left: "All the above let's get them all done."

## Context
- ADR 0056 found what the image lacked on current laptops: firmware for
  Intel's Wi-Fi 6 and 6E cards, a camera driver, a graphics driver, and
  Intel's Bluetooth firmware. ADR 0057 picked the pilot's models: the
  ThinkPad T14 Gen 2 Intel (AX210 Wi-Fi, Iris Xe, a USB camera), the Dell
  Latitude 7490 (8265 or 9560, UHD 620, a USB camera) and the ThinkPad X1
  Carbon Gen 9 (AX201, Iris Xe).
- Without them the first pilot laptop has no Wi-Fi at all, Meet has no
  camera, and the screen has no brightness control beyond the firmware's
  own keys.
- All of it exists as FreeBSD packages built for 15.1. None of it needs a
  kernel patch.
- Two kinds of license are new to the image:
  - **firmware**, which runs on the device and not on the computer's own
    processors, under its maker's terms;
  - **GPL-2.0 kernel modules and programs** (drm-kmod, webcamd). ADR 0027
    already ships GPL-2.0 libraries unmodified with their source on offer,
    and its owner question 1 is still open.

## Decision
1. **What the image adds** (`os/freebsd/images/hardware.third-party`, 64 MB):

   | Package | For | Size |
   |---|---|---|
   | `wifi-firmware-iwlwifi-kmod-22000`, `-ax210`, `-9000` | Intel Wi-Fi 6 and 6E: AX200, AX201, AX210, AX211 and the 9560 | 32 MB |
   | `iwmbt-firmware` | Intel Bluetooth on the same cards, loaded by the base system's `iwmbtfw` as the controller attaches | 15 MB |
   | `webcamd` | USB cameras (UVC) as V4L2 devices, for calls in the browser engine | 6 MB |
   | `drm-612-kmod` (`drm`, `ttm`, `dmabuf`, `i915kms` only) and Intel's GPU firmware for Tiger Lake and Kaby Lake | Intel graphics | 11 MB |

   The base packages gain `FreeBSD-libcuse` (webcamd's) and
   `FreeBSD-bluetooth` with its libraries (`iwmbtfw`, `hccontrol`,
   `bthidd`).
2. **Firmware ships unmodified under its maker's redistribution terms**,
   with the license in the notices (`tools/legal/notices.py`,
   `IMAGE_FIRMWARE`). Only Intel's is allowed so far; any other maker's
   firmware is refused until it is reviewed. The Wi-Fi packages call
   Intel's license "primary", and the list corrects them.
3. **The graphics and camera drivers ship under ADR 0027's GPL-2.0 rule:**
   unmodified, as separate files, with their source (the ports'
   distribution files) on offer. The offer now says "libraries, programs
   and kernel modules". ADR 0027's open owner question 1 covers these too.
4. **Cameras:**
   - devd starts webcamd as a USB camera attaches. The image ships its own
     rule (`devd/glasshouse-camera.conf`), for video interfaces only;
     webcamd's own rules also take every USB device of no class.
   - The device is open to anyone on the computer, as the microphone
     already is (`/dev/dsp`): the image's devfs ruleset sets `video*` to
     0666. The browser engine asks each site before it uses the camera.
   - Agents never see it: their jails' devfs ruleset (4) shows no hardware.
     The containment probe gains "dev: see cameras, the GPU, input or
     disks", 30 checks now. It must fail on the insecure host backend and
     pass in a jail.
5. **Graphics is off until a model is checked with it.**
   - `rc.d/glasshouse_gpu` loads `i915kms` on Intel graphics once
     `glasshouse_gpu_enable` is set. That is a per-model setting, after
     the model is checked on a unit (`docs/dev/hardware.md`).
   - The desktop draws on the console framebuffer (ADR 0016). Whether it
     keeps drawing once the DRM driver takes the screen over can only be
     seen on a real Intel laptop: the VM's emulated graphics aren't
     Intel's.
   - post-install indexes `/boot/modules` (`kldxref`), as a package
     install would. Without the index, `i915kms` can't find `drm` by its
     module name.
6. **`glassctl hardware` judges what the image carries.** Graphics says
   the driver is in the image and off until the model is checked. A USB
   camera without its device says webcamd isn't running for it, rather
   than that no driver exists.

## What the VM shows, and what it can't
- **Shown in the VM:**
  - every file is in place;
  - `i915kms` loads (and attaches to nothing);
  - `cuse` loads;
  - the camera's rc settings and devfs rule apply;
  - the containment probe passes 30 of 30;
  - the e2e suite passes.
- **Needs a laptop:** QEMU 8.2 emulates no Intel Wi-Fi, Bluetooth, USB
  camera or Intel graphics. The bring-up kit checks them on a unit.

## Consequences
- The image grows by 64 MB.
- Firmware updates arrive with the ports' quarterly packages, like the
  browser engine's (CF-16).
- A model joins the certified list once its unit has passed the hardware
  checklist with the graphics driver on. Until then it draws on the
  firmware's framebuffer.
