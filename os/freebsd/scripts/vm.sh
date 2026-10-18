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
#   console-log [-f]   print (or follow) the serial console log
#   screendump <out.png>     save what the VM's display shows right now (boot screens, splash)
# Env knobs: VM_DIR VM_SSH_PORT VM_CPUS VM_CPU_MODEL VM_ACCEL VM_MEM VM_DISK_SIZE VM_BOOT_TIMEOUT VM_BASE_IMAGE
#   VM_FIRMWARE=bios|uefi (see lib.sh).
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
		[[ -f $VM_EFIVARS ]] || cp "$OVMF_VARS" "$VM_EFIVARS"
		firmware=(-drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_CODE"
			-drive "if=pflash,format=raw,unit=1,file=$VM_EFIVARS")
	fi
	rm -f "$VM_MONITOR"
	qemu-system-x86_64 \
		-name glasshouse-dev -machine q35 -accel "$VM_ACCEL" -cpu "$VM_CPU_MODEL" \
		-smp "$VM_CPUS" -m "$VM_MEM" \
		-drive "if=virtio,file=$VM_DISK,format=qcow2,discard=unmap" \
		-drive "if=virtio,file=$VM_SEED,format=raw,readonly=on" \
		-netdev "user,id=net0,hostname=glasshouse-dev,hostfwd=tcp:127.0.0.1:$VM_SSH_PORT-:22" \
		-device virtio-net-pci,netdev=net0 -device virtio-rng-pci \
		"${firmware[@]}" \
		-display none -monitor "unix:$VM_MONITOR,server,nowait" -serial "file:$VM_CONSOLE" \
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
reset) cmd_reset ;;
console-log)
	[[ ${1:-} == -f ]] && exec tail -F "$VM_CONSOLE"
	cat "$VM_CONSOLE"
	;;
*) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2 && exit 2 ;;
esac
