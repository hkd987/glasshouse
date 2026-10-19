#!/usr/bin/env bash
# Usage: bake-pkgbase.sh [image]
# Builds build/images/glasshouse-<freebsd-version>-amd64-<image>.qcow2 from the package list
# os/freebsd/images/<image>.pkgs (default: desktop), rather than from the official VM image
# (ADR 0022 item 1, BS-3):
#   1. serves the pinned base repository (release.env) from build/cache/freebsd/<release>/pkgbase/
#      with pkg-mirror.py, which downloads each file once;
#   2. boots a throwaway build VM (the official image; $BAKE_VM_DIR, default build/vm/bake/, ssh
#      $BAKE_SSH_PORT, default 2223) with a new empty disk ($IMAGE_DISK_SIZE, default 6G) and runs
#      pkgbase-root.sh in it: a new ZFS pool holding exactly the listed packages, made bootable;
#   3. bakes that disk as bake-image.sh bakes the official one: first boot (provisioning from the
#      VM seed), deploy of the staged build/dist/freebsd/ with post-install.sh, power off, flatten.
# The image carries its package inventory in /usr/local/share/glasshouse/packages.txt (BS-6); a
# copy goes beside the image (<image>.packages.txt) for `make sbom`.
# Like bake-image.sh's, the image is a DEV image (the dev SSH key for root, the demo user).
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"
need qemu-img python3 curl sha256sum

image=${1:-desktop}
list=$FREEBSD_OS_DIR/images/$image.pkgs
[[ -f $list ]] || die "no package list ${list#"$REPO_ROOT"/}"
[[ -d $DIST_DIR ]] || die "nothing staged at ${DIST_DIR#"$REPO_ROOT"/} (run stage-dist.sh)"
out=${IMAGE_OUT:-$IMAGES_DIR/glasshouse-$FREEBSD_VERSION-$FREEBSD_ARCH-$image.qcow2}
inventory=${out%.qcow2}.packages.txt
root_disk=$IMAGES_DIR/.$image-root.qcow2
export VM_DIR=${BAKE_VM_DIR:-$BUILD_DIR/vm/bake} VM_SSH_PORT=${BAKE_SSH_PORT:-2223}
vm=$FREEBSD_SCRIPTS_DIR/vm.sh
start=$SECONDS

# 1. The repository, on a free port of this machine; the VM reaches it at 10.0.2.2.
mkdir -p "$VM_DIR" "$IMAGES_DIR"
port_file=$VM_DIR/pkg-mirror.port
rm -f "$port_file"
python3 "$FREEBSD_SCRIPTS_DIR/pkg-mirror.py" --cache "$CACHE_DIR/pkgbase" \
	--upstream "$FREEBSD_PKGBASE_MIRROR" --port-file "$port_file" &
mirror=$!
cleanup() {
	"$vm" down >/dev/null 2>&1 || true
	kill "$mirror" 2>/dev/null || true
	rm -f "$root_disk"
}
trap cleanup EXIT
for _ in $(seq 100); do [[ -s $port_file ]] && break; sleep 0.1; done
[[ -s $port_file ]] || die "the package mirror didn't start"
port=$(cat "$port_file")

# The catalog decides every version in the image: check it against the pin before building.
catalog=$(curl -fsS "http://127.0.0.1:$port/$FREEBSD_PKGBASE_REPO/$FREEBSD_PKGBASE_CATALOG" | sha256sum)
catalog=${catalog%% *}
if [[ $catalog != "$FREEBSD_PKGBASE_CATALOG_SHA256" ]]; then
	die "the base package catalog ($FREEBSD_PKGBASE_REPO/$FREEBSD_PKGBASE_CATALOG) is $catalog, release.env pins $FREEBSD_PKGBASE_CATALOG_SHA256: FreeBSD published new packages. To build from them, review them and set FREEBSD_PKGBASE_CATALOG_SHA256=$catalog; the cached copy is ${CACHE_DIR#"$REPO_ROOT"/}/pkgbase/$FREEBSD_PKGBASE_REPO"
fi

# 2. The root, built in a VM: it needs FreeBSD's pkg, gpart and ZFS.
log "building the $image root ($(grep -cv '^[[:space:]]*\(#\|$\)' "$list") packages) in a build VM"
"$vm" reset
rm -f "$root_disk"
qemu-img create -q -f qcow2 "$root_disk" "${IMAGE_DISK_SIZE:-6G}"
VM_BASE_IMAGE='' VM_EXTRA_DISK=$root_disk "$vm" up
"$vm" wait
vm_scp "$FREEBSD_SCRIPTS_DIR/pkgbase-root.sh" "$list" /tmp/
# The disk after the system's and the seed's is vtbd2.
vm_ssh "sh /tmp/pkgbase-root.sh vtbd2 http://10.0.2.2:$port/$FREEBSD_PKGBASE_REPO /tmp/$image.pkgs" \
	>"$VM_DIR/pkgbase-root.out"
sed -n '/^inventory:$/,$p' "$VM_DIR/pkgbase-root.out" | tail -n +2 >"$inventory.part"
[[ -s $inventory.part ]] || die "the build VM wrote no inventory"
"$vm" down
kill "$mirror" 2>/dev/null || true
log "root built: $(wc -l <"$inventory.part") packages, $(du -h "$root_disk" | cut -f1) disk"

# 3. First boot, our payload, flatten.
VM_BASE_IMAGE=$root_disk IMAGE_OUT=$out "$FREEBSD_SCRIPTS_DIR/bake-image.sh"
mv "$inventory.part" "$inventory"
log "ok: ${out#"$REPO_ROOT"/} in $(((SECONDS - start) / 60))m$(((SECONDS - start) % 60))s; inventory ${inventory#"$REPO_ROOT"/}"
