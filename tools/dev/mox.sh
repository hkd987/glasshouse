#!/usr/bin/env bash
# Usage: tools/dev/mox.sh [build|path|serve DIR]
# The mail server the mail tests run against (ADR 0037, EM0): mox, pinned, built from Go's module
# proxy and checked against Go's checksum database. A build-time test tool only: it lives in the main
# checkout's build/tools/mox-<version>/ (shared by every worktree), never in the repository and never
# in the image.
#   build        build it if it isn't there (needs Go; a newer toolchain than the one installed is
#                fetched through GOTOOLCHAIN=auto), then print its path
#   path         print where it is (or would be)
#   serve DIR    run `mox localserve` with its state in DIR (what the tests do themselves)
# MOX overrides where the binary goes.
set -euo pipefail

version=0.0.17
root=$(cd "$(dirname "$0")/../.." && pwd)
# A worktree shares the main checkout's build/tools.
common=$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || echo "$root/.git")
main=$(dirname "$common")
dir=${MOX:+$(dirname "$MOX")}
dir=${dir:-$main/build/tools/mox-$version}
bin=${MOX:-$dir/mox}

case ${1:-build} in
path)
	echo "$bin"
	;;
build)
	if [[ -x $bin ]]; then
		echo "$bin"
		exit 0
	fi
	command -v go >/dev/null || PATH=$PATH:/usr/local/go/bin
	mkdir -p "$dir"
	# Its own module and build caches, removed afterwards: the disk is small and nothing else here
	# builds Go.
	scratch=$(mktemp -d "$dir/.go.XXXXXX")
	trap 'chmod -R u+w "$scratch" 2>/dev/null; rm -rf "$scratch"' EXIT
	GOTOOLCHAIN=auto GOPATH=$scratch/path GOMODCACHE=$scratch/mod GOCACHE=$scratch/cache \
		GOFLAGS=-mod=mod CGO_ENABLED=0 GOBIN=$dir \
		go install "github.com/mjl-/mox@v$version" >&2
	"$bin" version >&2
	echo "$bin"
	;;
serve)
	[[ -n ${2:-} ]] || { echo "usage: $0 serve DIR" >&2; exit 2; }
	exec "$bin" localserve -dir "$2"
	;;
*)
	echo "usage: $0 [build|path|serve DIR]" >&2
	exit 2
	;;
esac
