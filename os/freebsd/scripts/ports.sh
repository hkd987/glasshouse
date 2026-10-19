#!/usr/bin/env bash
# Usage: ports.sh vm-install <package>...   install FreeBSD packages (from ports) in the dev VM
#        ports.sh sysroot <package>...      unpack the same packages into the cross-compile sysroot
#
# Third-party libraries the desktop links (libxkbcommon and pixman for the compositor, with
# xkeyboard-config's keymaps) come from FreeBSD's package repository, mirrored like the base
# packages (ADR 0022): pkg-mirror.py serves $FREEBSD_PORTS_REPO from build/cache/.../ports/,
# downloading each file once, so the VM installs over plain HTTP through 10.0.2.2 and the sysroot
# is unpacked from the very files the VM installed. Until our own repository exists (BS-2) the
# first catalog fetched is the one every later install sees; `sysroot` needs `vm-install` first
# (it takes the packages the VM downloaded).
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

cmd=${1:-}
(($# > 1)) || die "usage: ports.sh vm-install|sysroot <package>..."
shift
cache=$CACHE_DIR/ports
mkdir -p "$cache"

case $cmd in
vm-install)
	vm_running || die "VM is not running (vm.sh up && vm.sh wait)"
	port_file=$cache/.port
	rm -f "$port_file"
	python3 "$FREEBSD_SCRIPTS_DIR/pkg-mirror.py" --cache "$cache" \
		--upstream "$FREEBSD_PORTS_MIRROR" --port-file "$port_file" &
	mirror=$!
	trap 'kill "$mirror" 2>/dev/null || true' EXIT
	for _ in $(seq 100); do [[ -s $port_file ]] && break; sleep 0.1; done
	[[ -s $port_file ]] || die "the package mirror didn't start"
	port=$(cat "$port_file")
	# The guest's own repository config points at the internet over HTTPS, which this sandbox
	# intercepts; for this install it points at the mirror instead, then it's put back.
	vm_ssh "sh -s -- $port ${FREEBSD_PORTS_REPO#*/} $*" <<'EOF'
set -eu
port=$1 branch=$2
shift 2
mkdir -p /usr/local/etc/pkg/repos
conf=/usr/local/etc/pkg/repos/glasshouse-mirror.conf
cat >"$conf" <<CONF
FreeBSD-ports: { enabled: no }
FreeBSD-ports-kmods: { enabled: no }
glasshouse-ports: {
  url: "http://10.0.2.2:$port/\${ABI}/$branch",
  mirror_type: "none",
  signature_type: "fingerprints",
  fingerprints: "/usr/share/keys/pkg",
  enabled: yes
}
CONF
trap 'rm -f "$conf"' EXIT
env ASSUME_ALWAYS_YES=yes pkg install -y -r glasshouse-ports "$@"
EOF
	;;
sysroot)
	[[ -f $SYSROOT_DIR/.base-sha256 ]] || die "no sysroot (make freebsd-sysroot)"
	need tar zstd
	for name in "$@"; do
		# The newest file the VM downloaded for this package.
		file=$(find "$cache" -path '*/All/*' -name "$name-[0-9]*.pkg" | sort -V | tail -1)
		[[ -n $file ]] || die "$name isn't in ${cache#"$REPO_ROOT"/} (ports.sh vm-install $name first)"
		tar --zstd -xf "$file" -C "$SYSROOT_DIR" --exclude='+*' 2>/dev/null ||
			tar -xf "$file" -C "$SYSROOT_DIR" --exclude='+*'
		log "unpacked ${file##*/}"
	done
	;;
*) die "usage: ports.sh vm-install|sysroot <package>..." ;;
esac
