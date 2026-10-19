#!/usr/bin/env bash
# Usage: fetch.sh [--print-pins | src]
# Downloads the pinned FreeBSD VM image and base.txz into build/cache/freebsd/<release>/,
# or with `src` only src.txz (for the base system's notices, `make legal-base`),
# verifies their SHA256 against release.env, and decompresses the VM image once.
# Idempotent: verified files are stamped and skipped on later runs.
# --print-pins prints upstream SHA256s for the configured release (for bumping release.env),
# including the base package catalog's (bake-pkgbase.sh).
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"
need curl sha256sum xz

if [[ ${1:-} == --print-pins ]]; then
	img=$(curl -fsS "$FREEBSD_VM_IMAGE_URL/CHECKSUM.SHA256" | sed -n "s/^SHA256 ($FREEBSD_VM_IMAGE) = //p")
	manifest=$(curl -fsS "$FREEBSD_BASE_URL/MANIFEST")
	base=$(awk -v f="$FREEBSD_BASE_TXZ" '$1 == f { print $2 }' <<<"$manifest")
	src=$(awk -v f="$FREEBSD_SRC_TXZ" '$1 == f { print $2 }' <<<"$manifest")
	echo "FREEBSD_VM_IMAGE_SHA256=${img:?not found upstream}"
	echo "FREEBSD_BASE_TXZ_SHA256=${base:?not found upstream}"
	echo "FREEBSD_SRC_TXZ_SHA256=${src:?not found upstream}"
	catalog=$(curl -fsSL "$FREEBSD_PKGBASE_MIRROR/$FREEBSD_PKGBASE_REPO/$FREEBSD_PKGBASE_CATALOG" | sha256sum)
	echo "FREEBSD_PKGBASE_CATALOG_SHA256=${catalog%% *}"
	exit 0
fi

# fetch <url> <dest> <sha256>
fetch() {
	local url=$1 dest=$2 sum=$3 stamp=$2.sha256-ok
	if [[ -f $dest && -f $stamp && $(cat "$stamp") == "$sum" ]]; then
		log "cached: ${dest##*/}"
		return
	fi
	if [[ ! -f $dest ]]; then
		log "downloading $url"
		curl -fL --retry 5 --retry-delay 5 -C - -o "$dest.part" "$url"
		mv "$dest.part" "$dest"
	fi
	log "verifying ${dest##*/}"
	if ! echo "$sum  $dest" | sha256sum -c --quiet -; then
		mv "$dest" "$dest.bad"
		die "SHA256 mismatch for ${dest##*/} (moved to .bad); check release.env pins"
	fi
	echo "$sum" >"$stamp"
}

mkdir -p "$CACHE_DIR"
if [[ ${1:-} == src ]]; then
	fetch "$FREEBSD_BASE_URL/$FREEBSD_SRC_TXZ" "$CACHE_DIR/$FREEBSD_SRC_TXZ" "$FREEBSD_SRC_TXZ_SHA256"
	exit 0
fi
fetch "$FREEBSD_BASE_URL/$FREEBSD_BASE_TXZ" "$CACHE_DIR/$FREEBSD_BASE_TXZ" "$FREEBSD_BASE_TXZ_SHA256"

# Pristine base disk for VM overlays; read-only so nothing writes to it by accident. Its stamp says
# which download it was made from; once it matches the pin, the download isn't needed (cleanup.sh
# removes it).
if [[ -f $PRISTINE_IMAGE && $(cat "$PRISTINE_IMAGE.sha256-ok" 2>/dev/null) == "$FREEBSD_VM_IMAGE_SHA256" ]]; then
	log "cached: ${PRISTINE_IMAGE##*/}"
else
	fetch "$FREEBSD_VM_IMAGE_URL/$FREEBSD_VM_IMAGE" "$CACHE_DIR/$FREEBSD_VM_IMAGE" "$FREEBSD_VM_IMAGE_SHA256"
	if [[ ! -f $PRISTINE_IMAGE || $CACHE_DIR/$FREEBSD_VM_IMAGE.sha256-ok -nt $PRISTINE_IMAGE ]]; then
		log "decompressing ${FREEBSD_VM_IMAGE}"
		rm -f "$PRISTINE_IMAGE"
		xz -dc -T0 "$CACHE_DIR/$FREEBSD_VM_IMAGE" >"$PRISTINE_IMAGE.part"
		mv "$PRISTINE_IMAGE.part" "$PRISTINE_IMAGE"
		chmod a-w "$PRISTINE_IMAGE"
	fi
	echo "$FREEBSD_VM_IMAGE_SHA256" >"$PRISTINE_IMAGE.sha256-ok"
fi
log "ok: $CACHE_DIR"
