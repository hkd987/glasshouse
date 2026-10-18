#!/usr/bin/env bash
# Usage: stage-dist.sh
# Recreates the staging root build/dist/freebsd/ (mirrors the guest's /): copies the overlay
# tree os/freebsd/overlay/, then the cross-compiled binaries listed in os/freebsd/dist.manifest.
# Binaries come from ${CARGO_TARGET_DIR:-target}/x86_64-unknown-freebsd/${DIST_PROFILE:-release}/.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

manifest=$FREEBSD_OS_DIR/dist.manifest
bindir=${CARGO_TARGET_DIR:-$REPO_ROOT/target}/x86_64-unknown-freebsd/${DIST_PROFILE:-release}

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"
cp -a "$FREEBSD_OS_DIR/overlay/." "$DIST_DIR/"
find "$DIST_DIR" -name .gitkeep -delete

missing=() n=0 lineno=0
while read -r name path mode extra; do
	lineno=$((lineno + 1))
	[[ -z $name || $name == \#* ]] && continue
	[[ -n $mode && -z $extra && $mode =~ ^[0-7]{3,4}$ ]] ||
		die "dist.manifest:$lineno: expected '<binary> <relative-path> <octal-mode>'"
	[[ $path != /* && $path != *..* ]] || die "dist.manifest:$lineno: path must be relative: $path"
	if [[ ! -f $bindir/$name ]]; then
		missing+=("$name")
		continue
	fi
	install -D -m "$mode" "$bindir/$name" "$DIST_DIR/$path"
	n=$((n + 1))
done <"$manifest"

if ((${#missing[@]})); then
	die "missing binaries in ${bindir#"$REPO_ROOT"/}: ${missing[*]}" \
		"(build them: cargo build --release --target x86_64-unknown-freebsd)"
fi
log "staged $n binaries + overlay into ${DIST_DIR#"$REPO_ROOT"/} ($(find "$DIST_DIR" ! -type d | wc -l) files)"
