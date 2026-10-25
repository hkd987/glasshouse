#!/bin/sh
# Usage: capture.sh [dir]
# Saves what `glassctl hardware` reads, one file per tool, on a computer running this OS's base
# system: the image, or the stock release's USB stick booted to its live shell (write to the stick
# or /tmp there; / is read only). As root, or USB devices and the battery are left out. Then judge
# it anywhere, the dev host included: `glassctl hardware --from <dir>` (docs/dev/hardware.md).
# Serial numbers and the machine's UUID are left out; the kernel's messages keep MAC addresses.
set -eu
model=$(kenv -q smbios.system.product 2>/dev/null | tr -c 'A-Za-z0-9\n' '-')
dir=${1:-hardware-${model:-computer}}
mkdir -p "$dir"
kenv | grep -v -e serial -e uuid >"$dir/kenv" || :
sysctl -n hw.model hw.physmem >"$dir/sysctl"
pciconf -lv >"$dir/pciconf"
devinfo -v >"$dir/devinfo"
sysctl -n net.wlan.devices >"$dir/wlan" 2>/dev/null || :
cat /dev/sndstat >"$dir/sndstat" 2>/dev/null || :
sysctl -n hw.acpi.supported_sleep_state >"$dir/sleep" 2>/dev/null || :
sysctl -n hw.dri.0.name >"$dir/dri" 2>/dev/null || :
# The firmware's brightness levels (ADR 0060), once acpi_video is loaded.
kldload -n acpi_video 2>/dev/null || :
sysctl -n hw.acpi.video.lcd0.levels >"$dir/backlight" 2>/dev/null || :
ls /dev >"$dir/dev"
{
	cat /var/run/dmesg.boot 2>/dev/null || :
	dmesg
} >"$dir/dmesg"
if [ -d /usr/local/share/iwmbt-firmware ]; then : >"$dir/iwmbt-firmware"; fi
if [ "$(id -u)" = 0 ]; then
	usbconfig dump_curr_config_desc >"$dir/usbconfig" 2>/dev/null || :
	acpiconf -i 0 >"$dir/battery" 2>/dev/null || :
fi
echo "$dir"
