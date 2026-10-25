#!/usr/bin/env bash
# Usage: kernel-patch.sh build <patch dir> [--out <bundle>]
#        kernel-patch.sh key
# Builds a kernel patch's modules from the image's base source and signs them into a bundle, which
# `glassctl hardware patch apply` takes (ADR 0057). The dev VM stands in for the build service: it
# runs the release the modules are for and has the compiler. Nothing is built on a person's
# computer (BS-4).
#
# A patch is a folder: patch.diff (a unified diff against the base source, from `sys/`) and
# patch.toml (what it is, and the modules it rebuilds: os/freebsd/patches/*).
#
# The source is the pinned release's src.txz (fetch.sh src), unpacked once into a dataset of its
# own in the VM, zroot/kpatch/src@<release>. A patch that rebuilds the kernel starts instead from
# zroot/kpatch/kernel@<release>-ctf: that source with its GENERIC kernel built as the release
# builds it, CTF for DTrace included, once per release (about two hours in the emulated dev VM),
# so that each patch rebuilds only the files it changes and
# those that include them. Each build clones one of them at the same path (what a build records,
# the paths in its dependency lists, stays true for the next), applies the patch's sys/ part,
# builds, copies what it built back to build/kpatch/<id>/ and removes the clone. The bundle is
# signed with build/kpatch/dev-key, which `key` makes once and whose public half it prints: what
# a computer's glassd.toml `[hardware] keys` must name to take these bundles.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

vm="$FREEBSD_SCRIPTS_DIR/vm.sh"
out_dir=$BUILD_DIR/kpatch
key=$out_dir/dev-key
src_ds=zroot/kpatch/src
src_snap=$src_ds@$FREEBSD_RELEASE
kernel_ds=zroot/kpatch/kernel
# -ctf: built with CTF, as GENERIC asks (makeoptions WITH_CTF); an earlier base built without it
# is replaced, since a kernel links only from objects that all carry it.
kernel_snap=$kernel_ds@$FREEBSD_RELEASE-ctf
build_root=/var/cache/glasshouse-build
# Where every build runs, one at a time.
at=$build_root/work
clone=zroot/kpatch/build

kpatch() { cargo run -q --manifest-path "$REPO_ROOT/Cargo.toml" -p glass-kpatch -- "$@"; }

cmd_key() {
	mkdir -p "$out_dir"
	if [[ -f $key ]]; then
		kpatch public --key "$key"
	else
		kpatch keygen --out "$key"
	fi
}

# The pinned release's sys/ in the VM, once.
ensure_source() {
	if "$vm" ssh "zfs list -H -t snapshot $src_snap" >/dev/null 2>&1; then
		return
	fi
	log "unpacking $FREEBSD_RELEASE's kernel source into the VM (once)"
	"$FREEBSD_SCRIPTS_DIR/fetch.sh" src
	"$vm" ssh "zfs list zroot/kpatch >/dev/null 2>&1 || zfs create -o mountpoint=$build_root -o compression=lz4 zroot/kpatch
		zfs list $src_ds >/dev/null 2>&1 || zfs create $src_ds"
	"$vm" scp-in "$CACHE_DIR/$FREEBSD_SRC_TXZ" "$build_root/src.txz"
	"$vm" ssh "cd $build_root/src && tar -xf ../src.txz --strip-components 2 usr/src/sys && rm ../src.txz &&
		zfs snapshot $src_snap"
}

# What went wrong, from the last build's log.
build_errors() { "$vm" ssh "grep -E 'error|Error' $at/build.log | head -20; tail -5 $at/build.log" || true; }

# GENERIC, as the release builds it (CTF for DTrace included), without its modules (a patch names
# those it rebuilds), in the tree at $at. `config` again each time: a patch can add files to the kernel (sys/conf/files);
# it leaves what didn't change as it was, so make rebuilds only what did.
build_kernel() {
	"$vm" ssh "cd $at/sys/amd64/conf && config -d $at/kobj GENERIC >/dev/null &&
		make -s -j4 -C $at/kobj -m /usr/share/mk -m $at/sys/conf SRCTOP=$at S=$at/sys \
			NO_MODULES=yes >$at/build.log 2>&1" || {
		build_errors
		return 1
	}
}

# The release's kernel built in its source, once.
ensure_kernel() {
	if "$vm" ssh "zfs list -H -t snapshot $kernel_snap" >/dev/null 2>&1; then
		return
	fi
	log "building $FREEBSD_RELEASE's kernel once (about two hours in the emulated dev VM)"
	"$vm" ssh "zfs destroy -r $kernel_ds 2>/dev/null; zfs clone -o mountpoint=$at $src_snap $kernel_ds"
	build_kernel || {
		"$vm" ssh "zfs destroy -r $kernel_ds" || true
		die "$FREEBSD_RELEASE's kernel didn't build"
	}
	"$vm" ssh "rm $at/build.log && zfs set canmount=noauto $kernel_ds && zfs umount $kernel_ds &&
		zfs snapshot $kernel_snap"
}

cmd_build() {
	local dir=${1:?usage: kernel-patch.sh build <patch dir> [--out <bundle>]} out=
	shift
	[[ ${1:-} == --out ]] && out=$2
	dir=$(realpath "$dir")
	[[ -f $dir/patch.toml && -f $dir/patch.diff ]] || die "$dir: no patch.toml and patch.diff"
	[[ -f $key ]] || die "no signing key: kernel-patch.sh key"
	local id modules
	local kernel
	id=$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["id"])' "$dir/patch.toml")
	kernel=$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb")).get("kernel", False))' "$dir/patch.toml")
	# dir:file per module.
	modules=$(python3 -c 'import sys, tomllib
for m in tomllib.load(open(sys.argv[1], "rb")).get("module", []):
    print(m["dir"] + ":" + m["file"])' "$dir/patch.toml")
	out=${out:-$out_dir/$id.bundle.json}
	local work=$out_dir/$id
	rm -rf "$work" && mkdir -p "$work/modules"

	# The part of the patch that's the kernel's: sys/.
	python3 - "$dir/patch.diff" "$work/sys.diff" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
# Split before each file's header pair; keep files under sys/.
parts = re.split(r"(?m)^(?=diff --git |--- (?:a/|/dev/null))", text)
keep = []
for part in parts:
    m = re.search(r"(?m)^\+\+\+ b/(\S+)", part)
    if m and m.group(1).startswith("sys/"):
        # A `diff --git` part carries its own `---` line; a bare one starts with it.
        keep.append(part)
if not keep:
    sys.exit("the patch changes nothing under sys/")
open(sys.argv[2], "w").write("".join(keep))
PY

	ensure_source
	local from=$src_snap
	if [[ $kernel == True ]]; then
		ensure_kernel
		from=$kernel_snap
	fi
	"$vm" ssh "zfs destroy -r $clone 2>/dev/null; zfs clone -o mountpoint=$at $from $clone"
	"$vm" scp-in "$work/sys.diff" "$at/patch.diff"
	log "applying $id to $FREEBSD_RELEASE's source"
	"$vm" ssh "patch -p1 -s -E -d $at -i $at/patch.diff" || {
		"$vm" ssh "zfs destroy -r $clone" || true
		die "$id doesn't apply to $FREEBSD_RELEASE"
	}
	local m
	"$vm" ssh "mkdir -p $at/out"
	if [[ $kernel == True ]]; then
		log "building the kernel with it (GENERIC)"
		if ! { build_kernel && "$vm" ssh "cp $at/kobj/kernel $at/out/kernel"; }; then
			"$vm" ssh "zfs destroy -r $clone" || true
			die "the kernel didn't build"
		fi
	fi
	for m in $modules; do
		log "building ${m#*:} (sys/modules/${m%%:*})"
		# Objects in a tree of their own, its folders made as needed (some drivers build into
		# subfolders); <...> includes from the base's makefiles and the kernel's own.
		"$vm" ssh "make -s -j4 -m /usr/share/mk -m $at/sys/conf -C $at/sys/modules/${m%%:*} \
			SRCTOP=$at SYSDIR=$at/sys MAKEOBJDIRPREFIX=$at/obj WITH_AUTO_OBJ=yes MK_DEBUG_FILES=no \
			>$at/build.log 2>&1 &&
			cp \$(find $at/obj -name ${m#*:} -type f | head -1) $at/out/" || {
			build_errors
			"$vm" ssh "zfs destroy -r $clone" || true
			die "${m#*:} didn't build"
		}
	done
	"$vm" ssh "tar -C $at/out -cf - ." | tar -C "$work/modules" -xf -
	"$vm" ssh "zfs destroy -r $clone"
	kpatch bundle "$dir" --modules "$work/modules" --key "$key" --out "$out"
}

case ${1:-} in
key) cmd_key ;;
build)
	shift
	cmd_build "$@"
	;;
*) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2 && exit 2 ;;
esac
