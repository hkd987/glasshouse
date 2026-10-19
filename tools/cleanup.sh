#!/usr/bin/env bash
# Usage: tools/cleanup.sh [--apply] [--deep]
# What's using the disk, and what can go (the `cleanup` skill says when to run it). Without --apply it
# only reports. With --apply it removes:
#   - worktrees under .claude/worktrees whose branch is merged into main and that no agent holds
#     (a locked worktree belongs to a running agent: left alone), and their branches;
#   - incremental build caches of debug builds (target/**/incremental, shell/target/**/incremental),
#     rebuilt as needed, slower once; with --deep the `vm` profile's too, which makes the next
#     `make freebsd vm-deploy` take minutes instead of seconds;
#   - test binaries left from earlier builds of the same test (only the newest of each stays);
#   - split debug info (.dwo) older than 3 hours: cargo leaves one per codegen unit per build;
#   - build/dist (staging, rewritten by every stage-dist.sh) and build/logs;
#   - VM directories under build/ other than build/vm whose QEMU isn't running, and a bake VM
#     (build/vm/bake) a failed image build left behind;
#   - a package-list root that an interrupted `make image-pkgbase` left half built
#     (build/images/.*.part); the finished root it keeps for the next build stays;
#   - the compressed VM image download once the disk VMs start from was made from it;
#   - base-system packages outside release.env's pinned catalog (build/cache/freebsd/*/pkgbase);
#   - with --deep, FreeBSD downloads of releases other than release.env's (build/cache/freebsd/*).
# It never touches build/vm itself (the dev VM), the current release's downloads or build/freebsd
# (the sysroot). Either way it resumes any VM QEMU paused because the disk filled.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
apply=false deep=false
for arg in "$@"; do
	case $arg in
	--apply) apply=true ;;
	--deep) deep=true ;;
	*) echo "usage: tools/cleanup.sh [--apply] [--deep]" >&2 && exit 2 ;;
	esac
done

# du fails on a path that isn't there (an unmatched glob, such as no worktrees); that's no reason
# to stop.
size() { du -sh "$@" 2>/dev/null | sort -h || true; }
gone() {
	if $apply; then
		rm -rf "$@"
		echo "removed: $*"
	else
		echo "would remove: $(du -shc "$@" 2>/dev/null | tail -1 | cut -f1) in $*"
	fi
}

echo "== disk"
df -h / | tail -1
size target shell/target build/* .claude/worktrees/* 2>/dev/null | tail -12

echo "== worktrees"
git worktree prune
while read -r path branch; do
	[[ $path == "$root"/.claude/worktrees/* ]] || continue
	if git worktree list --porcelain | grep -A4 "^worktree $path$" | grep -q '^locked'; then
		echo "in use (locked; once its agent has reported and it's merged: git worktree unlock): $path"
	elif git merge-base --is-ancestor "$branch" main 2>/dev/null; then
		if $apply; then
			git worktree remove --force "$path" && git branch -D "${branch#refs/heads/}" >/dev/null
			echo "removed merged worktree: $path"
		else
			echo "would remove merged worktree: $path ($(du -sh "$path" | cut -f1))"
		fi
	else
		echo "not merged, kept: $path ($branch)"
	fi
done < <(git worktree list --porcelain | awk '/^worktree /{p=$2} /^branch /{print p, $2}')

echo "== build caches"
mapfile -t incremental < <(find target shell/target -maxdepth 3 -type d -name incremental 2>/dev/null |
	if $deep; then cat; else grep -v '/vm/incremental$'; fi)
((${#incremental[@]})) && gone "${incremental[@]}"
# Test binaries from earlier builds: each rebuild leaves the old one beside the new, under a new
# hash. Only the newest of each name stays (two crates' tests of the same name: cargo relinks the
# other when it next runs).
mapfile -t stale < <(find target/*/deps target/*/*/deps shell/target/*/deps shell/target/*/*/deps \
	-maxdepth 1 -type f -perm -u+x -regextype posix-extended -regex '.*/[a-z0-9_]+-[0-9a-f]{16}' \
	-printf '%T@ %p\n' 2>/dev/null |
	sort -rn | awk '{ key = $2; sub(/-[0-9a-f]+$/, "", key); if (seen[key]++) print $2 }')
((${#stale[@]})) && gone "${stale[@]}"
# Split debug info (split-debuginfo = "unpacked"): one .dwo per codegen unit of every build, and
# cargo never removes an earlier build's. Hundreds of thousands pile up; those not written in the
# last 3 hours go (a current one is written again by its next build; until then, only backtraces
# lose line numbers).
dwo() {
	find target/*/deps target/*/*/deps shell/target/*/deps shell/target/*/*/deps -maxdepth 1 \
		-name '*.dwo' -mmin +180 "$@" 2>/dev/null
}
if $apply; then
	dwo -delete || true
	echo "removed: split debug info older than 3 hours"
else
	echo "would remove: $(dwo -printf '%s\n' | awk '{ s += $1 } END { printf "%.1fG", s / 1e9 }') of split debug info older than 3 hours"
fi
# An image build deploys build/dist while its bake VM runs: leave it until then.
# The bake scripts keep their pid in build/images/.bake.pid while they run, QEMU up or not.
bake_running() {
	local pid
	for pid in build/images/.bake.pid build/vm/bake/qemu.pid; do
		[[ -f $pid ]] && kill -0 "$(cat "$pid")" 2>/dev/null && return 0
	done
	return 1
}
if bake_running; then echo "an image build is running: build/dist kept"; else [[ -e build/dist ]] && gone build/dist; fi
[[ -e build/logs ]] && gone build/logs

echo "== VMs"
running() { [[ -f $1/qemu.pid ]] && kill -0 "$(cat "$1/qemu.pid")" 2>/dev/null; }
for dir in build/*/ build/vm/*/; do
	dir=${dir%/}
	[[ -e $dir/qmp.sock || -e $dir/disk.qcow2 ]] || continue
	[[ $dir == build/vm ]] && continue
	if [[ $dir == build/vm/bake ]] && bake_running; then
		echo "an image build is running, kept: $dir"
		continue
	fi
	if running "$dir"; then
		echo "running, kept: $dir"
	else
		gone "$dir"
	fi
done

echo "== images and downloads"
size build/images build/cache/freebsd/*
# A root an interrupted package-list build left half made; none while a bake VM runs.
if ! bake_running; then
	mapfile -t partial < <(find build/images -maxdepth 1 -name '.*.part' 2>/dev/null)
	((${#partial[@]})) && gone "${partial[@]}"
fi
# shellcheck source=os/freebsd/release.env
. os/freebsd/release.env
cache=build/cache/freebsd/$FREEBSD_RELEASE
# The VM image's download, once the disk VMs start from records that it was made from it: fetch.sh
# doesn't fetch it again.
xz=$cache/$FREEBSD_VM_IMAGE
if [[ -f $xz && $(cat "$cache/${FREEBSD_VM_IMAGE%.xz}.sha256-ok" 2>/dev/null) == "$FREEBSD_VM_IMAGE_SHA256" ]]; then
	gone "$xz" "$xz.sha256-ok"
fi
# Base-system packages (pkg-mirror.py keeps each one a build asks for): only the pinned catalog's
# are of use. Repositories other than the pinned one go, and packages the pinned catalog no longer
# names, once FreeBSD published new ones and release.env moved to them.
pkgbase=$cache/pkgbase
for dir in "$pkgbase"/*/*/; do
	dir=${dir%/}
	[[ -d $dir && ${dir#"$pkgbase"/} != "$FREEBSD_PKGBASE_REPO" ]] && gone "$dir"
done
repo=$pkgbase/$FREEBSD_PKGBASE_REPO
if [[ -f $repo/$FREEBSD_PKGBASE_CATALOG && -d $repo/Hashed &&
	$(sha256sum <"$repo/$FREEBSD_PKGBASE_CATALOG") == "$FREEBSD_PKGBASE_CATALOG_SHA256  -" ]]; then
	mapfile -t unused < <(tar -xOf "$repo/$FREEBSD_PKGBASE_CATALOG" data | python3 -c '
import json, os, sys
named = {os.path.basename(p["repopath"]) for p in json.load(sys.stdin)["packages"]}
for name in sorted(os.listdir(sys.argv[1])):
    if name.endswith(".pkg") and name not in named:
        print(os.path.join(sys.argv[1], name))
' "$repo/Hashed")
	((${#unused[@]})) && gone "${unused[@]}"
fi
if $deep; then
	for dir in build/cache/freebsd/*/; do
		dir=${dir%/}
		[[ ${dir##*/} != "$FREEBSD_RELEASE" ]] && gone "$dir"
	done
fi
# A VM paused because the disk filled (status io-error) carries on once there's room.
python3 - "$root" <<'EOF'
import glob, json, socket, sys
for path in glob.glob(f"{sys.argv[1]}/build/*/qmp.sock") + glob.glob(f"{sys.argv[1]}/.claude/worktrees/*/build/*/qmp.sock"):
    try:
        s = socket.socket(socket.AF_UNIX); s.settimeout(3); s.connect(path)
        f = s.makefile("rw"); f.readline()
        def ask(cmd):
            f.write(json.dumps({"execute": cmd}) + "\n"); f.flush()
            while True:
                line = json.loads(f.readline())
                if "event" not in line:
                    return line
        ask("qmp_capabilities")
        status = ask("query-status")["return"]["status"]
        if status == "io-error":
            ask("cont")
            print(f"resumed a VM paused by a full disk: {path}")
    except OSError:
        pass
EOF
$apply || echo "(dry run: tools/cleanup.sh --apply removes the above)"
