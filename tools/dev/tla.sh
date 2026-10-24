#!/usr/bin/env bash
# Usage: tools/dev/tla.sh [fetch|path|check [Spec ...]|tlc ARGS...]
# TLC, the TLA+ model checker (MIT), for the models of key workflows in specs/tla (ADR 0051). A
# build-time tool, like mox: pinned, checked against its SHA-256, kept in the main checkout's
# build/tools/tla-<version>/ (shared by every worktree), never in the repository or the image.
#   fetch        download it if it isn't there, then print its path
#   path         print where it is (or would be)
#   check [S..]  model-check every spec in specs/tla (or those named) against its .cfg; fails on any
#                violated invariant, property or deadlock. Without Java it says so and passes, as the
#                mail tests do without mox.
#   tlc ARGS     run TLC with ARGS (exploring a model by hand)
# TLA_WORKERS sets each TLC's worker threads (default 1) and TLA_JOBS how many models are checked at
# once (default: the cores over TLA_WORKERS). TLC gains less from a second worker than from a
# second model: four models at once check every one in about two minutes on four cores.
set -euo pipefail

version=1.7.4
sha256=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
root=$(cd "$(dirname "$0")/../.." && pwd)
# A worktree shares the main checkout's build/tools.
common=$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || echo "$root/.git")
main=$(dirname "$common")
dir=$main/build/tools/tla-$version
jar=$dir/tla2tools.jar
specs=$root/specs/tla

fetch() {
	if [[ -f $jar ]]; then
		return
	fi
	mkdir -p "$dir"
	local part=$jar.part
	curl -fsSL -o "$part" "https://github.com/tlaplus/tlaplus/releases/download/v$version/tla2tools.jar"
	if ! echo "$sha256  $part" | sha256sum -c --status; then
		rm -f "$part"
		echo "tla: tla2tools.jar $version doesn't match its pinned SHA-256" >&2
		exit 1
	fi
	mv "$part" "$jar"
}

tlc() {
	# Java's own proxy settings (JAVA_TOOL_OPTIONS) would print on every run.
	env -u JAVA_TOOL_OPTIONS java -XX:+UseParallelGC -cp "$jar" tlc2.TLC "$@"
}

check() {
	if ! command -v java >/dev/null; then
		echo "tla: skipped, no java (the models need TLC, which runs on Java 11 or later)"
		return
	fi
	fetch
	local names=("$@")
	if ((${#names[@]} == 0)); then
		for f in "$specs"/*.cfg; do
			names+=("$(basename "$f" .cfg)")
		done
	fi
	local failed=0 out name
	out=$(mktemp -d)
	trap 'rm -rf "$out"' RETURN
	# Several models at once, each with TLA_WORKERS threads: the cores, shared.
	local parallel=${TLA_JOBS:-$(($(nproc) / ${TLA_WORKERS:-1}))}
	((parallel >= 1)) || parallel=1
	for name in "${names[@]}"; do
		while (($(jobs -rp | wc -l) >= parallel)); do
			wait -n || true
		done
		check_one "$name" "$out" >"$out/$name.result" &
	done
	wait
	for name in "${names[@]}"; do
		cat "$out/$name.result"
		grep -q '^  FAIL' "$out/$name.result" && failed=1
	done
	return $failed
}

# One model against its .cfg: a line saying how it went, and TLC's trace if it failed.
check_one() {
	local name=$1 out=$2
	local cfg=$specs/$name.cfg
	local spec=$specs/${name%%_*}.tla
	[[ -f $spec ]] || spec=$specs/$name.tla
	local log=$out/$name.log started=$SECONDS
	if (cd "$specs" && tlc -workers "${TLA_WORKERS:-1}" -metadir "$out/$name.states" -config "$cfg" "$spec") >"$log" 2>&1; then
		printf '  ok    %-28s %5ss  %s\n' "$name" "$((SECONDS - started))" \
			"$(grep -oE '[0-9,]+ distinct states found' "$log" | tail -1)"
	else
		printf '  FAIL  %-28s %5ss\n' "$name" "$((SECONDS - started))"
		sed -n '/^Error:/,$p' "$log" | head -80
	fi
}

case ${1:-check} in
fetch)
	fetch
	echo "$jar"
	;;
path)
	echo "$jar"
	;;
check)
	shift || true
	check "$@"
	;;
tlc)
	shift
	fetch
	tlc "$@"
	;;
*)
	echo "usage: $0 [fetch|path|check [Spec ...]|tlc ARGS...]" >&2
	exit 2
	;;
esac
