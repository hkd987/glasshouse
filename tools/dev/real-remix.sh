#!/usr/bin/env bash
# Usage: tools/dev/real-remix.sh up
#        tools/dev/real-remix.sh run "<request>"
#        tools/dev/real-remix.sh review <catalog-id> [shots-dir]
#        tools/dev/real-remix.sh down
#
# Hacking on our apps and sharing them back to the org's store, on a real Anthropic model (TL-5,
# GA-8, ADR 0032). `up` starts the admin console (http://127.0.0.1:7410) with a policy that routes
# to Claude Haiku 5.5, steps up to Sonnet 5.5 by cost, and lets the desktop agent remix and share
# apps (apps.*, as IT would widen it, ADR 0041). It also starts a glassd on this machine enrolled
# with it (tools/dev/host-run.sh, NOTHING CONTAINED), with a seeded workday, and the computer's
# app bridge for the Store (http://store.localhost:7411).
# `run` gives the desktop agent a request, approves what it asks, and prints the task and each
# model call's model, tokens and cost. `review` is IT's side in a browser: the review page,
# approve for everyone, the Store installs it, it opens. It saves screenshots
# (build/dev-real-remix/shots by default).
#
# The key comes from GLASS_ANTHROPIC_API_KEY (never ANTHROPIC_API_KEY) into a file only its owner
# can read, and is never printed; `down` deletes it. A remix costs about $0.004 on Haiku.
set -euo pipefail
unset CLICOLOR_FORCE
export NO_COLOR=1

repo=$(cd "$(dirname "$0")/../.." && pwd)
# Short, so the sockets' paths stay under 104 bytes.
dev=/tmp/glass-real-remix
run=$dev/run
out=$repo/build/dev-real-remix
bin=${CARGO_TARGET_DIR:-$repo/target}/debug
export GLASS_CONTROL_SOCKET=$run/control.sock PATH=$bin:$PATH
die() {
	echo "real-remix: $*" >&2
	exit 1
}

down() {
	for pid in "$dev"/*.pid; do
		if [[ -f $pid ]]; then
			kill "$(cat "$pid")" 2>/dev/null || true
			rm -f "$pid"
		fi
	done
	rm -f "$dev/model-secrets.toml"
}

up() {
	[[ -n ${GLASS_ANTHROPIC_API_KEY:-} ]] || die "GLASS_ANTHROPIC_API_KEY isn't set"
	down
	rm -rf "$dev"
	mkdir -p "$dev/console" "$dev/home" "$run" "$out"
	cd "$repo"
	cargo build -q -p glass-console -p glass-app-bridge -p glassd -p glassctl -p glass-desktop-agent -p glass-e2e
	[[ -f apps/console/dist/app.js ]] || make -s apps-build

	local policy=$dev/policy.toml
	glass-e2e real-model-policy anthropic claude-haiku-5-5 >"$policy"
	sed -i -e 's/^by_cost = false$/by_cost = true/' \
		-e 's|^\[models.routes\]$|[models.routes]\npublic = ["real", "sonnet"]\ninternal = ["real", "sonnet"]|' \
		-e '/^id = "desktop-agent"$/{n;s/"model.complete"\]$/"model.complete", "apps.*"]/}' "$policy"
	grep -q '"model.complete", "apps.\*"\]' "$policy" || die "the desktop agent's actions moved; apps.* didn't go in"
	cat >>"$policy" <<'EOF'

[[models.model]]
id = "sonnet"
provider = "anthropic"
model = "claude-sonnet-5-5"
name = "claude-sonnet-5-5"
usd_per_mtok_in = 2.0
usd_per_mtok_out = 10.0
usd_per_mtok_cache_read = 0.2
EOF
	# The console releases its draft as the org's first policy.
	python3 -I -c 'import json, sys; json.dump({"draft": open(sys.argv[1]).read(), "versions": [], "rings": {}}, open(sys.argv[2], "w"))' \
		"$policy" "$dev/console/policy.json"
	glass-console --dev --listen 127.0.0.1:7410 --state "$dev/console" --web "$repo/apps/console/dist" \
		--admin "$(id -un)" >"$out/console.log" 2>&1 &
	echo $! >"$dev/console.pid"
	for _ in $(seq 150); do
		[[ -f $dev/console/enrollment.json ]] && break
		sleep 0.2
	done
	[[ -f $dev/console/enrollment.json ]] || die "the console didn't start; see $out/console.log"

	(umask 077 && printf '[anthropic]\napi_key = "%s"\n' "$GLASS_ANTHROPIC_API_KEY" >"$dev/model-secrets.toml")
	GLASS_DEV_SKIP_BUILD=1 GLASS_DEV_DIR=$dev GLASS_DEV_RUN=$run GLASS_DEV_DEVICE=dev-host \
		GLASS_DEV_ENROLL=$dev/console/enrollment.json GLASS_DEV_MODEL_SECRETS=$dev/model-secrets.toml \
		"$repo/tools/dev/host-run.sh" >"$out/glassd.log" 2>&1 &
	echo $! >"$dev/glassd.pid"
	for _ in $(seq 300); do
		glassctl status 2>/dev/null | grep -q 'managed v' && break
		sleep 0.2
	done
	glassctl status | grep -q 'managed v' || die "glassd isn't on the org's policy; see $out/glassd.log"
	python3 -I "$repo/tools/dev/seed-workday.py" "$dev/home" "$(id -un)" >/dev/null

	glass-app-bridge --dev --listen 127.0.0.1:7411 --apps "$repo/apps" --socket "$run/apps.sock" \
		>"$out/bridge.log" 2>&1 &
	echo $! >"$dev/bridge.pid"
	echo "up: console http://127.0.0.1:7410, Store http://store.localhost:7411"
	echo "    export GLASS_CONTROL_SOCKET=$GLASS_CONTROL_SOCKET"
}

task() {
	local request=$1 task ask
	task=$(glassctl --json run desktop-agent -y "$request" | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["id"])')
	for _ in $(seq 1800); do
		ask=$(glassctl --json approvals | python3 -I -c '
import json, sys
print(next((a["id"] for a in json.load(sys.stdin) if a.get("task") == sys.argv[1]), ""))' "$task")
		if [[ -n $ask ]]; then
			glassctl approvals
			glassctl approve "$ask" >/dev/null
		fi
		glassctl show "$task" | head -1 | grep -qE ' (Done|Failed|Stopped) ' && break
		sleep 0.5
	done
	glassctl show "$task" | sed -n '1,/^Changes/p'
	echo "-- each model call"
	# shellcheck disable=SC2016 # Python's f-strings, not the shell's.
	glassctl --json audit "$task" | python3 -I -c '
import json, sys
def walk(v):
    if isinstance(v, dict):
        yield v
        for x in v.values():
            yield from walk(x)
    elif isinstance(v, list):
        for x in v:
            yield from walk(x)
calls = [d for d in walk(json.load(sys.stdin)) if "cost_usd" in d and "provider_model" in d]
for d in calls:
    model, tin, tout = d["provider_model"], d.get("input_tokens"), d.get("output_tokens")
    usd, tools = d["cost_usd"], ", ".join(d.get("tools_called") or []) or "answer"
    print(f"  {model:20} {tin:>7} in {tout:>6} out  ${usd:.4f}  {tools}")
total = sum(d["cost_usd"] for d in calls)
print(f"  {len(calls)} calls, ${total:.4f}")'
}

case ${1:-} in
up) up ;;
run) task "${2:?what to ask the desktop agent}" ;;
review)
	cd "$repo/apps"
	PLAYWRIGHT_BROWSERS_PATH=${PLAYWRIGHT_BROWSERS_PATH:-/opt/pw-browsers} \
		node "$repo/tools/dev/real-remix-review.mjs" "${2:?the catalog id the share got}" "${3:-$out/shots}" "$dev"
	;;
down) down ;;
*) die "usage: real-remix.sh up | run <request> | review <catalog-id> [shots-dir] | down" ;;
esac
