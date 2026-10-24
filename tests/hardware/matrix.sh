#!/usr/bin/env bash
# Usage: tests/hardware/matrix.sh [profile...]
# Boots the dev VM with emulated hardware, one profile at a time, and checks that
# `glassctl hardware` sees each part as the OS drives it (DK-12): sound with and without a
# microphone, USB sound, Intel's and Realtek's Ethernet, a TPM 2.0, and 2 GB of memory with an
# agent running in it. Sound has to play: what the VM plays lands in a WAV file here, which must
# hold more than silence, and the microphone has to record.
#
# What QEMU can't emulate (Wi-Fi radios, cameras, Bluetooth, modern standby, batteries, GPUs)
# is checked on laptops with `glassctl hardware` (docs/dev/hardware.md).
#
# Each profile restarts the VM; the last step starts it again as `make vm-up` does. Needs the
# current build deployed (`make freebsd vm-deploy`) and, for the TPM, swtpm on this host.
# Writes build/hardware/<profile>.json (the report), .wav (what it played) and <profile>/ (what
# tests/hardware/capture.sh saved: `glassctl hardware --from build/hardware/<profile>` shows it).
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm="$repo/os/freebsd/scripts/vm.sh"
out="$repo/build/hardware"
mkdir -p "$out"
failures=0

# name | QEMU arguments | memory (MB) | what the report must say: kind=status[~a device has this]
profiles=(
	"hda-mic|-audiodev wav,id=snd0,path=$out/hda-mic.wav -device ich9-intel-hda -device hda-micro,audiodev=snd0||audio=works~hdac0"
	"hda-out|-audiodev wav,id=snd0,path=$out/hda-out.wav -device intel-hda -device hda-output,audiodev=snd0||audio=limited~hdac0"
	"usb-audio|-audiodev wav,id=snd0,path=$out/usb-audio.wav -device usb-audio,audiodev=snd0||audio=limited"
	"intel-nic|-netdev user,id=net1 -device e1000e,netdev=net1 -netdev user,id=net2 -device igb,netdev=net2||ethernet=works~em0 ethernet=works~igb0"
	"realtek-nic|-netdev user,id=net1 -device rtl8139,netdev=net1||ethernet=works~rl0"
	"tpm|-chardev socket,id=chrtpm,path=$out/tpm/sock -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-crb,tpmdev=tpm0||tpm=works"
	"small||2048|"
)

log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*"; }
fail() {
	log "FAIL $*"
	failures=$((failures + 1))
}

boot() {
	"$vm" down >/dev/null 2>&1 || true
	"$vm" ssh-close >/dev/null 2>&1 || true
	VM_DEVICES=$1 VM_MEM=${2:-4096} "$vm" up >/dev/null
	"$vm" wait >/dev/null
}

# The report says what `expect` asks: kind=status, and with ~text, a device naming it.
check_report() {
	local name=$1 expect=$2
	python3 - "$out/$name.json" "$expect" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
parts = {p["kind"]: p for p in report["parts"]}
bad = []
for want in sys.argv[2].split():
    kind, rest = want.split("=")
    status, _, device = rest.partition("~")
    part = parts[kind]
    if part["status"] != status:
        bad.append(f"{kind} is {part['status']}, not {status}: {part['note']}")
    elif device and not any(device in d for d in part["devices"]):
        bad.append(f"{kind}: no {device} in {part['devices']}")
for b in bad:
    print(b)
sys.exit(1 if bad else 0)
PY
}

# Sound plays: a second of noise to the default device lands in the WAV file as more than
# silence. And, with a microphone, a second records.
check_sound() {
	local name=$1 record=$2
	: >"$out/$name.wav.before"
	"$vm" ssh 'dd if=/dev/random of=/dev/dsp bs=8000 count=1 2>/dev/null' ||
		{ fail "$name: nothing plays"; return; }
	sleep 2
	python3 - "$out/$name.wav" <<'PY' || fail "$name: the speakers got only silence"
import struct, sys
data = open(sys.argv[1], "rb").read()[44:]
samples = struct.unpack(f"<{len(data) // 2}h", data[: len(data) // 2 * 2])
loud = max((abs(s) for s in samples), default=0)
print(f"played: {len(samples)} samples, loudest {loud}")
sys.exit(0 if loud > 1000 else 1)
PY
	if [[ $record == yes ]]; then
		"$vm" ssh 'timeout 10 dd if=/dev/dsp of=/dev/null bs=8000 count=1 2>&1 | tail -1' ||
			fail "$name: the microphone didn't record"
	fi
	rm -f "$out/$name.wav.before"
}

wanted=("$@")
for profile in "${profiles[@]}"; do
	IFS='|' read -r name devices memory expect <<<"$profile"
	if ((${#wanted[@]})) && [[ " ${wanted[*]} " != *" $name "* ]]; then
		continue
	fi
	log "$name: booting"
	tpm_pid=
	if [[ $name == tpm ]]; then
		command -v swtpm >/dev/null || { log "$name: skipped, no swtpm here (apt-get install swtpm)"; continue; }
		rm -rf "$out/tpm" && mkdir -p "$out/tpm"
		swtpm socket --tpm2 --tpmstate "dir=$out/tpm" --ctrl "type=unixio,path=$out/tpm/sock" \
			--terminate --daemon --pid "file=$out/tpm/pid"
		tpm_pid=$(cat "$out/tpm/pid")
	fi
	rm -f "$out/$name.wav"
	boot "$devices" "$memory"
	"$vm" ssh 'glassctl --json hardware' >"$out/$name.json" 2>/dev/null ||
		{ fail "$name: glassctl hardware failed"; continue; }
	"$vm" scp-in "$repo/tests/hardware/capture.sh" /tmp/capture.sh
	rm -rf "${out:?}/$name"
	"$vm" ssh "rm -rf /tmp/$name && sh /tmp/capture.sh /tmp/$name >/dev/null && tar -C /tmp -cf - $name" |
		tar -C "$out" -xf - || fail "$name: capture.sh failed"
	if [[ -n $expect ]]; then
		check_report "$name" "$expect" || fail "$name: the report doesn't match"
	fi
	case $name in
	hda-mic) check_sound "$name" yes ;;
	hda-out | usb-audio) check_sound "$name" no ;;
	small)
		said=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["memory_mb"])' <"$out/$name.json")
		((said < 2100)) || fail "$name: the report says $said MB"
		# An agent runs in 2 GB.
		id=$("$vm" ssh "su demo -c 'glassctl --json run it-agent \"How much disk is free?\" --yes'" |
			python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])') || id=
		if [[ -z $id ]] || ! "$vm" ssh "su demo -c 'glassctl wait --timeout 300 $id'" >/dev/null; then
			fail "$name: an agent didn't run in 2 GB"
		fi
		;;
	esac
	[[ -n $tpm_pid ]] && { kill "$tpm_pid" 2>/dev/null || true; }
	log "$name: done"
done

log "starting the dev VM as usual"
boot "" ""
if ((failures)); then
	log "hardware matrix: $failures failures"
	exit 1
fi
log "hardware matrix: passed"
