#!/usr/bin/env bash
# Usage: tools/dev/claude-plan.sh ["request"]
#
# The desktop agent on your own Claude Code and Claude plan (ADR 0042), on a development host:
# a glassd of its own (tools/dev/host-run.sh, NOTHING CONTAINED), then
# `glassctl run desktop-agent-plan --drive`, which starts your Claude Code as you, with this
# computer's tools only. Run it in your own terminal on Linux or macOS, signed in to Claude Code
# the usual way (`claude`, then /login). Nothing here reads, keeps or prints your sign-in.
#
# It refuses to run inside a Claude Code session (CLAUDECODE is set there, and a cloud session's
# container has its own sign-in that a nested Claude Code can pick up): the run must be on your
# plan, not the session's.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
request=${1:-"What invoices do I have, and what is their total? Read each one."}

die() {
	echo "claude-plan: $*" >&2
	exit 1
}
[[ -z ${CLAUDECODE:-} && ! -d /root/.ccr ]] ||
	die "run this in your own terminal, not inside a Claude Code session: it must use your sign-in"
command -v claude >/dev/null || die "Claude Code isn't installed: https://code.claude.com/docs/en/setup"

# Which sign-in Claude Code will use: only its method, never the account.
status=$(claude auth status 2>/dev/null || true)
read -r signed method provider < <(printf '%s' "$status" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except ValueError:
    d = {}
print(str(d.get("loggedIn", False)).lower(), d.get("authMethod", "none"), d.get("apiProvider", "?"))')
[[ $signed == true ]] || die "Claude Code isn't signed in: run claude, then /login"
echo "Claude Code signs in with: $method ($provider)"
case $method in
claude.ai | oauth_token) ;;
*) echo "claude-plan: that isn't a Claude plan sign-in; this run bills that account instead" >&2 ;;
esac

dev=$repo/build/dev-claude-plan
run=$(mktemp -d /tmp/glass-claude-plan.XXXXXX)
bin=${CARGO_TARGET_DIR:-$repo/target}/debug
cargo build -q --manifest-path "$repo/Cargo.toml" -p glassctl -p glass-claude-runner -p glass-mcp
GLASS_DEV_DIR=$dev GLASS_DEV_RUN=$run "$repo/tools/dev/host-run.sh" >"$run/glassd.log" 2>&1 &
glassd=$!
# shellcheck disable=SC2064
trap "kill $glassd 2>/dev/null; rm -rf '$run'" EXIT
export GLASS_CONTROL_SOCKET=$run/control.sock
for _ in $(seq 600); do
	"$bin/glassctl" status >/dev/null 2>&1 && break
	kill -0 "$glassd" 2>/dev/null || die "glassd stopped: $(tail -3 "$run/glassd.log")"
	sleep 0.5
done
"$bin/glassctl" run desktop-agent-plan --read "$dev/home/Invoices" -y "$request" --drive
