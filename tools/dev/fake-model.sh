#!/usr/bin/env bash
# Usage: tools/dev/fake-model.sh [--it] [--ladder] <scenario> [request]
#
# The desktop agent (or, with --it, the IT agent) against a local stand-in for Anthropic's
# Messages API (tools/dev/fake-anthropic.py): the real Anthropic provider, gateway, HTTP and TLS
# path, for no credits. The stand-in checks requests as the API documents (thinking blocks bound
# to the conversation before them, tool results answering the calls before them, strict fields)
# and can fail on purpose. Scenarios: desktop parallel overload2 ratelimit2 hangup2 refusal2 maxtokens1
# maxtokens credit billing triage (see fake-anthropic.py). --ladder approves Haiku, Sonnet and
# Opus 5.5 with by_cost on (ADR 0044), to watch a task step up.
#
# Runs glassd on this machine (tools/dev/host-run.sh, NOTHING CONTAINED) with a seeded workday,
# prints the task, the model each request went to and how it ended, and keeps every request
# and reply in build/dev-fake-model/requests.jsonl. tools/dev/real-model.sh is the same on a
# real key.
set -euo pipefail
unset CLICOLOR_FORCE
export NO_COLOR=1

repo=$(cd "$(dirname "$0")/../.." && pwd)
agent=desktop
ladder=
die() {
	echo "fake-model: $*" >&2
	exit 1
}
while [[ ${1:-} == --* ]]; do
	case $1 in
	--it) agent=it ;;
	--ladder) ladder=1 ;;
	*) die "unknown option $1" ;;
	esac
	shift
done
scenario=${1:-desktop}
[[ $scenario =~ ^[a-z0-9]+$ ]] || die "'$scenario' isn't a scenario"
if [[ $agent == it ]]; then
	request=${2:-"The printer on floor 2 won't print"}
else
	request=${2:-"What is in my LaunchNotes folder? Read each file and tell me in a sentence what it says."}
fi

out=$repo/build/dev-fake-model
bin=${CARGO_TARGET_DIR:-$repo/target}/debug
# Short, so the sockets' paths stay under 104 bytes.
dev=$(mktemp -d /tmp/glass-fake-model.XXXXXX)
run=$dev/run
mkdir -p "$out" "$run"
rm -f "$out/requests.jsonl"
cargo build -q -p glassd -p glassctl -p glass-desktop-agent -p glass-it-agent -p glass-e2e

# A certificate for 127.0.0.1, trusted by this glassd alone (SSL_CERT_FILE).
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=fake-anthropic" \
	-addext "subjectAltName=IP:127.0.0.1" -keyout "$dev/key.pem" -out "$dev/cert.pem" 2>/dev/null
port=$((20000 + RANDOM % 20000))
python3 -I "$repo/tools/dev/fake-anthropic.py" --port "$port" --cert "$dev/cert.pem" \
	--key "$dev/key.pem" --log "$out/requests.jsonl" --scenario "$scenario" >"$out/fake.log" 2>&1 &
fake=$!

policy=$dev/fake-policy.toml
"$bin/glass-e2e" real-model-policy anthropic claude-haiku-5-5 ${ladder:+--ladder} |
	sed "s|^kind = \"anthropic\"|kind = \"anthropic\"\nendpoint = \"https://127.0.0.1:$port\"|" >"$policy"
# glassd refuses a secrets file anyone else can read.
(umask 077 && printf '[anthropic]\napi_key = "sk-ant-api03-not-a-real-key"\n' >"$dev/model-secrets.toml")

SSL_CERT_FILE=$dev/cert.pem GLASS_DEV_SKIP_BUILD=1 GLASS_DEV_DIR=$dev GLASS_DEV_RUN=$run \
	GLASS_DEV_POLICY=$policy GLASS_DEV_MODEL_SECRETS=$dev/model-secrets.toml \
	"$repo/tools/dev/host-run.sh" >"$out/glassd.log" 2>&1 &
glassd=$!
# shellcheck disable=SC2064
trap "kill $glassd $fake 2>/dev/null; wait 2>/dev/null; rm -rf '$dev'" EXIT
export GLASS_CONTROL_SOCKET=$run/control.sock PATH=$bin:$PATH
for _ in $(seq 300); do
	glassctl status >/dev/null 2>&1 && break
	kill -0 "$glassd" 2>/dev/null || die "glassd stopped; see $out/glassd.log"
	sleep 0.2
done
python3 -I "$repo/tools/dev/seed-workday.py" "$dev/home" "$(id -un)" >/dev/null

short() { sed -n 's/.*(\([0-9a-f]*\))\. Watch.*/\1/p'; }
if [[ $agent == it ]]; then
	task=$(glassctl run it-agent -y "$request" | short)
else
	task=$(glassctl run desktop-agent --read "$dev/home/LaunchNotes" --read "$dev/home/Invoices" -y "$request" | short)
fi
for _ in $(seq 600); do
	shown=$(glassctl show "$task")
	ask=$(printf '%s\n' "$shown" | sed -n 's/.*glassctl approve \([0-9a-f]*\).*/\1/p' | head -1)
	[[ -n $ask ]] && glassctl approve "$ask" >/dev/null
	printf '%s\n' "$shown" | head -1 | grep -qE ' (Done|Failed|Stopped) ' && break
	sleep 0.2
done
glassctl show "$task" | sed -n '1,3p;/^Answer/,/^Activity/p'
echo "-- each request: the model it went to, how the stand-in answered"
python3 -I -c '
import json, sys
for n, line in enumerate(open(sys.argv[1]), 1):
    r = json.loads(line)
    reply, model, status = r["reply"], r["body"]["model"], r["status"]
    how = reply.get("stop_reason") or reply.get("error", {}).get("type")
    if status == 0:
        print(f"  {n}. {model:20} the connection closed with no reply")
        continue
    print(f"  {n}. {model:20} HTTP {status} {how}")
' "$out/requests.jsonl"
