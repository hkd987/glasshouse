#!/bin/sh
# Usage: bringup.sh [--out DIR] [--cycles N] [--auto] [--ssid NAME]
# The pilot laptop bring-up kit (ADR 0056, ADR 0058, docs/dev/hardware.md): checks a laptop
# running this image part by part, asking the person at it what only a person can tell (a sound
# heard, the screen dimmer, the lid), and writes what it found to DIR (default
# /var/db/glasshouse-bringup/<model>-<date>): report.md for people, results.tsv, and what
# `glassctl hardware` and capture.sh read, for the certified list (DK-12).
#
# As root. --auto asks nothing and skips what needs a person. --cycles N sleeps and wakes N times
# (default 3; each wake is the person opening the lid or pressing a key, as the base system can't
# set a wake alarm). --ssid joins that Wi-Fi network (the password is asked, never shown or kept).
set -u

out=
cycles=3
auto=
ssid=
while [ $# -gt 0 ]; do
	case $1 in
	--out) out=$2; shift 2 ;;
	--cycles) cycles=$2; shift 2 ;;
	--auto) auto=1; shift ;;
	--ssid) ssid=$2; shift 2 ;;
	*) sed -n '2,/^set -u/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2; exit 2 ;;
	esac
done
[ "$(id -u)" = 0 ] || { echo "bringup: as root" >&2; exit 2; }

model=$(kenv -q smbios.system.product 2>/dev/null | tr -c 'A-Za-z0-9\n' '-')
maker=$(kenv -q smbios.system.maker 2>/dev/null)
out=${out:-/var/db/glasshouse-bringup/${model:-computer}-$(date +%Y%m%d-%H%M)}
mkdir -p "$out"
results=$out/results.tsv
: >"$results"
passed=0 failed=0 skipped=0

say() { printf '%s\n' "$*"; }
# result <part> <PASS|FAIL|SKIP> <what was found>
result() {
	printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$results"
	case $2 in
	PASS) passed=$((passed + 1)); say "  ✓ $1: $3" ;;
	FAIL) failed=$((failed + 1)); say "  ✗ $1: $3" ;;
	*) skipped=$((skipped + 1)); say "  – $1: $3" ;;
	esac
}
# ask <question>: yes (0) or no (1); never asked with --auto (2).
ask() {
	[ -n "$auto" ] && return 2
	while :; do
		printf '  ? %s [y/n] ' "$1"
		read -r answer </dev/tty || return 2
		case $answer in
		[Yy]*) return 0 ;;
		[Nn]*) return 1 ;;
		esac
	done
}
# asked <part> <question> <what a yes means> <what a no means>
asked() {
	ask "$2"
	case $? in
	0) result "$1" PASS "$3" ;;
	1) result "$1" FAIL "$4" ;;
	*) result "$1" SKIP "needs a person (run without --auto)" ;;
	esac
}
pause() {
	[ -n "$auto" ] && return 0
	printf '  … %s, then Enter ' "$1"
	read -r _ </dev/tty
}

# What's saved is read later, not on this screen: no colors in it.
export NO_COLOR=1
say "Bring-up of ${maker:-a computer} ${model:-}, into $out"

say "1. What this OS makes of each part"
glassctl --json hardware >"$out/hardware.json" 2>/dev/null
glassctl hardware >"$out/hardware.txt" 2>&1
/usr/local/libexec/glasshouse/capture.sh "$out/capture" >/dev/null 2>&1 || :
blockers=$(grep -c '^✗' "$out/hardware.txt" 2>/dev/null)
if [ "${blockers:-0}" = 0 ]; then
	result report PASS "no part this OS can't drive (hardware.txt)"
else
	result report FAIL "$blockers part(s) marked ✗ in hardware.txt"
fi

say "2. Wi-Fi"
radio=$(sysctl -n net.wlan.devices 2>/dev/null)
if [ -z "$radio" ]; then
	result wifi FAIL "no radio a driver attached (dmesg for its firmware)"
else
	ifconfig wlan0 >/dev/null 2>&1 || { sysrc -q "wlans_${radio%% *}=wlan0" >/dev/null; service netif start wlan0 >/dev/null 2>&1; }
	wpa_cli -i wlan0 scan >/dev/null 2>&1
	sleep 6
	seen=$(wpa_cli -i wlan0 scan_results 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
	if [ "${seen:-0}" -gt 0 ]; then
		result wifi-scan PASS "$radio sees $seen network(s)"
	else
		result wifi-scan FAIL "$radio sees no networks"
	fi
	if [ -n "$ssid" ] && [ -z "$auto" ]; then
		stty -echo </dev/tty
		printf '  ? password for %s (nothing for an open network): ' "$ssid"
		read -r password </dev/tty
		stty echo </dev/tty
		echo
		id=$(wpa_cli -i wlan0 add_network | tail -1)
		hex=$(printf '%s' "$ssid" | od -An -tx1 | tr -d ' \n')
		wpa_cli -i wlan0 set_network "$id" ssid "$hex" >/dev/null
		if [ -n "$password" ]; then
			wpa_cli -i wlan0 set_network "$id" psk "\"$password\"" >/dev/null
		else
			wpa_cli -i wlan0 set_network "$id" key_mgmt NONE >/dev/null
		fi
		password=
		wpa_cli -i wlan0 select_network "$id" >/dev/null
		joined=
		for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
			sleep 5
			if wpa_cli -i wlan0 status | grep -q '^wpa_state=COMPLETED'; then joined=1; break; fi
		done
		if [ -n "$joined" ] && ping -c 3 -t 10 -o 1.1.1.1 >/dev/null 2>&1; then
			result wifi-join PASS "joined $ssid and reached the internet"
		else
			result wifi-join FAIL "didn't join $ssid ($(wpa_cli -i wlan0 status | sed -n 's/^wpa_state=//p'))"
		fi
		wpa_cli -i wlan0 remove_network "$id" >/dev/null
	else
		result wifi-join SKIP "no --ssid to join"
	fi
fi

say "3. Bluetooth"
if dmesg | grep -q 'ubt[0-9]'; then
	if hccontrol -n ubt0hci read_bd_addr >/dev/null 2>&1; then
		result bluetooth PASS "controller answers ($(hccontrol -n ubt0hci read_bd_addr 2>/dev/null | tail -1))"
	else
		result bluetooth FAIL "a controller attached but doesn't answer (its firmware: dmesg | grep iwmbt)"
	fi
else
	result bluetooth SKIP "no Bluetooth controller attached"
fi

say "4. Camera"
if ls /dev/video* >/dev/null 2>&1; then
	result camera PASS "$(echo /dev/video*) (webcamd)"
	asked camera-call "In a meeting in the browser, does the camera show you?" "the camera works in a call" "the camera doesn't show in a call"
elif usbconfig 2>/dev/null | grep -qi camera || usbconfig dump_curr_config_desc 2>/dev/null | grep -q 'bInterfaceClass = 0x000e'; then
	result camera FAIL "a USB camera, and no /dev/video (service webcamd status)"
else
	result camera SKIP "no USB camera found"
fi

say "5. Sound"
if grep -q '^pcm' /dev/sndstat 2>/dev/null; then
	result sound-device PASS "$(grep -m1 default /dev/sndstat 2>/dev/null || grep -m1 '^pcm' /dev/sndstat)"
	pause "Turn the volume up"
	dd if=/dev/random of=/dev/dsp bs=8000 count=2 2>/dev/null
	asked speakers "Did you hear a burst of noise from the speakers?" "the speakers play" "nothing from the speakers"
	if dd if=/dev/dsp of="$out/mic.raw" bs=8000 count=3 2>/dev/null &&
		[ "$(od -An -tu1 "$out/mic.raw" | tr -s ' ' '\n' | sort -u | wc -l)" -gt 8 ]; then
		result microphone PASS "three seconds recorded, not silence"
	else
		result microphone FAIL "the recording is silence"
	fi
	rm -f "$out/mic.raw"
else
	result sound-device FAIL "no sound device (/dev/sndstat)"
fi

say "6. Graphics and the screen"
if sysctl -n hw.dri.0.name >/dev/null 2>&1; then
	result graphics PASS "its driver is loaded ($(sysctl -n hw.dri.0.name))"
elif ask "Try Intel's graphics driver now? The screen may go black; the desktop is checked after"; then
	service glasshouse_gpu onestart >/dev/null 2>&1
	sleep 3
	if sysctl -n hw.dri.0.name >/dev/null 2>&1; then
		result graphics PASS "i915kms attached; glasshouse_gpu_enable=YES makes it stay"
		asked graphics-desktop "Does the desktop still show and redraw?" "the desktop draws with the driver" "the desktop stopped drawing with the driver"
	else
		result graphics FAIL "i915kms didn't attach (dmesg | grep drm)"
	fi
else
	result graphics SKIP "the graphics driver left off"
fi
before=$(glassctl display brightness 2>/dev/null)
case $before in
*%*)
	glassctl display brightness 30 >/dev/null
	asked brightness "Is the screen dimmer now?" "Settings' brightness dims the screen" "the screen didn't dim"
	glassctl display brightness "${before##* }" >/dev/null
	pause "Press the brightness keys up and down a few times"
	asked brightness-keys "Did it step once a press, with the level on screen?" "the brightness keys step once a press" "the brightness keys did nothing, or stepped twice"
	;;
*) result brightness FAIL "the screen's brightness isn't set from here (no backlight, no firmware levels)" ;;
esac

say "7. Watchdog, battery, TPM"
if [ -e /dev/fido ]; then
	result watchdog PASS "$(dmesg | grep -m1 'ichwd0:' | sed 's/^ichwd0: //')"
else
	result watchdog FAIL "no hardware watchdog: a kernel patch that hangs this model needs the power button"
fi
if battery=$(acpiconf -i 0 2>/dev/null) && [ -n "$battery" ]; then
	result battery PASS "$(echo "$battery" | grep -m1 'Remaining capacity' | sed 's/.*:[[:space:]]*//') left, $(echo "$battery" | grep -m1 'State' | sed 's/.*:[[:space:]]*//')"
else
	result battery FAIL "no battery reported"
fi
if [ -e /dev/tpm0 ]; then result tpm PASS "TPM 2.0 at /dev/tpm0"; else result tpm FAIL "no TPM"; fi

say "8. Sleep and wake, $cycles time(s)"
if ! sysctl -n hw.acpi.supported_sleep_state 2>/dev/null | grep -q S3; then
	result sleep FAIL "the firmware offers no S3 (Linux S3 in the firmware's power settings)"
elif [ -n "$auto" ]; then
	result sleep SKIP "each wake needs a person (run without --auto)"
else
	ok=0
	i=0
	while [ "$i" -lt "$cycles" ]; do
		i=$((i + 1))
		say "  sleep $i of $cycles: close the lid or wait; open it or press a key to wake"
		start=$(date +%s)
		acpiconf -s 3
		sleep 5
		woke=$(($(date +%s) - start))
		if [ -n "$radio" ]; then
			for _ in 1 2 3 4 5 6; do
				wpa_cli -i wlan0 status 2>/dev/null | grep -q '^wpa_state=COMPLETED' && break
				sleep 5
			done
			wpa_cli -i wlan0 status 2>/dev/null | grep -q '^wpa_state=COMPLETED' ||
				say "    Wi-Fi didn't come back after wake $i"
		fi
		ok=$((ok + 1))
		say "    awake ($woke s asleep and back)"
	done
	asked sleep "Did each wake bring back the screen, keyboard and touchpad?" "$ok of $cycles sleeps woke whole" "a wake didn't come back whole"
fi

say "9. Kernel patches"
glassctl hardware patch status >"$out/patches.txt" 2>&1 || :
result patches PASS "$(head -1 "$out/patches.txt")"

{
	echo "# Bring-up: ${maker:-} ${model:-}"
	echo
	echo "$(date -u '+%Y-%m-%d %H:%M UTC'), $(uname -r). $passed passed, $failed failed, $skipped skipped."
	echo
	echo "| Part | Result | What was found |"
	echo "|---|---|---|"
	awk -F '\t' '{ for (i = 1; i <= 3; i++) gsub(/\|/, "\\|", $i); printf "| %s | %s | %s |\n", $1, $2, $3 }' "$results"
	echo
	echo "Also here: hardware.txt and hardware.json (\`glassctl hardware\`), capture/ (\`glassctl hardware --from\`), patches.txt."
} >"$out/report.md"
say ""
say "$passed passed, $failed failed, $skipped skipped: $out/report.md"
[ "$failed" = 0 ]
