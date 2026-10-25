#!/usr/bin/env bash
# Usage: vm.sh <command> [args]
#   up                 boot the dev VM (creates overlay disk + seed ISO on first use)
#   down               clean shutdown (ssh poweroff), force-kill after a timeout
#   status             show whether QEMU runs and SSH answers (exit 1 if stopped)
#   wait [secs]        block until SSH answers and first-boot provisioning finished
#   ssh [args]         interactive root shell, or run ssh args in the VM
#   ssh-close          drop the shared SSH connection (after a reboot)
#   run <cmd...>       run a shell command as root in the VM
#   scp-in <src...> <dest>   copy host files into the VM
#   exec <binary> [args]     copy a host binary into the VM and run it (usable as a cargo runner)
#   reset              stop and delete the overlay + seed (next `up` is a fresh first boot)
#   power-cut          restart the VM at once, as if unplugged (nothing shut down or written first)
#   sleep-wake [secs]  the guest sleeps (suspend to RAM, as a closed lid), then wakes after secs
#                      (default 5), as an opened lid; waits until SSH answers again
#   clear-snapshots    destroy the tasks' `@glass-` snapshots test runs piled up (they slow every
#                      snapshot, docs/dev/performance.md), with glassd stopped meanwhile
#   console-log [-f]   print (or follow) the serial console log
#   screendump <out.png>     save what the VM's display shows right now (boot screens, splash)
#   point <x> <y>            move the pointer to screen pixel (x, y) (the USB tablet)
#   click <x> <y>            ... and click there
#   drag <x> <y> <x2> <y2>   press at (x, y), move to (x2, y2) in steps, release there
#   key <key>...             press keys, QEMU names: ctrl-spc, ret, esc, a, shift-a
#   keydown|keyup <key>      one key down or up on its own (keydown alt, key tab: Alt Tab's switcher)
#   scroll up|down [n]       turn the wheel n notches where the pointer is
#   type <text>              type text on the USB keyboard (letters, digits, space, . , - / :)
#   usb-stick plug|unplug    plug a 16 MB USB drive into the running VM, or pull it out (tests of
#                            IT's devices.usb_storage setting, ADR 0031)
# Env knobs: VM_DIR VM_SSH_PORT VM_CPUS VM_CPU_MODEL VM_ACCEL VM_MEM VM_DISK_SIZE VM_BOOT_TIMEOUT VM_BASE_IMAGE
#   VM_FIRMWARE=uefi|bios VM_SCREEN VM_EXTRA_DISK (see lib.sh), VM_DEVICES (more QEMU arguments:
#   emulated hardware, as tests/hardware/matrix.sh adds).
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

make_seed() {
	local tmp
	tmp=$(mktemp -d)
	printf 'instance-id: glasshouse-dev-%s\nlocal-hostname: glasshouse-dev\n' "$(date +%s)" >"$tmp/meta-data"
	sed "s|@SSH_PUBKEY@|$(cat "$VM_SSH_KEY.pub")|" "$FREEBSD_OS_DIR/vm/user-data.in" >"$tmp/user-data"
	xorriso -as mkisofs -o "$VM_SEED" -V cidata -J -r "$tmp/meta-data" "$tmp/user-data" 2>"$tmp/log" ||
		die "seed ISO creation failed: $(cat "$tmp/log")"
	rm -rf "$tmp"
}

cmd_up() {
	if vm_running; then
		log "already running (pid $(vm_pid))"
		return
	fi
	vm_ssh_close
	need qemu-system-x86_64 qemu-img xorriso ssh-keygen
	[[ -n ${VM_BASE_IMAGE:-} ]] || "$FREEBSD_SCRIPTS_DIR/fetch.sh"
	mkdir -p "$VM_DIR" "$(dirname "$VM_SSH_KEY")"
	if [[ ! -f $VM_SSH_KEY ]]; then
		ssh-keygen -q -t ed25519 -N '' -C glasshouse-dev-vm -f "$VM_SSH_KEY"
	fi
	if [[ ! -f $VM_DISK ]]; then
		log "creating overlay ${VM_DISK#"$REPO_ROOT"/} ($VM_DISK_SIZE) on ${BASE_IMAGE##*/}"
		# Relative backing path keeps the overlay valid if the checkout moves.
		qemu-img create -q -f qcow2 -F qcow2 \
			-b "$(realpath --relative-to="$VM_DIR" "$BASE_IMAGE")" "$VM_DISK" "$VM_DISK_SIZE"
		make_seed
	fi
	[[ -f $VM_CONSOLE ]] && mv -f "$VM_CONSOLE" "$VM_CONSOLE.prev"
	local firmware=()
	if [[ $VM_FIRMWARE == uefi ]]; then
		[[ -f $OVMF_CODE ]] || die "VM_FIRMWARE=uefi needs OVMF ($OVMF_CODE): apt-get install ovmf"
		# Fresh each start: stored boot entries name PCI slots, and go stale
		# when the device list changes.
		cp "$OVMF_VARS" "$VM_EFIVARS"
		firmware=(-drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_CODE"
			-drive "if=pflash,format=raw,unit=1,file=$VM_EFIVARS")
	fi
	# A second disk for the guest to write (the package-list image build), after the seed: vtbd2.
	local extra=()
	if [[ -n ${VM_EXTRA_DISK:-} ]]; then
		[[ -f $VM_EXTRA_DISK ]] || die "VM_EXTRA_DISK $VM_EXTRA_DISK doesn't exist"
		extra=(-drive "if=virtio,file=$VM_EXTRA_DISK,format=qcow2,discard=unmap")
	fi
	# More emulated hardware (tests/hardware/matrix.sh): QEMU arguments, split on spaces.
	local devices=()
	[[ -n ${VM_DEVICES:-} ]] && read -ra devices <<<"$VM_DEVICES"
	rm -f "$VM_MONITOR" "$VM_QMP"
	qemu-system-x86_64 \
		-name glasshouse-dev -machine q35 -accel "$VM_ACCEL" -cpu "$VM_CPU_MODEL" \
		-smp "$VM_CPUS" -m "$VM_MEM" \
		-drive "if=virtio,file=$VM_DISK,format=qcow2,discard=unmap" \
		-drive "if=virtio,file=$VM_SEED,format=raw,readonly=on" "${extra[@]}" \
		-netdev "user,id=net0,hostname=glasshouse-dev,hostfwd=tcp:127.0.0.1:$VM_SSH_PORT-:22" \
		-device virtio-net-pci,netdev=net0 -device virtio-rng-pci \
		-vga none -device "VGA,xres=${VM_SCREEN%x*},yres=${VM_SCREEN#*x}" \
		-device qemu-xhci -device usb-kbd -device usb-tablet \
		"${firmware[@]}" "${devices[@]}" \
		-display none -monitor "unix:$VM_MONITOR,server,nowait" -qmp "unix:$VM_QMP,server,nowait" \
		-serial "file:$VM_CONSOLE" \
		-daemonize -pidfile "$VM_PIDFILE"
	log "started (pid $(vm_pid), $VM_FIRMWARE), ssh on 127.0.0.1:$VM_SSH_PORT; next: vm.sh wait"
}

cmd_down() {
	if ! vm_running; then
		log "not running"
		rm -f "$VM_PIDFILE"
		return
	fi
	local pid deadline
	pid=$(vm_pid)
	deadline=$((SECONDS + ${VM_SHUTDOWN_TIMEOUT:-180}))
	if vm_ssh true 2>/dev/null; then
		log "shutting down (pid $pid)"
		vm_ssh 'shutdown -p now' >/dev/null 2>&1 || true
		vm_ssh_close
	else
		log "ssh unreachable; stopping QEMU (pid $pid)"
		deadline=$SECONDS
	fi
	while kill -0 "$pid" 2>/dev/null && ((SECONDS < deadline)); do sleep 2; done
	if kill -0 "$pid" 2>/dev/null; then
		((deadline == SECONDS)) || log "guest did not power off in time; killing QEMU"
		kill "$pid"
		sleep 3
		kill -9 "$pid" 2>/dev/null || true
	fi
	rm -f "$VM_PIDFILE"
	log "stopped"
}

cmd_status() {
	if ! vm_running; then
		echo "stopped"
		return 1
	fi
	local ssh_state=down
	vm_ssh true 2>/dev/null && ssh_state=up
	echo "running: pid $(vm_pid), ssh 127.0.0.1:$VM_SSH_PORT $ssh_state, disk ${VM_DISK#"$REPO_ROOT"/}"
}

cmd_wait() {
	local timeout=${1:-$VM_BOOT_TIMEOUT} start=$SECONDS
	vm_running || die "VM is not running (vm.sh up)"
	vm_ssh_close
	log "waiting up to ${timeout}s for SSH + first-boot provisioning"
	# /firstboot is removed by rc(8) once every firstboot script (incl. nuageinit runcmd) is done.
	until vm_ssh 'test ! -e /firstboot' 2>/dev/null; do
		vm_running || die "QEMU exited; see $VM_CONSOLE"
		((SECONDS - start < timeout)) || die "timed out after ${timeout}s; see $VM_CONSOLE"
		sleep 5
	done
	log "ready after $((SECONDS - start))s"
}

# qmp_input <json events>: sends input events through QMP.
qmp_input() {
	qmp "{\"execute\": \"input-send-event\", \"arguments\": {\"events\": $1}}"
}

# qmp_status: what QEMU says the guest is doing: running, suspended, ...
qmp_status() {
	python3 - "$VM_QMP" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
f = s.makefile("rw")
f.readline()
for command in ({"execute": "qmp_capabilities"}, {"execute": "query-status"}):
    f.write(json.dumps(command) + "\n")
    f.flush()
    while True:
        reply = json.loads(f.readline())
        if "return" in reply:
            break
print(reply["return"].get("status", ""))
PY
}

# qmp <json command>: sends one command through QMP.
qmp() {
	vm_running || die "VM is not running"
	[[ -S $VM_QMP ]] || die "no QMP socket (restart the VM: vm.sh down && vm.sh up)"
	python3 - "$VM_QMP" "$1" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
f = s.makefile("rw")
f.readline()  # greeting
for command in ({"execute": "qmp_capabilities"}, json.loads(sys.argv[2])):
    f.write(json.dumps(command) + "\n")
    f.flush()
    while True:
        reply = json.loads(f.readline())
        if "error" in reply:
            sys.exit(f"vm.sh: QMP: {reply['error'].get('desc')}")
        if "return" in reply:
            break
PY
}

# abs_events <x> <y>: the tablet events for screen pixel (x, y); QEMU's
# absolute axes run 0 to 0x7fff across the screen.
abs_events() {
	local w=${VM_SCREEN%x*} h=${VM_SCREEN#*x}
	printf '{"type":"abs","data":{"axis":"x","value":%d}},{"type":"abs","data":{"axis":"y","value":%d}}' \
		$(($1 * 32767 / (w - 1))) $(($2 * 32767 / (h - 1)))
}

cmd_point() {
	(($# == 2)) || die "usage: vm.sh point <x> <y>"
	qmp_input "[$(abs_events "$1" "$2")]"
}

cmd_click() {
	(($# == 2)) || die "usage: vm.sh click <x> <y>"
	local at
	at=$(abs_events "$1" "$2")
	# The position goes with each button event: a move sent on its own can
	# be lost when the tablet reports nothing else.
	qmp_input "[$at]"
	sleep 0.1
	qmp_input "[$at,{\"type\":\"btn\",\"data\":{\"down\":true,\"button\":\"left\"}}]"
	sleep 0.1
	qmp_input "[$at,{\"type\":\"btn\",\"data\":{\"down\":false,\"button\":\"left\"}}]"
}

cmd_drag() {
	(($# == 4)) || die "usage: vm.sh drag <x> <y> <x2> <y2>"
	local from to step x y
	from=$(abs_events "$1" "$2")
	to=$(abs_events "$3" "$4")
	qmp_input "[$from]"
	sleep 0.1
	qmp_input "[$from,{\"type\":\"btn\",\"data\":{\"down\":true,\"button\":\"left\"}}]"
	# Steps on the way, as a hand would move it.
	for step in 1 2 3 4; do
		x=$(($1 + ($3 - $1) * step / 5)) y=$(($2 + ($4 - $2) * step / 5))
		sleep 0.1
		qmp_input "[$(abs_events "$x" "$y")]"
	done
	sleep 0.1
	qmp_input "[$to]"
	sleep 0.1
	qmp_input "[$to,{\"type\":\"btn\",\"data\":{\"down\":false,\"button\":\"left\"}}]"
}

# monitor <command>: one human-monitor command.
monitor() {
	vm_running || die "VM is not running"
	python3 - "$VM_MONITOR" "$1" <<'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall(sys.argv[2].encode() + b"\n")
time.sleep(0.15)
s.close()
PY
}

# usb-stick plug|unplug: a USB mass-storage drive hot-plugged through the monitor.
cmd_usb_stick() {
	local image=$VM_DIR/usb-stick.img
	case ${1:-} in
	plug)
		[[ -f $image ]] || truncate -s 16M "$image"
		monitor "drive_add 0 if=none,id=stick,file=$image,format=raw"
		monitor "device_add usb-storage,id=stick,drive=stick"
		;;
	unplug)
		monitor "device_del stick"
		sleep 1
		monitor "drive_del stick"
		;;
	*) die "usage: vm.sh usb-stick plug|unplug" ;;
	esac
}

cmd_key() {
	(($# >= 1)) || die "usage: vm.sh key <key>... (QEMU names: ctrl-spc, ret, esc, a, shift-a)"
	local k
	for k in "$@"; do monitor "sendkey $k"; done
}

# scroll up|down [notches]: turn the wheel where the pointer is (QMP).
cmd_scroll() {
	[[ ${1:-} == up || ${1:-} == down ]] || die "usage: vm.sh scroll up|down [notches]"
	local button=wheel-$1 i
	for ((i = 0; i < ${2:-1}; i++)); do
		qmp_input "[{\"type\":\"btn\",\"data\":{\"down\":true,\"button\":\"$button\"}}]"
		qmp_input "[{\"type\":\"btn\",\"data\":{\"down\":false,\"button\":\"$button\"}}]"
	done
}

# keydown|keyup <qcode>: one key down, or up, on its own (QMP), to see what shows while it's held,
# such as Alt Tab's switcher; a key held with sendkey's hold time repeats.
cmd_keyupdown() {
	local down=$1
	(($# == 2)) || die "usage: vm.sh keydown|keyup <key> (QEMU names: alt, shift, ctrl, meta_l)"
	qmp_input "[{\"type\":\"key\",\"data\":{\"down\":$down,\"key\":{\"type\":\"qcode\",\"data\":\"$2\"}}}]"
}

cmd_type() {
	(($# == 1)) || die "usage: vm.sh type <text>"
	local text=$1 i c k
	for ((i = 0; i < ${#text}; i++)); do
		c=${text:i:1}
		case $c in
		[a-z0-9]) k=$c ;;
		[A-Z]) k="shift-${c,}" ;;
		' ') k="spc" ;;
		.) k="dot" ;;
		,) k="comma" ;;
		-) k="minus" ;;
		/) k="slash" ;;
		:) k="shift-semicolon" ;;
		\;) k="semicolon" ;;
		\') k="apostrophe" ;;
		\") k="shift-apostrophe" ;;
		\!) k="shift-1" ;;
		@) k="shift-2" ;;
		\#) k="shift-3" ;;
		\?) k="shift-slash" ;;
		\() k="shift-9" ;;
		\)) k="shift-0" ;;
		_) k="shift-minus" ;;
		=) k="equal" ;;
		+) k="shift-equal" ;;
		\[) k="bracket_left" ;;
		\]) k="bracket_right" ;;
		\{) k="shift-bracket_left" ;;
		\}) k="shift-bracket_right" ;;
		\$) k="shift-4" ;;
		%) k="shift-5" ;;
		\&) k="shift-7" ;;
		\*) k="shift-8" ;;
		\<) k="shift-comma" ;;
		\>) k="shift-dot" ;;
		\|) k="shift-backslash" ;;
		\\) k="backslash" ;;
		\`) k="grave_accent" ;;
		\~) k="shift-grave_accent" ;;
		*) die "vm.sh type: can't type '$c'" ;;
		esac
		monitor "sendkey $k"
	done
}

cmd_screendump() {
	(($# == 1)) || die "usage: vm.sh screendump <out.png>"
	vm_running || die "VM is not running"
	[[ -S $VM_MONITOR ]] || die "no monitor socket (restart the VM: vm.sh down && vm.sh up)"
	python3 - "$VM_MONITOR" "$1" <<'PY'
import os, socket, sys, tempfile, time
from PIL import Image
monitor, out = sys.argv[1], sys.argv[2]
ppm = tempfile.mktemp(suffix=".ppm")
s = socket.socket(socket.AF_UNIX)
s.connect(monitor)
s.sendall(f"screendump {ppm}\n".encode())
for _ in range(50):
    if os.path.exists(ppm) and os.path.getsize(ppm) > 0:
        break
    time.sleep(0.1)
time.sleep(0.2)
s.close()
Image.open(ppm).save(out)
os.unlink(ppm)
PY
}

cmd_exec() {
	(($# >= 1)) || die "usage: vm.sh exec <binary> [args...]"
	local bin=$1 dest
	shift
	dest=/tmp/vm-exec/${bin##*/}
	vm_ssh 'mkdir -p /tmp/vm-exec'
	vm_scp "$bin" "$dest"
	vm_ssh "$(printf '%q ' "$dest" "$@")"
}

cmd_clear_snapshots() {
	# One `zfs destroy` per 100 snapshots of a dataset (`fs@a,b,c`): one a
	# snapshot took 5 minutes for 942.
	vm_ssh "set -e
service glassd stop >/dev/null 2>&1 || true
n=\$(zfs list -H -t snapshot -o name | grep -c '@glass-' || true)
for ds in \$(zfs list -H -t snapshot -o name | grep '@glass-' | sed 's/@.*//' | sort -u); do
	zfs list -H -t snapshot -o name -d 1 \"\$ds\" | grep '@glass-' | sed 's/.*@//' | xargs -n 100 echo | tr ' ' ',' |
		while read -r names; do zfs destroy \"\$ds@\$names\"; done
done
service glassd start </dev/null >/dev/null 2>&1
for i in \$(jot 120); do glassctl status >/dev/null 2>&1 && break; sleep 0.5; done
echo \"destroyed \$n snapshots\""
}

# A power cut: the VM restarts at once, as if unplugged (nothing is shut down
# or written first), and boots again.
cmd_power_cut() {
	qmp '{"execute": "system_reset"}'
	log "power cut; the VM is booting again (vm.sh wait)"
}

# The guest suspends to RAM, as a closed lid does (rc.suspend, then acpiconf), and wakes when
# QEMU's firmware is told to (system_wakeup), as an opened lid does.
cmd_sleep_wake() {
	local secs=${1:-5} state=
	vm_ssh 'nohup acpiconf -s 3 >/dev/null 2>&1 </dev/null &' || true
	vm_ssh_close
	for _ in $(seq 120); do
		state=$(qmp_status)
		[[ $state == suspended ]] && break
		sleep 1
	done
	[[ $state == suspended ]] || die "the guest didn't go to sleep (QEMU says ${state:-nothing})"
	log "asleep; waking in ${secs}s"
	sleep "$secs"
	qmp '{"execute": "system_wakeup"}'
	local start=$SECONDS
	until vm_ssh true 2>/dev/null; do
		((SECONDS - start < 180)) || die "the guest didn't wake (QEMU says $(qmp_status)); see $VM_CONSOLE"
		vm_ssh_close
		sleep 3
	done
	log "awake after $((SECONDS - start))s"
}

cmd_reset() {
	cmd_down
	rm -f "$VM_DISK" "$VM_SEED" "$VM_CONSOLE" "$VM_CONSOLE.prev"
	log "reset; next 'vm.sh up' boots a fresh overlay"
}

cmd=${1:-}
(($# > 0)) && shift
case $cmd in
up) cmd_up ;;
down) cmd_down ;;
status) cmd_status ;;
wait) cmd_wait "$@" ;;
ssh) vm_ssh "$@" ;;
ssh-close) vm_ssh_close ;;
run)
	(($# > 0)) || die "usage: vm.sh run <cmd...>"
	vm_ssh "$*"
	;;
scp-in)
	(($# >= 2)) || die "usage: vm.sh scp-in <src...> <dest>"
	vm_scp "$@"
	;;
exec) cmd_exec "$@" ;;
screendump) cmd_screendump "$@" ;;
point) cmd_point "$@" ;;
click) cmd_click "$@" ;;
drag) cmd_drag "$@" ;;
key) cmd_key "$@" ;;
keydown) cmd_keyupdown true "$@" ;;
scroll) cmd_scroll "$@" ;;
keyup) cmd_keyupdown false "$@" ;;
type) cmd_type "$@" ;;
usb-stick) cmd_usb_stick "$@" ;;
reset) cmd_reset ;;
power-cut) cmd_power_cut ;;
sleep-wake) cmd_sleep_wake "$@" ;;
clear-snapshots) cmd_clear_snapshots ;;
console-log)
	[[ ${1:-} == -f ]] && exec tail -F "$VM_CONSOLE"
	cat "$VM_CONSOLE"
	;;
*) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2 && exit 2 ;;
# `exit` on the same line: bash reads the dispatch whole before running it, never
# what an edit made of the file while it ran (a long build in the VM survives one).
esac; exit
