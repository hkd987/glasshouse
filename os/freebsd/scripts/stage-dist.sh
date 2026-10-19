#!/usr/bin/env bash
# Usage: stage-dist.sh
# Recreates the staging root build/dist/freebsd/ (mirrors the guest's /): copies the overlay
# tree os/freebsd/overlay/, then the cross-compiled binaries listed in os/freebsd/dist.manifest.
# Binaries come from ${CARGO_TARGET_DIR:-target}/x86_64-unknown-freebsd/${DIST_PROFILE:-release}/,
# and shell/<binary> from the shell workspace's ${SHELL_TARGET_DIR:-shell/target}/x86_64-unknown-freebsd/<same>/
# (a worktree can stage another checkout's builds by pointing both at them).
# STAGE_FOR=pkgbase (make image-pkgbase): for an image built from base packages; its source offer names
# the pinned catalog's source packages instead of the release's src.txz.
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
	install -p -D -m "$mode" "$from" "$DIST_DIR/$path"
	n=$((n + 1))
done <"$manifest"

# Third-party libraries and data from FreeBSD's packages, until they come as packages from our own
# repository: the desktop's, and the browser engine's (os/freebsd/images/*.third-party say which
# and why). Packages the cache doesn't hold yet are fetched.
third_party=("$FREEBSD_OS_DIR"/images/*.third-party)
ports_cache=$CACHE_DIR/ports/$FREEBSD_PORTS_REPO/All
need tar zstd
python3 "$FREEBSD_SCRIPTS_DIR/third-party.py" fetch "${third_party[@]}" ||
	die "couldn't fetch the third-party packages"
for list in "${third_party[@]}"; do
	while read -r package paths; do
		[[ -z $package || $package == \#* ]] && continue
		file=$(find "$ports_cache" -name "$package~*.pkg" 2>/dev/null | head -1 || true)
		if [[ -z $file ]]; then
			# Rebuilt upstream since our catalog was fetched (ports.sh fetch): the same version, a
			# later build than the catalog pins. Fine for the VM; an image waits for the catalog
			# to be fetched again and the lists bumped (third-party.py update --bump).
			file=$(find "$ports_cache" -name "$package.pkg" 2>/dev/null | head -1 || true)
			[[ -n $file ]] || die "$package isn't in ${ports_cache#"$REPO_ROOT"/}"
			[[ ${STAGE_FOR:-} != pkgbase || -n ${ALLOW_REBUILT_PACKAGES:-} ]] ||
				die "$package is a later build than the pinned ports catalog (ALLOW_REBUILT_PACKAGES=1 takes it)"
		fi
		members=()
		for path in $paths "usr/local/share/licenses/$package"; do members+=("/$path"); done
		tar --zstd -xf "$file" -C "$DIST_DIR" "${members[@]}" 2>/dev/null ||
			die "${file##*/} doesn't hold ${members[*]}"
	done < <(sed -e '/^#license/d' -e 's/#.*//' "$list")
done

# The web apps (UI-5): each one's manifest, which glassd installs it from, and its built files,
# which the person's app bridge serves (make apps-build). The admin console isn't a device app.
# Each ships its source beside them, which people remix into their own (ADR 0032).
for manifest in "$REPO_ROOT"/apps/*/manifest.json; do
	app=$(basename "$(dirname "$manifest")")
	[[ -f $REPO_ROOT/apps/$app/dist/app.js ]] || die "apps/$app isn't built (make apps-build)"
	install -D -m 0644 "$manifest" "$DIST_DIR/usr/local/share/glasshouse/apps/$app/manifest.json"
	cp -Rp "$REPO_ROOT/apps/$app/dist" "$DIST_DIR/usr/local/share/glasshouse/apps/$app/dist"
	cp -Rp "$REPO_ROOT/apps/$app/src" "$DIST_DIR/usr/local/share/glasshouse/apps/$app/src"
done
# The SDK every remix builds on, and the template a new app starts from.
sdk=$DIST_DIR/usr/local/share/glasshouse/sdk
install -D -m 0644 "$REPO_ROOT/apps/sdk/package.json" "$sdk/package.json"
cp -Rp "$REPO_ROOT/apps/sdk/src" "$REPO_ROOT/apps/sdk/template" "$sdk/"
# The bundler glassd builds them with, in a jail: esbuild for this platform, the version and
# checksum the apps' lockfile pins, so the computer builds people's apps as we build ours. Alone
# in its folder, which the jail mounts; its license goes in the notices.
mapfile -t pin < <(python3 "$REPO_ROOT/tools/studio/esbuild-pin.py" "$REPO_ROOT/apps/package-lock.json" freebsd-x64) ||
	die "no esbuild pinned for this platform in apps/package-lock.json"
esbuild_dir=$CACHE_DIR/esbuild-${pin[0]}
if [[ ! -x $esbuild_dir/esbuild ]]; then
	mkdir -p "$esbuild_dir"
	curl -fsSL "${pin[1]}" -o "$esbuild_dir/package.tgz.part"
	[[ $(sha512sum <"$esbuild_dir/package.tgz.part") == "${pin[2]}  -" ]] ||
		die "esbuild ${pin[0]} doesn't match the lockfile's checksum"
	tar -xzf "$esbuild_dir/package.tgz.part" -C "$esbuild_dir" --strip-components=2 package/bin/esbuild
	rm "$esbuild_dir/package.tgz.part"
fi
install -D -m 0755 "$esbuild_dir/esbuild" "$DIST_DIR/usr/local/lib/glasshouse/bundler/esbuild"
esbuild_license=$BUILD_DIR/legal/esbuild
install -D -m 0644 "$REPO_ROOT/apps/node_modules/esbuild/LICENSE.md" "$esbuild_license/LICENSE-esbuild.md"
# The desktop's fonts, for the browser engine too (os/freebsd/overlay/usr/local/etc/fonts).
install -d "$DIST_DIR/usr/local/share/fonts/glasshouse"
install -p -m 0644 "$REPO_ROOT"/shell/assets/fonts/*.ttf "$DIST_DIR/usr/local/share/fonts/glasshouse/"

# Open-source notices for the shipped binaries and the base system (BR-2).
notice_args=(--third-party-licenses "$DIST_DIR/usr/local/share/licenses")
for list in "${third_party[@]}"; do notice_args+=(--third-party "$list"); done
for name in "${shipped[@]}"; do notice_args+=(--package "$name"); done
# The shell's crates, and the fonts it embeds.
for name in "${shell_shipped[@]}"; do notice_args+=(--shell-package "$name"); done
if ((${#shell_shipped[@]})); then notice_args+=(--assets "$REPO_ROOT/shell/assets/fonts"); fi
notice_args+=(--assets "$esbuild_license")
# An image built from base packages (STAGE_FOR=pkgbase, make image-pkgbase) offers the source
# packages of the catalog it's built from, not the release's src.txz (BR-3).
if [[ ${STAGE_FOR:-} == pkgbase ]]; then
	catalog=$CACHE_DIR/pkgbase/$FREEBSD_PKGBASE_REPO/$FREEBSD_PKGBASE_CATALOG
	if [[ ! -s $catalog ]]; then
		mkdir -p "$(dirname "$catalog")"
		curl -fsSL "$FREEBSD_PKGBASE_MIRROR/$FREEBSD_PKGBASE_REPO/$FREEBSD_PKGBASE_CATALOG" -o "$catalog.part"
		mv "$catalog.part" "$catalog"
	fi
	[[ $(sha256sum <"$catalog") == "$FREEBSD_PKGBASE_CATALOG_SHA256  -" ]] ||
		die "${catalog#"$REPO_ROOT"/} isn't the catalog release.env pins"
	notice_args+=(--pkgbase-catalog "$catalog" --pkgbase-catalog-sha256 "$FREEBSD_PKGBASE_CATALOG_SHA256")
fi
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
