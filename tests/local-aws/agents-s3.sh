#!/usr/bin/env bash
# Glasshouse Agents against AWS on this machine (deploy/local; make
# aws-local-test runs it): an agent's computer sleeps to S3 and wakes the
# same (ADR 0065 §11, ADR 0066).
#
# glassd and the front run as tools/dev/agents-run.sh runs them, with
# GLASS_DEV_STORE=s3: moto's bucket under the stack's KMS key, signed with
# the stand-in instance role's keys. Through the `glass` CLI, an agent's
# computer writes GLASS_LOCAL_TEST_MB (default 300) of random bytes, a
# small file and a 1 GiB sparse file. It sleeps, and the script checks
# that only the computer's record is left on the host and that every
# object in the bucket is under the stack's key. Woken and asleep again
# with nothing changed, nothing new goes up. The next task wakes it, and
# every file's SHA-256 must match.
#
# The scripted model runs the commands (no network, no key): a prompt that
# names computer_run calls it with the JSON after the name.
set -Eeuo pipefail
trap 'echo "agents-s3: stopped at line $LINENO: $BASH_COMMAND" >&2' ERR

repo=$(cd "$(dirname "$0")/../.." && pwd)
env="$repo/build/local-aws/env"
[ -s "$env" ] || {
	echo "agents-s3: needs AWS on this machine: make aws-local" >&2
	exit 1
}
# shellcheck source=/dev/null
. "$env"
bin="${CARGO_TARGET_DIR:-$repo/target}/debug"
mb=${GLASS_LOCAL_TEST_MB:-300}
host="$repo/build/local-aws/host-a"
port=${GLASS_LOCAL_TEST_PORT:-7521}
failed=0

pass() { printf '  ok    %s\n' "$*"; }
fail() {
	printf '  FAIL  %s\n' "$*"
	failed=1
}
now() { date +%s.%N; }
since() { python3 -c 'import sys; print(f"{float(sys.argv[2]) - float(sys.argv[1]):.1f} s")' "$1" "$(now)"; }
s3() { curl -fsS "$GLASS_LOCAL_METERING/_fake/s3?prefix=$1"; }
field() { python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"; }

rm -rf "$host"
mkdir -p "$host"
# Its own process group, so stopping it stops glassd and the front.
GLASS_DEV_DIR=$host GLASS_DEV_RUN=$host/run GLASS_DEV_STORE=s3 GLASS_DEV_SKIP_BUILD=1 \
	GLASS_AGENTS_LISTEN=127.0.0.1:$port GLASS_AGENTS_TOKEN_FILE=$host/agents.json \
	setsid "$repo/tools/dev/agents-run.sh" >"$host/stack.log" 2>&1 &
stack=$!
trap 'kill -TERM -- "-$stack" 2>/dev/null || true; wait "$stack" 2>/dev/null || true' EXIT
for _ in $(seq 1 600); do
	[ -s "$host/agents.json" ] && break
	kill -0 "$stack" 2>/dev/null || {
		echo "agents-s3: the stack stopped; its log:" >&2
		tail -20 "$host/stack.log" "$host/glassd.log" >&2
		exit 1
	}
	sleep 0.1
done
GLASS_AGENTS_URL=$(field url <"$host/agents.json")
GLASS_AGENTS_TOKEN=$(field token <"$host/agents.json")
export GLASS_AGENTS_URL GLASS_AGENTS_TOKEN
glass="$bin/glass"

# Runs a command on the agent's computer; prints what the screen showed.
run() {
	"$glass" ask keeper "computer_run $(python3 -c 'import json,sys; print(json.dumps({"command": sys.argv[1]}))' "$1")"
}
sums() { run 'cd ~/work && sha256sum big.bin note.txt sparse.img' | grep -oE '[0-9a-f]{64}  [a-z.]+' | sort -u; }

echo "agents-s3: a computer in S3 at $GLASS_LOCAL_S3 (bucket $GLASS_LOCAL_BUCKET)"
"$glass" new desktop-agent --name Keeper >/dev/null
started=$(now)
run "mkdir -p ~/work && head -c ${mb}M /dev/urandom >~/work/big.bin && printf 'kept\n' >~/work/note.txt && truncate -s 1G ~/work/sparse.img" >/dev/null
before=$(sums)
if [ "$(printf '%s\n' "$before" | wc -l)" = 3 ]; then
	pass "the computer wrote ${mb} MB, a small file and a 1 GiB sparse one ($(since "$started"))"
else
	fail "the computer's files: $before"
fi
record=$(find "$host" -name computer.json -path '*keeper*' | head -1)
computer=$(dirname "$record")
on_host=$(du -sm "$computer" | cut -f1)

started=$(now)
"$glass" sleep keeper >/dev/null
slept=$(since "$started")
left=$(find "$computer" -mindepth 1 -maxdepth 1 ! -name computer.json | wc -l)
if [ "$left" = 0 ]; then
	pass "asleep in $slept: of its ${on_host} MB, only its record is left on the host"
else
	fail "asleep, but $left entries are left in $computer"
fi
report=$(s3 computers/)
objects=$(field objects <<<"$report")
encryption=$(field encryption <<<"$report")
all=$(s3 "")
if [ "$(field encryption <<<"$all")" = "{'aws:kms $GLASS_LOCAL_KMS_KEY': $(field objects <<<"$all")}" ]; then
	pass "all $(field objects <<<"$all") objects in the bucket are under the stack's KMS key ($objects of them this computer's versions)"
else
	fail "objects not under the stack's key: $(field encryption <<<"$all") (this computer's: $encryption)"
fi

# Woken and asleep again with nothing changed: nothing new goes up.
chunks_before=$(field objects <<<"$(s3 chunks/)")
started=$(now)
"$glass" wake keeper >/dev/null
woke=$(since "$started")
"$glass" sleep keeper >/dev/null
chunks_after=$(field objects <<<"$(s3 chunks/)")
if [ "$chunks_after" = "$chunks_before" ]; then
	pass "awake in $woke and asleep again unchanged: no new chunks ($chunks_after in the bucket)"
else
	fail "awake and asleep again unchanged, but chunks went from $chunks_before to $chunks_after"
fi

# Woken by the next task: every byte came back.
after=$(sums)
if [ "$after" = "$before" ]; then
	pass "woken by the next task, every file the same (SHA-256 of all three)"
else
	fail "woken, but the files differ: before $before; after $after"
fi

if [ "$failed" = 0 ]; then
	echo "agents-s3: passed"
else
	echo "agents-s3: FAILED; logs in $host" >&2
fi
exit "$failed"
