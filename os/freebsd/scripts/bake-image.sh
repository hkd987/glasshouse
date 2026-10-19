#!/usr/bin/env bash
# Usage: bake-image.sh
# Boots a fresh throwaway VM ($BAKE_VM_DIR, default build/vm/bake/; ssh port $BAKE_SSH_PORT, default
# 2223) so the dev VM can keep running, deploys the staged build/dist/freebsd/, shuts down cleanly
# and flattens the disk into $IMAGE_OUT (default build/images/glasshouse-<freebsd-version>-amd64.qcow2;
# compressed, no backing file). The VM starts from the official VM image, or from $VM_BASE_IMAGE
# (bake-pkgbase.sh passes the package-list root it built).
# The image is a DEV image: it contains the dev SSH key for root and the demo user.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"
need qemu-img

[[ -d $DIST_DIR ]] || die "nothing staged at ${DIST_DIR#"$REPO_ROOT"/} (run stage-dist.sh)"
out=${IMAGE_OUT:-$IMAGES_DIR/glasshouse-$FREEBSD_VERSION-$FREEBSD_ARCH.qcow2}
export VM_DIR=${BAKE_VM_DIR:-$BUILD_DIR/vm/bake} VM_SSH_PORT=${BAKE_SSH_PORT:-2223}
vm=$FREEBSD_SCRIPTS_DIR/vm.sh

trap '"$vm" down >/dev/null 2>&1 || true' EXIT
"$vm" reset
"$vm" up
"$vm" wait
"$FREEBSD_SCRIPTS_DIR/deploy.sh"
"$vm" down
trap - EXIT

log "flattening into ${out#"$REPO_ROOT"/}"
mkdir -p "$(dirname "$out")"
qemu-img convert -O qcow2 -c "$VM_DIR/disk.qcow2" "$out.part"
mv "$out.part" "$out"
rm -rf "$VM_DIR"
log "ok: ${out#"$REPO_ROOT"/} ($(du -h "$out" | cut -f1)); boot it with VM_BASE_IMAGE=$out vm.sh up"
