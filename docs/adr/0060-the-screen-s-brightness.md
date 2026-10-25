# 0060. The screen's brightness

Status: **Accepted, built** (2026-10-25). The owner, after the gaps ADR 0056
to 0058 left: "All the above let's get them all done."

## Context
- ADR 0056 found the screen had no brightness control beyond the firmware's
  own keys. On a laptop that is the control people reach for most after
  the volume.
- The base system has two ways to set it:
  - **backlight(8)**, for a backlight a graphics driver gives
    (`/dev/backlight/backlight0`). i915kms gives one, and ADR 0058 leaves it
    off until a model is checked with it.
  - **acpi_video(4)**, the firmware's own levels (`hw.acpi.video.lcd0`, a
    percent each), which most laptops have without the graphics driver.
    acpi_video also takes the firmware's brightness-key events and steps
    the level itself.
- Some keyboards send the brightness keys as keys instead (evdev's
  `KEY_BRIGHTNESSDOWN` and `KEY_BRIGHTNESSUP`, 224 and 225).
- The VM has neither: QEMU's screen has no backlight and its firmware no
  levels.
- No PRD requirement names brightness. It belongs with UX-1 (Settings) and
  DK-12 (what a pilot laptop must do).

## Decision
1. **`glass_platform::backlight`** uses backlight(8)'s first device, else
   acpi_video's levels (the two `_BCL` defaults left out), through the
   base system's `backlight` and `sysctl`. Brightness is a percent.
2. **Never dark from here.** 5% is the least it's set to, from Settings, a
   key, `glassctl` or an agent IT allowed. A panel at 0 is off on many
   laptops, and what agents do on the screen has to stay visible.
3. **The keys step it** by 10% on a backlight(8) device, or to the
   firmware's next level. They work on the lock screen too. The desktop
   shows the level for 1.5 seconds, low on the screen, as a Mac does.
4. **One path in glassd.** The `SettingsHandle` holds the backlight. Three
   ways reach it:
   - the Settings app's `settings.brightness` and
     `settings.brightness_set`;
   - the desktop's `display.brightness` on the control socket, which only
     the person at the screen (the seat) or root may call;
   - `glassctl display brightness [up|down|<percent>]`.
5. **Kept for the next start.** glassd writes the level to its state
   directory with each change, and sets it again when it starts. The
   firmware starts the screen at full.
6. **Whose it is.** Agents get Never by default. IT may let one dim the
   screen, never below 5%. The Settings app gets Auto, and its changes are
   on the audit record like any action. A brightness key press isn't
   recorded, as a key press isn't.
7. **acpi_video loads at startup** (post-install's `kld_list`, as the TPM's
   driver does). On a computer whose firmware has no levels it attaches
   to nothing.
8. **`glassctl hardware` says which it is:** the graphics driver's
   backlight, the firmware's levels, or neither. `capture.sh` saves the
   firmware's levels from a stock stick.

## How it's tested
- Level parsing, the floor, stepping and choosing a device: unit tests
  against the formats the base system prints.
- The handle and actions against a stand-in file (`Backlight::stand_in`).
- The control method's seat check, in the runtime's tests.
- The keys and the overlay in the shell's tests and `make shell-preview`
  (`brightness.png`).
- The slider in the apps' browser suite (`settings.spec.ts`).
  `make dev-host` sets `[display] backlight` to a file.
- A real backlight is on the pilot checklist (`docs/dev/hardware.md`).

## Consequences
- A pilot laptop dims from Settings and its keys with the graphics driver
  off, wherever its firmware has levels, and through the driver once it's
  on.
- **Possible double steps.** A laptop whose firmware and keyboard both send
  a brightness key could step twice a press. Linux has a switch for this;
  15.1 has none. The pilot checklist looks for it.
- **No automatic brightness.** There's no dimming on battery beyond what
  acpi_video does on a power change, and no ambient light sensor.
