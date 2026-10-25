# 0062. Bluetooth in Settings

Status: **Accepted, built** (2026-10-25); pairing on a laptop is on the
pilot's checklist. CF-13. The owner: "All the above let's get them all done."

## Context
- CF-13 asks for a Bluetooth pane that pairs, lists and removes keyboards,
  mice and other classic devices. IT can turn Bluetooth off, or allow only
  input devices. Headsets and Bluetooth LE need the base system's work
  first.
- The base system has the classic stack:
  - `ng_ubt`, the USB controller, which devd brings up as it attaches
    (`service bluetooth start ubt0`: its HCI and L2CAP nodes);
  - `hccontrol`, for inquiries, names and connections;
  - `hcsecd`, which answers pairing with a PIN and keeps link keys;
  - `bthidd`, which connects keyboards and mice. `bthidcontrol query`
    reads a device's HID descriptor into its configuration, and `-u` sends
    the input through evdev, as the desktop reads it.
- ADR 0058 put Intel's Bluetooth firmware (`iwmbt`) and the stack's tools
  in the image.
- The VM can't show a pairing: QEMU emulates no Bluetooth controller.

## Decision
1. **glassd drives the base system's stack**
   (`glass_platform::bluetooth::Controller`):
   - an inquiry and each device's name (`hccontrol`);
   - its kind, from its Class of Device;
   - the connections open.
2. **Pairing.** A device is paired only as the last search found it: the
   request names its address, never what it is. Settings gives a PIN of
   six random digits. glassd writes it into `hcsecd.conf` (root's, mode
   600) and restarts hcsecd. For an input device, it also writes the block
   `bthidcontrol query` reads into `bthidd.conf` and restarts bthidd. The
   person types the PIN on the device. The PIN is in what Settings shows,
   never in a result's text.
3. **What glassd writes is marked** (`# glasshouse class 0x002540` in each
   block). It lists and removes only its own blocks, and what IT or a
   person wrote by hand stays. A name can't break out of its quotes: only
   plain characters are kept.
4. **Removing** drops the device's two blocks and its link key from
   `/var/db/hcsecd.keys`.
5. **IT's rules**, device settings that the owner of an unmanaged computer
   sets:
   - `bluetooth.enabled`: off brings the stack down (`service bluetooth
     stop`). glassd checks every 30 seconds, for a controller plugged in
     since or a stack devd started again. Paired devices are kept.
   - `bluetooth.input_only`: only keyboards, mice and other input devices
     pair.

   Headphones and speakers are refused either way: the base system has no
   Bluetooth audio.
6. **The person's own.** All five actions are Never for agents:
   - `settings.bluetooth`, `settings.bluetooth_scan`;
   - `settings.bluetooth_pair`, the person's own (`PERSONS_OWN`), because a
     paired keyboard types into everything;
   - `settings.bluetooth_remove`, `settings.bluetooth_power`.

   The Settings app also refuses any caller but itself.
7. **The image** enables hcsecd, which runs with no controller, waiting
   for one. glassd enables bthidd as the first input device is paired, and
   disables it as the last is removed: in the VM, bthidd refused an empty
   `bthidd.conf` at boot ("syntax error in line 1"). Its rc script passes
   `-u`, and uinput is in the kernel.
8. **A stand-in.** `[bluetooth] stand_in` names a JSON file that stands in
   for a controller. The dev host has a keyboard, a mouse, a phone and
   headphones nearby, and the apps' browser suite pairs the keyboard
   (`settings.spec.ts`).

## Consequences
- **Legacy PIN pairing only.** hcsecd answers PIN requests: keyboards and
  mice that ask for a typed PIN pair. A device that insists on Secure
  Simple Pairing's numeric comparison may not, and the pilot's checklist
  tries the laptops' own keyboards and mice.
- **The PIN stays in `hcsecd.conf`** after pairing, root's alone; the link
  key is what reconnects the device.
- **Headsets and Bluetooth LE** wait for the base system (PM-10).
