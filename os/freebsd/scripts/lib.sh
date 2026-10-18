# lib.sh: shared helpers for os/freebsd/scripts. Source it, don't run it.
# Sets the repo/build paths, loads release.env, and provides logging + SSH helpers.
# shellcheck shell=bash disable=SC2034  # variables are used by the sourcing scripts

FREEBSD_SCRIPTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
FREEBSD_OS_DIR=$(dirname "$FREEBSD_SCRIPTS_DIR")
REPO_ROOT=$(cd "$FREEBSD_OS_DIR/../.." && pwd)

# shellcheck source=../release.env
. "$FREEBSD_OS_DIR/release.env"

BUILD_DIR=$REPO_ROOT/build
CACHE_DIR=$BUILD_DIR/cache/freebsd/$FREEBSD_RELEASE   # survives `make clean`
SYSROOT_DIR=$BUILD_DIR/freebsd/sysroot
DIST_DIR=$BUILD_DIR/dist/freebsd
IMAGES_DIR=$BUILD_DIR/images
PRISTINE_IMAGE=$CACHE_DIR/${FREEBSD_VM_IMAGE%.xz}      # decompressed by fetch.sh, read-only

# VM knobs (override from the environment).
VM_DIR=${VM_DIR:-$BUILD_DIR/vm}
VM_SSH_PORT=${VM_SSH_PORT:-2222}
VM_CPUS=${VM_CPUS:-2}
# KVM when usable, else TCG. QEMU 8.2 TCG mis-emulates AVX (15.1 panics in zfs.ko SIMD init),
# so TCG gets a CPU model without AVX; la57 off saves a page-walk level.
[[ -z ${VM_ACCEL:-} && -w /dev/kvm ]] && VM_ACCEL=kvm
VM_ACCEL=${VM_ACCEL:-tcg,thread=multi}
[[ $VM_ACCEL == kvm* ]] && VM_CPU_MODEL=${VM_CPU_MODEL:-host}
VM_CPU_MODEL=${VM_CPU_MODEL:-max,la57=off,avx=off,avx2=off}
VM_MEM=${VM_MEM:-4096}
VM_DISK_SIZE=${VM_DISK_SIZE:-20G}
VM_FIRMWARE=${VM_FIRMWARE:-bios}                          # bios, or uefi (needs OVMF; like real hardware)
OVMF_CODE=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
OVMF_VARS=${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}
VM_BOOT_TIMEOUT=${VM_BOOT_TIMEOUT:-900}                 # seconds; TCG first boot is slow
BASE_IMAGE=${VM_BASE_IMAGE:-$PRISTINE_IMAGE}            # backing disk for new overlays
VM_SSH_KEY=${VM_SSH_KEY:-$BUILD_DIR/vm/ssh/id_ed25519}  # dev-only key, shared by all VM_DIRs

VM_DISK=$VM_DIR/disk.qcow2
VM_SEED=$VM_DIR/seed.iso
VM_PIDFILE=$VM_DIR/qemu.pid
VM_CONSOLE=$VM_DIR/console.log
VM_MONITOR=$VM_DIR/monitor.sock                         # QEMU human monitor, for screendumps
VM_EFIVARS=$VM_DIR/efivars.fd

log() { printf '[%s] %s\n' "${0##*/}" "$*" >&2; }
die() { log "error: $*"; exit 1; }
need() {
	local t
	for t in "$@"; do
		command -v "$t" >/dev/null 2>&1 || die "missing host tool '$t' (see os/freebsd/scripts/check-host-deps.sh)"
	done
}

# Throwaway dev VM: never record or check host keys.
SSH_OPTS=(
	-i "$VM_SSH_KEY" -o IdentitiesOnly=yes -o BatchMode=yes
	-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o GlobalKnownHostsFile=/dev/null
	-o LogLevel=ERROR -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=4
)

# vm_ssh [ssh-args...] -- runs ssh as root against the dev VM.
vm_ssh() { ssh "${SSH_OPTS[@]}" -p "$VM_SSH_PORT" root@127.0.0.1 "$@"; }

# vm_scp <src...> <remote-dest> -- copies host files into the VM as root.
vm_scp() {
	local args=("$@") dest=${*: -1}
	scp "${SSH_OPTS[@]}" -P "$VM_SSH_PORT" -q "${args[@]:0:$#-1}" "root@127.0.0.1:$dest"
}

vm_pid() { [[ -f $VM_PIDFILE ]] && cat "$VM_PIDFILE"; }
vm_running() { local p; p=$(vm_pid) && [[ $(cat "/proc/$p/comm" 2>/dev/null) == qemu-system* ]]; }
