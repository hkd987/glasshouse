#!/usr/bin/env bash
# Glasshouse Agents against AWS on this machine (deploy/local; make
# aws-local-test runs it): an agent's computer sleeps to S3 and wakes the
# same, and when its host is replaced, another host wakes it (ADR 0065
# §11, ADR 0066).
#
# glassd and the front run as tools/dev/agents-run.sh runs them, with
# GLASS_DEV_STORE=s3: moto's bucket under the stack's KMS key, signed with
# the stand-in instance role's keys. Through the `glass` CLI, an agent's
# computer writes GLASS_LOCAL_TEST_MB (default 300) of bytes that don't
# repeat, a small file and a 1 GiB sparse file. Then:
#   1. It sleeps. Only its record is left on the host, and every object
#      in the bucket is under the stack's key.
#   2. Woken and asleep again with nothing changed, nothing new goes up.
#   3. The next task wakes it, and every file's SHA-256 matches.
#   4. Its host is told it's being replaced (the stand-in metadata
#      service's lifecycle state, as the Auto Scaling group's hook sets
#      it). It puts the computer to sleep by itself and refuses new tasks.
#   5. Another host, which never had it, lists it asleep and wakes it with
#      every file the same.
#
# The scripted model runs the commands (no network, no key): a prompt that
# names computer_run calls it with the JSON after the name. The bytes are
# the same each run (AES-CTR over zeros), so moto, which keeps everything
# in memory, holds one copy however often this runs.
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
glass="$bin/glass"
mb=${GLASS_LOCAL_TEST_MB:-300}
port=${GLASS_LOCAL_TEST_PORT:-7521}
failed=0
stack=""

pass() { printf '  ok    %s\n' "$*"; }
fail() {
	printf '  FAIL  %s\n' "$*"
	failed=1
}
now() { date +%s.%N; }
since() { python3 -c 'import sys; print(f"{float(sys.argv[2]) - float(sys.argv[1]):.1f} s")' "$1" "$(now)"; }
s3() { curl -fsS "$GLASS_LOCAL_METERING/_fake/s3?prefix=$1"; }
field() { python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"; }

# Starts host $1 (glassd and the front) on port $2, in its own process
# group, so stopping it stops both.
start_host() {
	host="$repo/build/local-aws/$1"
	rm -rf "$host"
	mkdir -p "$host"
	GLASS_DEV_DIR=$host GLASS_DEV_RUN=$host/run GLASS_DEV_STORE=s3 GLASS_DEV_SKIP_BUILD=1 \
		GLASS_DEV_HOST=$1 GLASS_AGENTS_LISTEN=127.0.0.1:$2 GLASS_AGENTS_TOKEN_FILE=$host/agents.json \
		setsid "$repo/tools/dev/agents-run.sh" >"$host/stack.log" 2>&1 &
	stack=$!
	for _ in $(seq 1 600); do
		[ -s "$host/agents.json" ] && break
		kill -0 "$stack" 2>/dev/null || {
			echo "agents-s3: $1 stopped; its log:" >&2
			tail -20 "$host/stack.log" "$host/glassd.log" >&2
			exit 1
		}
		sleep 0.1
	done
	GLASS_AGENTS_URL=$(field url <"$host/agents.json")
	GLASS_AGENTS_TOKEN=$(field token <"$host/agents.json")
	export GLASS_AGENTS_URL GLASS_AGENTS_TOKEN
}
stop_host() {
	kill -TERM -- "-$stack" 2>/dev/null || true
	wait "$stack" 2>/dev/null || true
}
trap 'kill -TERM -- "-$stack" 2>/dev/null || true' EXIT

# Runs a command on the agent's computer; prints what the screen showed.
run() {
	"$glass" ask keeper "computer_run $(python3 -c 'import json,sys; print(json.dumps({"command": sys.argv[1]}))' "$1")"
}
sums() { run 'cd ~/work && sha256sum big.bin note.txt sparse.img' | grep -oE '[0-9a-f]{64}  [a-z.]+' | sort -u; }
# The computer's state, as the front shows it.
state() {
	curl -fsS -H "Authorization: Bearer $GLASS_AGENTS_TOKEN" "$GLASS_AGENTS_URL/api/v1/agents" |
		python3 -c 'import json,sys; print(next((a["computer"]["state"] for a in json.load(sys.stdin) if a["id"] == "keeper"), "none"))'
}

echo "agents-s3: a computer in S3 at $GLASS_LOCAL_S3 (bucket $GLASS_LOCAL_BUCKET)"
start_host host-a "$port"
"$glass" new desktop-agent --name Keeper >/dev/null
started=$(now)
run "mkdir -p ~/work && head -c ${mb}M /dev/zero | openssl enc -aes-128-ctr -nosalt -K 676c617373686f757365206167656e74 -iv 0 >~/work/big.bin && printf 'kept\n' >~/work/note.txt && truncate -s 1G ~/work/sparse.img" >/dev/null
before=$(sums)
if [ "$(printf '%s\n' "$before" | wc -l)" = 3 ]; then
	pass "the computer wrote ${mb} MB, a small file and a 1 GiB sparse one ($(since "$started"))"
else
	fail "the computer's files: $before"
fi
record=$(find "$host" -name computer.json -path '*keeper*' | head -1)
computer=$(dirname "$record")
key=$(field key <"$record")
on_host=$(du -sm "$computer" | cut -f1)

# 1. Asleep: only its record here; everything in the bucket under the key.
started=$(now)
"$glass" sleep keeper >/dev/null
slept=$(since "$started")
left=$(find "$computer" -mindepth 1 -maxdepth 1 ! -name computer.json | wc -l)
if [ "$left" = 0 ]; then
	pass "asleep in $slept: of its ${on_host} MB, only its record is left on the host"
else
	fail "asleep, but $left entries are left in $computer"
fi
all=$(s3 "")
if [ "$(field encryption <<<"$all")" = "{'aws:kms $GLASS_LOCAL_KMS_KEY': $(field objects <<<"$all")}" ]; then
	pass "all $(field objects <<<"$all") objects in the bucket are under the stack's KMS key, its own as $key"
else
	fail "objects not under the stack's key: $(field encryption <<<"$all")"
fi

# 2. Woken and asleep again with nothing changed: nothing new goes up.
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

# 3. Woken by the next task: every byte came back.
after=$(sums)
if [ "$after" = "$before" ]; then
	pass "woken by the next task, every file the same (SHA-256 of all three)"
else
	fail "woken, but the files differ: before $before; after $after"
fi

# 4. Its host is being replaced: it sleeps by itself, and takes no new task.
curl -fsS -X POST "$GLASS_LOCAL_METERING/_fake/lifecycle?host=host-a&state=Terminated" >/dev/null
started=$(now)
for _ in $(seq 1 120); do
	[ "$(state)" = asleep ] && break
	sleep 0.5
done
if [ "$(state)" = asleep ]; then
	pass "host A is being replaced: it put the computer to sleep by itself in $(since "$started")"
else
	fail "host A is being replaced, but the computer is $(state)"
fi
refused=$(run 'true' 2>&1 || true)
if grep -q "being replaced" <<<"$refused"; then
	pass "a task on host A is refused: \"$(grep -o 'this host is being replaced[^)]*' <<<"$refused" | head -1)\""
else
	fail "a task on host A wasn't refused: $refused"
fi
stop_host
curl -fsS -X POST "$GLASS_LOCAL_METERING/_fake/lifecycle?host=host-a&state=InService" >/dev/null

# 5. Another host, which never had it, finds it asleep and wakes it.
start_host host-b "$((port + 1))"
if [ "$(state)" = asleep ]; then
	pass "host B, which never had it, lists Keeper asleep (its record in the store)"
else
	fail "host B lists Keeper as $(state)"
fi
started=$(now)
after=$(sums)
if [ "$after" = "$before" ]; then
	pass "host B woke it in $(since "$started") with every file the same"
else
	fail "host B woke it, but the files differ: before $before; after $after"
fi
stop_host
trap - EXIT

if [ "$failed" = 0 ]; then
	# Its computer's few hundred MB here, and its versions in the bucket
	# (the chunks stay: the next run's are the same).
	curl -fsS -X DELETE "$GLASS_LOCAL_METERING/_fake/s3?prefix=computers/$key/" >/dev/null
	rm -rf "$repo/build/local-aws/host-a" "$repo/build/local-aws/host-b"
	echo "agents-s3: passed"
else
	echo "agents-s3: FAILED; logs in $repo/build/local-aws/host-a and host-b" >&2
fi
exit "$failed"
