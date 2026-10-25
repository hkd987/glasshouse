# Checking hardware

Glasshouse V1 runs on a short certified list of laptops from one or two
makers (DK-12, PM-10), so what matters is that those models work, part by
part. Three tools check that. ADR 0056 explains the split between them.

| Tool | Runs on | Checks |
|---|---|---|
| `glassctl hardware` | any computer running the image, as root | each part (graphics, Wi-Fi, Ethernet, Bluetooth, camera, sound, touchpad, sleep, battery, TPM) and whether this OS drives it; `--json` for the certified list |
| `tests/hardware/matrix.sh` | the dev VM, with emulated devices | that the report sees what QEMU can emulate (HD Audio with and without a microphone, USB sound, Intel's and Realtek's Ethernet, a TPM 2.0, 2 GB of memory), and that sound reaches the speakers |
| `tests/soak/soak.sh` | the dev VM, for hours | agents working without a break through glassd crashes and power cuts: memory, open files, sandboxes, snapshots and the audit log stay bounded and intact |

## What the VM can't stand in for

QEMU has no Wi-Fi radio, camera, Bluetooth controller, battery, GPU with a
driver, dock or Thunderbolt, and its firmware sleeps only with S3. The
base system's simulated radio (`wtap`) does ad hoc and mesh networks only,
so it can't play a laptop joining an access point (its source:
`ic_caps = IEEE80211_C_MBSS | IEEE80211_C_IBSS`). Those parts are checked on
the laptops themselves.

## Checking a laptop

Not yet tried on a laptop: these steps follow how the dev VM is set up.

1. **The image on a USB stick.** `make image-release` (or a test image),
   then `qemu-img convert -O raw build/images/glasshouse-*.qcow2 /dev/sdX`.
   Turn Secure Boot off in the firmware (the loader isn't signed yet,
   PA-6) and boot from the stick.
2. **A way in as root.** The image has no `sudo`. Plug in Ethernet (a USB
   adapter on thin laptops; the image asks for DHCP on every wired port)
   and give it the dev VM's seed: write `build/vm/seed.iso` to a second
   stick. First boot reads it (label `cidata`) and takes the dev key, so
   `ssh -i build/vm/ssh/id_ed25519 root@<laptop>` works.
3. **The report.** `glassctl hardware` as root, and `glassctl --json
   hardware > <model>.json` for the record. Anything marked ✗ keeps the
   model off the list until fixed; ~ works with the limit it names.
4. **What only a person can check,** each against its requirement:
   - Wi-Fi joins WPA2 and WPA2-Enterprise networks, by hand until the
     Wi-Fi pane exists (CF-10, CF-12);
   - the camera and the headset in a meeting in the browser (DK-11, CF-20);
   - sound from the speakers and the headphone jack, and the built-in
     microphone;
   - closing the lid sleeps it and opening it wakes it, agents paused and
     resumed (PM-1, PM-5), timed;
   - a dock and an external screen (DK-11, CF-32);
   - battery life against Windows on the same model (PM-6).
5. **Sleep cycles (PM-4).** 1,000 cycles need something to wake the
   laptop each time: the base system can't set a wake alarm, so use Wake
   on LAN from another machine (`ifconfig <port> wol_magic`, where the
   model wakes from S3 by its network port), or a USB key presser.

## The pilot's models (ADR 0057)

Laptops 15.1 already drives, whose firmware still offers S3:

1. **Lenovo ThinkPad T14 Gen 2 (Intel)**: set the firmware's Config >
   Power > Sleep State to "Linux S3".
2. **Dell Latitude 7490**: S3 by default.
3. **Lenovo ThinkPad X1 Carbon Gen 9**: "Linux S3", as the T14 Gen 2.

Check one unit of each with `capture.sh` from 15.1's own USB stick before
buying more.

## Kernel patches (ADR 0057)

A hardware fix is a patch to the base system's source:
`os/freebsd/patches/<id>/` holds `patch.diff` and `patch.toml`, which says
what it fixes and what it rebuilds: modules, and the kernel when the code
it changes is built into it (`kernel = true`).

```sh
os/freebsd/scripts/kernel-patch.sh key      # the development signing key, once
os/freebsd/scripts/kernel-patch.sh build os/freebsd/patches/lkpi-wlan-suspend
# On the computer, as root:
glassctl hardware patch apply lkpi-wlan-suspend.bundle.json   # tried at the next start, once
glassctl hardware patch status                                # on probation: minutes of use so far
glassctl hardware patch cancel                                # before restarting
glassctl hardware patch undo                                  # on probation or kept: back to the one before
```

What the build does:
- builds in the dev VM, which stands in for the build service, from the
  release's own source (`zroot/kpatch/src@15.1-RELEASE`);
- for a kernel patch, starts from the release's kernel built once
  (`zroot/kpatch/kernel@15.1-RELEASE`: about two hours in the emulated
  VM), so the patch rebuilds only the files it changes and those that
  include them;
- signs the bundle with `build/kpatch/dev-key`.

A computer takes bundles signed by a key its `glassd.toml` names under
`[hardware] keys`. The image names none.

When the computer starts on the patch, glassd checks it and puts it on
probation, or says it failed. On probation, a clean restart comes back to
it, and a crash, panic or power cut starts the environment before; after a
working day of use (`[hardware] probation_minutes`) it's kept. A patch that
rewrites a file a kept one rewrote must say it was built on top of it
(`includes = ["<id>"]` in `patch.toml`), or it's refused.

`tests/hardware/patch-vm.sh` runs it end to end in the VM: the Wi-Fi fix
through probation to kept and out again, and patches that panic as they
load, panic three minutes later, or hang, plus the bundles the VM must
refuse.

## Known before any laptop is tried

From the image's contents and the base system's source (ADR 0056, 0058):

- **Sleep:** this OS version (15.1) sleeps with S3 only; suspend to idle
  is in the base system's development branch. Laptops whose firmware
  offers only modern standby can't sleep at all.
- **Wi-Fi:** the image carries firmware for Intel's cards: the older ones
  (`iwm`, up to the 8265) and, since ADR 0058, Wi-Fi 6 and 6E on
  `iwlwifi` (the 9000 series, AX200, AX201, AX210, AX211). Wi-Fi 7 (BE200)
  and Realtek's `rtw88`/`rtw89` still need firmware it doesn't have.
- **Cameras:** USB (UVC) cameras work through `webcamd`, which devd starts
  as one attaches. The cameras behind Intel's image processor (IPU6, most
  Intel laptops since 2021) have no driver on this OS.
- **Graphics:** Intel's driver (`i915kms`) and its firmware are in the
  image, off until the model is checked with it: set
  `glasshouse_gpu_enable=YES` (`sysrc`) on the unit, restart, and check
  the sign-in screen and desktop still draw, at the panel's resolution.
  Without it: one screen, at the resolution the firmware set.
- **Sound:** HD Audio works; laptops that route their microphone through
  Intel's audio DSP have no driver for it.
- **Bluetooth:** Intel's controllers' firmware (`iwmbt`) is in the image,
  loaded by `iwmbtfw` as the controller attaches.
