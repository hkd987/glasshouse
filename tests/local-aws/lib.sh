# shellcheck shell=bash
# shellcheck disable=SC2034,SC2154 # set here for the scripts that source it, or by them
# What the local-AWS tests share (make aws-local first): checks printed one
# a line, people signed in at the stand-in load balancer, their API calls,
# and an agent host started as the stack runs one. Source it after setting
# `work` (a scratch folder) and `failed=0`.

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
env="$repo/build/local-aws/env"
[ -s "$env" ] || {
	echo "needs AWS on this machine: make aws-local" >&2
	exit 1
}
# shellcheck source=/dev/null
. "$env"
bin="${CARGO_TARGET_DIR:-$repo/target}/debug"
lb=$GLASS_LOCAL_LOAD_BALANCER
front=http://127.0.0.1:7500

pass() { printf '  ok    %s\n' "$*"; }
fail() {
	printf '  FAIL  %s\n' "$*"
	failed=1
}
expect() { # expect <what> <want> <got>
	if [ "$2" = "$3" ]; then pass "$1"; else fail "$1: wanted $2, got $3 $(head -c 300 "$work/body" 2>/dev/null)"; fi
}
# A Python expression over the last body, as `d`.
json() { python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2], {"d": d, "json": json}))' "$work/body" "$1"; }

# Signs <person> in at the load balancer, as <name>: their cookie.
signin() { curl -s -o /dev/null -c "$work/$1.jar" -d "email=$1@example.com&name=$2&next=/" "$lb/oauth2/sign-in"; }
# <person> <method> <path> [json]: the status; the body in $work/body.
# Each call's body goes to $work/body.<person> too, for calls at once.
as() {
	local data=()
	[ -n "${4:-}" ] && data=(-H 'content-type: application/json' --data-binary "$4")
	local code
	code=$(curl -s -o "$work/body.$1" -w '%{http_code}' -b "$work/$1.jar" -X "$2" "${data[@]}" "$lb/api/v1/$3")
	cp "$work/body.$1" "$work/body" 2>/dev/null || true
	echo "$code"
}
# Straight to the front, past the load balancer: <path> [header]
direct() {
	local header=()
	[ -n "${2:-}" ] && header=(-H "$2")
	curl -s -o "$work/body" -w '%{http_code}' "${header[@]}" "$front/api/v1/$1"
}
# What a scripted agent does: calls <tool> with <json>.
use() { python3 -c 'import json,sys; print(json.dumps({"text": sys.argv[1] + " " + sys.argv[2]}))' "$1" "$2"; }
# ...or runs <command> on its computer.
run() { use computer_run "$(python3 -c 'import json,sys; print(json.dumps({"command": sys.argv[1]}))' "$1")"; }
# <person> <agent>: waits until the agent is idle or asleep again.
settle() {
	for _ in $(seq 1 150); do
		curl -s -o "$work/settle.$1" -b "$work/$1.jar" "$lb/api/v1/agents/$2"
		python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1]))["agent"]["status"] in ("idle","asleep","needs_you") else 1)' "$work/settle.$1" 2>/dev/null && return 0
		sleep 0.3
	done
	return 1
}
# <person> <agent> <text json>: asks and waits.
ask_and_wait() {
	as "$1" POST "agents/$2/ask" "$3" >/dev/null
	sleep 0.3
	settle "$1" "$2"
}
screen() { curl -s -b "$work/$1.jar" "$lb/api/v1/agents/$2/screen?since=0" | python3 -c 'import json,sys; print(json.load(sys.stdin)["text"])'; }

# Starts an agent host in <dir>: glassd and the front behind the load
# balancer, in a process group of its own ($stack). More settings for
# tools/dev/agents-run.sh and host-run.sh come from the environment.
start_host() {
	local dir=$1
	rm -rf "$dir" "$dir-computers" "$dir/state-computers"
	mkdir -p "$dir"
	GLASS_DEV_DIR=$dir GLASS_DEV_RUN=$dir/run GLASS_DEV_SKIP_BUILD=1 \
		GLASS_AGENTS_IDENTITY=load-balancer GLASS_AGENTS_LISTEN=127.0.0.1:7500 \
		setsid "$repo/tools/dev/agents-run.sh" >"$dir/stack.log" 2>&1 &
	stack=$!
	for _ in $(seq 1 300); do
		curl -s -o /dev/null "$front/" && return 0
		kill -0 "$stack" 2>/dev/null || {
			tail -20 "$dir/stack.log" "$dir/glassd.log" >&2
			exit 1
		}
		sleep 0.1
	done
	echo "the host in $dir didn't answer" >&2
	exit 1
}
stop_host() {
	kill -TERM -- "-$stack" 2>/dev/null || true
	wait "$stack" 2>/dev/null || true
}
