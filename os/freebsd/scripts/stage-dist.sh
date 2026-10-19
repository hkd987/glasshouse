#!/usr/bin/env bash
# Usage: stage-dist.sh
# Recreates the staging root build/dist/freebsd/ (mirrors the guest's /): copies the overlay
# tree os/freebsd/overlay/, then the cross-compiled binaries listed in os/freebsd/dist.manifest.
# Binaries come from ${CARGO_TARGET_DIR:-target}/x86_64-unknown-freebsd/${DIST_PROFILE:-release}/,
# and shell/<binary> from the shell workspace's ${SHELL_TARGET_DIR:-shell/target}/x86_64-unknown-freebsd/<same>/
# (a worktree can stage another checkout's builds by pointing both at them).
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

manifest=$FREEBSD_OS_DIR/dist.manifest
bindir=${CARGO_TARGET_DIR:-$REPO_ROOT/target}/x86_64-unknown-freebsd/${DIST_PROFILE:-release}
shell_bindir=${SHELL_TARGET_DIR:-$REPO_ROOT/shell/target}/x86_64-unknown-freebsd/${DIST_PROFILE:-release}

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"
cp -a "$FREEBSD_OS_DIR/overlay/." "$DIST_DIR/"
find "$DIST_DIR" -name .gitkeep -delete

missing=() shipped=() shell_shipped=() n=0 lineno=0
while read -r name path mode extra; do
	lineno=$((lineno + 1))
	[[ -z $name || $name == \#* ]] && continue
	[[ -n $mode && -z $extra && $mode =~ ^[0-7]{3,4}$ ]] ||
		die "dist.manifest:$lineno: expected '<binary> <relative-path> <octal-mode>'"
	[[ $path != /* && $path != *..* ]] || die "dist.manifest:$lineno: path must be relative: $path"
	from=$bindir/$name
	[[ $name == shell/* ]] && from=$shell_bindir/${name#shell/}
	if [[ ! -f $from ]]; then
		missing+=("$name")
		continue
	fi
	if [[ $name == shell/* ]]; then shell_shipped+=("${name#shell/}"); else shipped+=("$name"); fi
	install -D -m "$mode" "$from" "$DIST_DIR/$path"
	n=$((n + 1))
done <"$manifest"

# Third-party libraries and data the desktop needs, from FreeBSD's packages, until they come as
# packages from our own repository (os/freebsd/images/desktop.third-party says which and why).
third_party=$FREEBSD_OS_DIR/images/desktop.third-party
ports_cache=$CACHE_DIR/ports/$FREEBSD_PORTS_REPO/All
need tar zstd
while read -r package paths; do
	[[ -z $package || $package == \#* ]] && continue
	file=$(find "$ports_cache" \( -name "$package.pkg" -o -name "$package~*.pkg" \) 2>/dev/null | head -1)
	[[ -n $file ]] || die "$package isn't in ${ports_cache#"$REPO_ROOT"/} (os/freebsd/scripts/ports.sh vm-install ${package%-*} fetches it)"
	members=()
	for path in $paths "usr/local/share/licenses/$package"; do members+=("/$path"); done
	tar --zstd -xf "$file" -C "$DIST_DIR" "${members[@]}" 2>/dev/null ||
		die "${file##*/} doesn't hold ${members[*]}"
done < <(sed 's/#.*//' "$third_party")

# Open-source notices for the shipped binaries and the base system (BR-2).
notice_args=(--third-party "$third_party" --third-party-licenses "$DIST_DIR/usr/local/share/licenses")
for name in "${shipped[@]}"; do notice_args+=(--package "$name"); done
# The shell's crates, and the fonts it embeds.
for name in "${shell_shipped[@]}"; do notice_args+=(--shell-package "$name"); done
if ((${#shell_shipped[@]})); then notice_args+=(--assets "$REPO_ROOT/shell/assets/fonts"); fi
python3 "$REPO_ROOT/tools/legal/notices.py" "${notice_args[@]}" \
	--base-txz "$CACHE_DIR/$FREEBSD_BASE_TXZ" \
	--notices "$DIST_DIR/usr/local/share/glasshouse/legal/NOTICES.txt" 2>/dev/null ||
	die "open-source notices failed: run tools/legal/notices.py for details"

# The OS identity the image reports (BR-1; read by rc.d/glasshouse_identity).
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml")
build=$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
git -C "$REPO_ROOT" diff --quiet HEAD 2>/dev/null || build="$build-dirty"
install -d "$DIST_DIR/usr/local/share/glasshouse/branding"
printf 'version=%s\nbuild=%s\nprofile=%s\n' "$version" "$build" "${DIST_PROFILE:-release}" \
	>"$DIST_DIR/usr/local/share/glasshouse/branding/build-info"

if ((${#missing[@]})); then
	die "missing binaries: ${missing[*]} (build them: make freebsd FREEBSD_PROFILE=${DIST_PROFILE:-release})"
fi
log "staged $n binaries + overlay into ${DIST_DIR#"$REPO_ROOT"/} ($(find "$DIST_DIR" ! -type d | wc -l) files)"
