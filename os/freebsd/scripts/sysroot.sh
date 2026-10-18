#!/usr/bin/env bash
# Usage: sysroot.sh
# Builds the cross-compile sysroot at build/freebsd/sysroot/ from the cached base.txz
# (headers, crt objects and libraries only). Rebuilt only when release.env's base.txz pin changes.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"
need tar xz

stamp=$SYSROOT_DIR/.base-sha256
if [[ -f $stamp && $(cat "$stamp") == "$FREEBSD_BASE_TXZ_SHA256" ]]; then
	log "up to date: $SYSROOT_DIR"
	exit 0
fi

"$FREEBSD_SCRIPTS_DIR/fetch.sh"
log "extracting sysroot from $FREEBSD_BASE_TXZ"
rm -rf "$SYSROOT_DIR"
mkdir -p "$SYSROOT_DIR"
# lib/ holds the real shared objects (libc.so.7, libthr.so.3, libgcc_s.so.1, ...);
# usr/lib holds crt*.o, static libs and the .so dev links/linker scripts.
tar -xJf "$CACHE_DIR/$FREEBSD_BASE_TXZ" -C "$SYSROOT_DIR" --no-same-owner --warning=no-unknown-keyword \
	--exclude='./usr/lib/debug' --exclude='./usr/lib/dtrace' \
	./lib ./usr/lib ./usr/include

# Dev links must resolve inside the sysroot, so rewrite any absolute symlink targets.
while IFS= read -r -d '' link; do
	target=$(readlink "$link")
	[[ $target == /* ]] || continue
	ln -sfn "$(realpath -m --relative-to="$(dirname "$link")" "$SYSROOT_DIR$target")" "$link"
done < <(find "$SYSROOT_DIR" -type l -print0)

echo "$FREEBSD_BASE_TXZ_SHA256" >"$stamp"
log "ok: $SYSROOT_DIR ($(du -sh "$SYSROOT_DIR" | cut -f1))"
