#!/usr/bin/env bash
# People's schedules across two agent hosts (ADR 0069): make aws-local
# first; make aws-local-test runs it.
#
# Host A runs glassd and the front behind the stand-in load balancer; host
# B runs glassd only. Both keep schedules in moto's DynamoDB table and
# computers in its bucket, as a stack's hosts do. Dana adds an agent, which
# writes a file on her computer. Then, each check printed:
#   - a schedule for the next minute runs once, with both hosts looking;
#   - host A is told it's being replaced: it puts her computer to sleep in
#     S3 and claims nothing more. Her next schedule runs on host B, which
#     never saw her: it wakes her computer from S3 and the file is there;
#   - a schedule in another time zone runs at that zone's time;
#   - one she turned off doesn't run;
#   - Sam sees none of hers and can't turn off, remove or run his
#     schedules on her agents; bad schedules, zones and directions, and a
#     21st, are refused; all on the record.
# Needs root: it makes the front's account, as the stack does. Takes about
# four minutes: schedules run on the minute.
set -Eeuo pipefail
trap 'echo "schedules: stopped at line $LINENO: $BASH_COMMAND" >&2' ERR

repo=$(cd "$(dirname "$0")/../.." && pwd)
env="$repo/build/local-aws/env"
[ -s "$env" ] || {
	echo "schedules: needs AWS on this machine: make aws-local" >&2
	exit 1
}
# shellcheck source=/dev/null
. "$env"
lb=$GLASS_LOCAL_LOAD_BALANCER
front=http://127.0.0.1:7500
# Short: a computer's task socket must fit in a Unix socket's 108 bytes.
a="$repo/build/sa"
b="$repo/build/sb"
work=$(mktemp -d)
failed=0
stack_a=""
stack_b=""

pass() { printf '  ok    %s\n' "$*"; }
fail() {
	printf '  FAIL  %s\n' "$*"
	failed=1
}
expect() { # expect <what> <want> <got>
	if [ "$2" = "$3" ]; then pass "$1"; else fail "$1: wanted $2, got $3 $(head -c 300 "$work/body")"; fi
}
json() { python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2], {"d": d}))' "$work/body" "$1"; }

signin() { curl -s -o /dev/null -c "$work/$1.jar" -d "email=$1@example.com&name=$2&next=/" "$lb/oauth2/sign-in"; }
as() { # <person> <method> <path> [json]: status, body in $work/body
	local data=()
	[ -n "${4:-}" ] && data=(-H 'content-type: application/json' -d "$4")
	curl -s -o "$work/body" -w '%{http_code}' -b "$work/$1.jar" -X "$2" "${data[@]}" "$lb/api/v1/$3"
}
# What a scripted agent runs on its computer.
run() { python3 -c 'import json,sys; print("computer_run " + json.dumps({"command": sys.argv[1]}))' "$1"; }
schedule() { # <when> <zone> <command>: the JSON for a new schedule
	python3 -c 'import json,sys; print(json.dumps({"prompt": sys.argv[3], "when": sys.argv[1], "zone": sys.argv[2]}))' "$1" "$2" "$(run "$3")"
}
# The minute a schedule can be set for: at least 35 s off, as "HH:MM" in
# zone $1, then its Unix time.
slot() {
	python3 - "$1" "${2:-0}" <<'PY'
import sys, time, datetime, zoneinfo
zone, extra = sys.argv[1], int(sys.argv[2])
now = time.time()
at = (int(now) // 60 + 1) * 60
if at - now < 35:
    at += 60
at += extra * 60
local = datetime.datetime.fromtimestamp(at, zoneinfo.ZoneInfo(zone))
print(local.strftime("%H:%M"), at)
PY
}
last_outcome() { # <person> <schedule id>
	as "$1" GET schedules >/dev/null
	python3 -c 'import json,sys; s=[s for s in json.load(open(sys.argv[1])) if s["id"]==sys.argv[2]]; print((s[0].get("last") or {}).get("outcome","none") if s else "gone")' "$work/body" "$2"
}
wait_outcome() { # <person> <id> <until unix>: the outcome once it's ended, or what it was
	local got
	while :; do
		got=$(last_outcome "$1" "$2")
		case "$got" in done | failed | refused | missed | stopped) break ;; esac
		[ "$(date +%s)" -gt "$3" ] && break
		sleep 3
	done
	echo "$got"
}
started() { grep -c "Started the scheduled run" "$1/state/audit/audit.log" 2>/dev/null || true; }
owner_of() { python3 -c 'import hashlib,sys; print("p"+hashlib.sha256(sys.argv[1].encode()).hexdigest()[:24])' "$1"; }
sub_of() { curl -s "$GLASS_LOCAL_METERING/_fake/config" | python3 -c 'import json,sys; print([p["sub"] for p in json.load(sys.stdin)["people"] if p["email"]==sys.argv[1]][0])' "$1"; }

stop() {
	for s in $stack_a $stack_b; do kill -TERM -- "-$s" 2>/dev/null || true; done
	for s in $stack_a $stack_b; do wait "$s" 2>/dev/null || true; done
}
trap 'stop; rm -rf "$work"' EXIT

# Computers of Dana's and Sam's that earlier runs left in the bucket.
for who in dana sam; do
	curl -fsS -X DELETE "$GLASS_LOCAL_METERING/_fake/s3?prefix=computers/$(owner_of "$(sub_of "$who@example.com")")" >/dev/null || true
done

rm -rf "$a" "$b" "$a-computers" "$b-computers" "$a/state-computers" "$b/state-computers"
mkdir -p "$a" "$b"
curl -fsS -X POST "$GLASS_LOCAL_METERING/_fake/lifecycle?host=host-a&state=InService" >/dev/null
curl -fsS -X POST "$GLASS_LOCAL_METERING/_fake/lifecycle?host=host-b&state=InService" >/dev/null
GLASS_DEV_DIR=$a GLASS_DEV_RUN=$a/run GLASS_DEV_SKIP_BUILD=1 GLASS_DEV_STORE=s3 GLASS_DEV_HOST=host-a \
	GLASS_AGENTS_IDENTITY=load-balancer GLASS_AGENTS_LISTEN=127.0.0.1:7500 \
	setsid "$repo/tools/dev/agents-run.sh" >"$a/stack.log" 2>&1 &
stack_a=$!
for _ in $(seq 1 300); do
	curl -s -o /dev/null "$front/" && break
	kill -0 "$stack_a" 2>/dev/null || {
		tail -20 "$a/stack.log" "$a/glassd.log" >&2
		exit 1
	}
	sleep 0.1
done
GLASS_DEV_DIR=$b GLASS_DEV_RUN=$b/run GLASS_DEV_SKIP_BUILD=1 GLASS_DEV_STORE=s3 GLASS_DEV_HOST=host-b \
	GLASS_DEV_COMPUTERS=1 GLASS_DEV_PEOPLE=1 GLASS_DEV_SCRIPTED_MODEL=1 \
	setsid "$repo/tools/dev/host-run.sh" >"$b/glassd.log" 2>&1 &
stack_b=$!
for _ in $(seq 1 300); do
	[ -S "$b/run/control.sock" ] && break
	kill -0 "$stack_b" 2>/dev/null || {
		tail -20 "$b/glassd.log" >&2
		exit 1
	}
	sleep 0.1
done

echo "schedules: Dana and Sam; hosts A (with the front) and B, one DynamoDB table"
signin dana "Dana Alvarez"
signin sam "Sam Lee"
# Schedules of theirs that earlier runs left in the table.
for who in dana sam; do
	as "$who" GET schedules >/dev/null
	for id in $(json '" ".join(s["id"] for s in d)'); do as "$who" DELETE "schedules/$id" >/dev/null; done
done

expect "Dana adds an agent" 200 "$(as dana POST agents '{"template":"desktop-agent","name":"Reconciler"}')"
body=$(python3 -c 'import json,sys; print(json.dumps({"text": sys.argv[1]}))' "$(run 'echo dana-ledger > /root/ledger.txt')")
as dana POST agents/reconciler/ask "$body" >/dev/null
for _ in $(seq 1 100); do
	as dana GET agents/reconciler >/dev/null
	[ "$(json 'd["agent"]["status"]')" = idle ] && break
	sleep 0.3
done

# --- refused ------------------------------------------------------------
read -r hm1 at1 < <(slot UTC 0)
read -r hm2 at2 < <(slot Europe/London 2)
expect "a schedule for the next minute" 200 "$(as dana POST agents/reconciler/schedules "$(schedule "daily $hm1" UTC 'cat /root/ledger.txt; echo first-run')")"
first=$(json 'd["id"]')
expect "one in London's time, two minutes later" 200 "$(as dana POST agents/reconciler/schedules "$(schedule "daily $hm2" Europe/London 'cat /root/ledger.txt; echo second-run')")"
second=$(json 'd["id"]')
expect "  its next run is that minute" "$at2" "$(json 'int(__import__("datetime").datetime.fromisoformat(d["next_run"].replace("Z","+00:00")).timestamp())')"
expect "one she'll turn off" 200 "$(as dana POST agents/reconciler/schedules "$(schedule "daily $hm1" UTC 'echo should-not-run')")"
off=$(json 'd["id"]')
expect "  turned off" 200 "$(as dana POST "schedules/$off/off")"
expect "a schedule it can't read" 400 "$(as dana POST agents/reconciler/schedules "$(schedule 'at nine-ish' UTC 'true')")"
expect "an hour that isn't one" 400 "$(as dana POST agents/reconciler/schedules "$(schedule 'daily 25:00' UTC 'true')")"
expect "a time zone that isn't one" 400 "$(as dana POST agents/reconciler/schedules "$(schedule 'daily 09:00' Mars/Olympus 'true')")"
expect "a path for a time zone" 400 "$(as dana POST agents/reconciler/schedules "$(schedule 'daily 09:00' ../../../etc/passwd 'true')")"
expect "no directions" 400 "$(as dana POST agents/reconciler/schedules '{"prompt":"  ","when":"hourly"}')"
expect "a field it doesn't take" 400 "$(as dana POST agents/reconciler/schedules '{"prompt":"x","when":"hourly","run_as":"root"}')"
expect "an agent she doesn't have" 404 "$(as dana POST agents/closer/schedules "$(schedule hourly UTC 'true')")"
expect "an agent id that's a path" 404 "$(as dana POST 'agents/..%2Fsam/schedules' "$(schedule hourly UTC 'true')")"
as sam GET schedules >/dev/null
expect "Sam sees none of Dana's" 0 "$(json 'len(d)')"
expect "Sam can't turn hers off" 404 "$(as sam POST "schedules/$first/off")"
expect "Sam can't turn hers on" 404 "$(as sam POST "schedules/$off/on")"
expect "Sam can't remove hers" 404 "$(as sam DELETE "schedules/$first")"
expect "Sam can't direct her agent" 404 "$(as sam POST agents/reconciler/schedules "$(schedule hourly UTC 'true')")"
# Up to twenty on: three are, one off; seventeen more fill it.
many=()
for _ in $(seq 1 18); do
	as dana POST agents/reconciler/schedules "$(schedule 'daily 03:00' UTC 'true')" >/dev/null
	many+=("$(json 'd["id"]')")
done
expect "a 21st on" 409 "$(as dana POST agents/reconciler/schedules "$(schedule 'daily 03:00' UTC 'true')")"
expect "nor the one she turned off, back on" 409 "$(as dana POST "schedules/$off/on")"
for id in "${many[@]}"; do as dana DELETE "schedules/$id" >/dev/null; done

# --- the first: both hosts look, one runs it --------------------------------
# Asleep, either host may wake it: which host has a computer awake is the
# lease's to say (ADR 0066), not yet built.
expect "her computer goes to sleep in S3" 200 "$(as dana POST agents/reconciler/sleep)"
echo "  ... waiting for $hm1 UTC"
expect "the first ran" "done" "$(wait_outcome dana "$first" $((at1 + 120)))"
expect "  once, on one host of the two" 1 "$(($(started "$a") + $(started "$b")))"

# --- host A goes; B runs the second, waking her computer from S3 ---------
curl -fsS -X POST "$GLASS_LOCAL_METERING/_fake/lifecycle?host=host-a&state=Terminated" >/dev/null
for _ in $(seq 1 60); do
	grep -q "This host is being replaced" "$a/state/audit/audit.log" 2>/dev/null && break
	sleep 0.5
done
for _ in $(seq 1 60); do
	as dana GET agents/reconciler >/dev/null
	[ "$(json 'd["agent"]["computer"]["state"]')" = asleep ] && break
	sleep 1
done
expect "host A, being replaced, has her computer asleep" asleep "$(json 'd["agent"]["computer"]["state"]')"
before_a=$(started "$a")
echo "  ... waiting for $hm2 in London"
expect "the second ran" "done" "$(wait_outcome dana "$second" $((at2 + 120)))"
expect "  on host B, which never had her computer" "0 1" "$(($(started "$a") - before_a)) $(started "$b")"
screen_b=$(find "$b/state-computers" -name screen.log -path '*/computer/screen.log' 2>/dev/null | head -1)
if [ -n "$screen_b" ] && grep -q dana-ledger "$screen_b" && grep -q second-run "$screen_b"; then
	pass "  her file from host A was on the computer B woke"
else
	fail "  the run on B didn't find her file: $(tail -5 "${screen_b:-/dev/null}")"
fi
if grep -q "Acting for Dana Alvarez (their schedule)" "$b/state/audit/audit.log"; then
	pass "  B acted for her because of her schedule, on the record"
else
	fail "  B's record doesn't say whom it acted for"
fi
expect "the one she turned off didn't run" none "$(last_outcome dana "$off")"
if find "$a/state-computers" "$b/state-computers" -name screen.log -exec grep -l should-not-run {} + 2>/dev/null | grep -q .; then
	fail "a schedule that was off ran"
fi
if grep -q "Scheduled reconciler: every day at" "$a/state/audit/audit.log" && grep -q "Turned off the schedule for reconciler" "$a/state/audit/audit.log"; then
	pass "making and turning off schedules is on the record"
else
	fail "schedules aren't on A's record"
fi

for id in "$first" "$second" "$off"; do as dana DELETE "schedules/$id" >/dev/null; done
curl -fsS -X POST "$GLASS_LOCAL_METERING/_fake/lifecycle?host=host-a&state=InService" >/dev/null
stop
trap - EXIT
rm -rf "$work"
if [ "$failed" = 0 ]; then
	for who in dana sam; do
		curl -fsS -X DELETE "$GLASS_LOCAL_METERING/_fake/s3?prefix=computers/$(owner_of "$(sub_of "$who@example.com")")" >/dev/null || true
	done
	rm -rf "$a" "$b" "$repo/build/sa-computers" "$repo/build/sb-computers"
	echo "schedules: passed"
else
	echo "schedules: FAILED; logs in $a and $b" >&2
fi
exit "$failed"
