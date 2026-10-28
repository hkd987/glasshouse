#!/usr/bin/env bash
# Many people on one agent host (ADR 0067, ADR 0068), and everything that
# should fail does: make aws-local first; make aws-local-test runs it.
#
# glassd and the front run as an agent host does: the front as its own
# account (glass-agents), behind the stand-in load balancer, acting for
# each request's signed-in person, proven to glassd by the load balancer's
# ES256 claims or a `glass` token. Dana and Sam sign in through the
# stand-in's sign-in page. Then, each check printed:
#   - each sees, asks and works on only their own agents and computer;
#     every route on the other's agent is absent (404), not forbidden;
#   - forged, expired, foreign and unsigned claims, made-up tokens and no
#     proof at all get 401, from the front directly too;
#   - glass login through the browser flow, its code once only, tokens
#     per person, logout revoking them;
#   - glassd itself refuses `session.as` from any account but the front's,
#     and the front's account anything without a session; both on the
#     record;
#   - two people at once, and two of one person's agents at once;
#   - the front won't serve one identity off this machine's loopback.
# Needs root: it makes the front's account, as the stack does.
set -Eeuo pipefail
trap 'echo "people: stopped at line $LINENO: $BASH_COMMAND" >&2' ERR

repo=$(cd "$(dirname "$0")/../.." && pwd)
env="$repo/build/local-aws/env"
[ -s "$env" ] || {
	echo "people: needs AWS on this machine: make aws-local" >&2
	exit 1
}
# shellcheck source=/dev/null
. "$env"
bin="${CARGO_TARGET_DIR:-$repo/target}/debug"
lb=$GLASS_LOCAL_LOAD_BALANCER
front=http://127.0.0.1:7500
# Short: a computer's task socket must fit in a Unix socket's 108 bytes.
host="$repo/build/lp"
work=$(mktemp -d)
failed=0
stack=""

pass() { printf '  ok    %s\n' "$*"; }
fail() {
	printf '  FAIL  %s\n' "$*"
	failed=1
}
expect() { # expect <what> <want> <got>
	if [ "$2" = "$3" ]; then pass "$1"; else fail "$1: wanted $2, got $3"; fi
}

rm -rf "$host"
mkdir -p "$host"
GLASS_DEV_DIR=$host GLASS_DEV_RUN=$host/run GLASS_DEV_SKIP_BUILD=1 \
	GLASS_AGENTS_IDENTITY=load-balancer GLASS_AGENTS_LISTEN=127.0.0.1:7500 \
	setsid "$repo/tools/dev/agents-run.sh" >"$host/stack.log" 2>&1 &
stack=$!
trap 'kill -TERM -- "-$stack" 2>/dev/null || true; rm -rf "$work"' EXIT
for _ in $(seq 1 300); do
	curl -s -o /dev/null "$front/" && break
	kill -0 "$stack" 2>/dev/null || {
		tail -20 "$host/stack.log" "$host/glassd.log" >&2
		exit 1
	}
	sleep 0.1
done

# As a person through the load balancer: their sign-in's cookie.
signin() { curl -s -o /dev/null -c "$work/$1.jar" -d "email=$1@example.com&name=$2&next=/" "$lb/oauth2/sign-in"; }
# <person> <method> <path> [json]: status, body in $work/body
as() {
	local data=()
	[ -n "${4:-}" ] && data=(-H 'content-type: application/json' -d "$4")
	curl -s -o "$work/body" -w '%{http_code}' -b "$work/$1.jar" -X "$2" "${data[@]}" "$lb/api/v1/$3"
}
# Straight to the front, past the load balancer: <path> [header]
direct() {
	local header=()
	[ -n "${2:-}" ] && header=(-H "$2")
	curl -s -o "$work/body" -w '%{http_code}' "${header[@]}" "$front/api/v1/$1"
}
ids() { python3 -c 'import json,sys; print(" ".join(sorted(a["id"] for a in json.load(open(sys.argv[1])))))' "$work/body"; }
screen() { as "$1" GET "agents/$2/screen?since=0" >/dev/null && python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["text"])' "$work/body"; }
ask_and_wait() { # <person> <agent> <command>: waits for its task to end
	local body
	body=$(python3 -c 'import json,sys; print(json.dumps({"text": "computer_run " + json.dumps({"command": sys.argv[1]})}))' "$3")
	as "$1" POST "agents/$2/ask" "$body" >/dev/null
	for _ in $(seq 1 100); do
		as "$1" GET "agents/$2" >/dev/null
		python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1]))["agent"]["status"] in ("idle","asleep") else 1)' "$work/body" && return 0
		sleep 0.3
	done
}
# A raw call to glassd's control socket: <as-account> <method> <params>
rpc() {
	local who=()
	case "$1" in
	root) ;;
	nobody) who=(setpriv --reuid=65534 --regid=65534 --clear-groups) ;;
	*) who=(setpriv --reuid="$1" --regid="$1" --init-groups) ;;
	esac
	"${who[@]}" python3 -c '
import json, socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])
s.sendall((json.dumps({"jsonrpc": "2.0", "id": 1, "method": sys.argv[2], "params": json.loads(sys.argv[3])}) + "\n").encode())
print(s.makefile().readline())' "$host/run/control.sock" "$2" "$3"
}

echo "people: Dana and Sam on one agent host, behind the load balancer at $lb"
signin dana "Dana Alvarez"
signin sam "Sam Lee"

# --- each their own -----------------------------------------------------
expect "Dana adds an agent" 200 "$(as dana POST agents '{"template":"desktop-agent","name":"Reconciler"}')"
expect "Sam adds an agent by the same name" 200 "$(as sam POST agents '{"template":"desktop-agent","name":"Reconciler"}')"
expect "Dana adds a second agent" 200 "$(as dana POST agents '{"template":"desktop-agent","name":"Closer"}')"
as dana GET agents >/dev/null
expect "Dana sees her two agents only" "closer reconciler" "$(ids)"
as sam GET agents >/dev/null
expect "Sam sees his one only" "reconciler" "$(ids)"
ask_and_wait dana reconciler "echo dana-secret > /root/ledger.txt"
ask_and_wait sam reconciler "cat /root/ledger.txt 2>&1; ls /root"
if screen sam reconciler | grep -q "No such file"; then
	pass "Sam's Reconciler works on Sam's computer: Dana's file isn't there"
else
	fail "Sam's agent saw something of Dana's: $(screen sam reconciler | tail -3)"
fi
if screen sam reconciler | grep -q dana-secret; then fail "Dana's file showed on Sam's screen"; fi
if screen dana reconciler | grep -q "No such file"; then fail "Sam's task showed on Dana's screen"; else pass "and Sam's task isn't on Dana's screen"; fi

# Every route on an agent Sam doesn't have: absent, as if it didn't exist.
gone=0
for route in "GET agents/closer" "GET agents/closer/screen" "POST agents/closer/ask" "POST agents/closer/take-over" \
	"POST agents/closer/hand-back" "POST agents/closer/sleep" "POST agents/closer/wake" "POST agents/closer/input" "POST agents/closer/undo"; do
	read -r method path <<<"$route"
	got=$(as sam "$method" "$path" '{"text":"hello","line":"ls"}')
	[ "$got" = 404 ] || {
		fail "Sam: $route on Dana's agent gave $got"
		gone=1
	}
done
if [ "$gone" = 0 ]; then pass "Sam: all 9 routes on Dana's agent are absent (404)"; fi
expect "an id that's a path" 404 "$(as sam GET 'agents/..%2F..%2Fetc%2Fpasswd')"
expect "a change from another site's page" 403 "$(curl -s -o /dev/null -w '%{http_code}' -b "$work/dana.jar" -H 'Origin: https://evil.example' -H 'content-type: application/json' -d '{"text":"hi"}' "$lb/api/v1/agents/reconciler/ask")"

# --- proofs that aren't -------------------------------------------------
claims() { curl -s "$GLASS_LOCAL_METERING/_fake/assertion?email=dana@example.com&$1"; }
expect "no proof, straight to the front" 401 "$(direct agents)"
expect "claims signed by a key AWS never published" 401 "$(direct agents "x-amzn-oidc-data: $(claims stranger=1)")"
expect "claims that expired ten minutes ago" 401 "$(direct agents "x-amzn-oidc-data: $(claims exp_in=-600)")"
expect "claims from another load balancer" 401 "$(direct agents "x-amzn-oidc-data: $(claims signer=arn:aws:elasticloadbalancing:us-east-1:999999999999:loadbalancer/app/other/1)")"
unsigned=$(claims "" | cut -d. -f1-2).
expect "claims with the signature cut off" 401 "$(direct agents "x-amzn-oidc-data: $unsigned")"
none=$(python3 -c 'import base64,json; b=lambda d: base64.urlsafe_b64encode(json.dumps(d).encode()).decode(); print(b({"alg":"none","kid":"x","signer":"'"$GLASS_LOCAL_SIGNER"'","exp":9999999999})+"."+b({"sub":"sub-dana"})+".")')
expect "claims with alg none" 401 "$(direct agents "x-amzn-oidc-data: $none")"
expect "garbage for claims" 401 "$(direct agents 'x-amzn-oidc-data: not-a-jwt')"
expect "a made-up token" 401 "$(direct agents 'authorization: Bearer gat_0000')"
expect "a cookie from a development front" 401 "$(direct agents 'cookie: glass_agents=0000')"
expect "the page itself, unsigned" 401 "$(curl -s -o /dev/null -w '%{http_code}' "$front/")"
expect "good claims sent straight to the front still work (the network is the security group's job)" 200 "$(direct agents "x-amzn-oidc-data: $(claims exp_in=60)")"

# --- glass login ----------------------------------------------------------
login() { # <person>: runs glass login as the browser would, prints status
	local home="$work/home-$1" page code
	mkdir -p "$home"
	HOME=$home "$bin/glass" login "$lb" 2>"$work/login-$1.err" &
	local glass=$!
	for _ in $(seq 1 50); do
		page=$(grep -o 'http://[^ ]*/login/cli?[^ ]*' "$work/login-$1.err" || true)
		[ -n "$page" ] && break
		sleep 0.1
	done
	curl -s -o /dev/null -b "$work/$1.jar" "$page"
	local form=${page#*\?}
	code=$(curl -s -o /dev/null -w '%{redirect_url}' -b "$work/$1.jar" -d "$form" "$lb/login/cli")
	curl -s -o /dev/null "$code"
	wait "$glass"
	echo "${code#*code=}" | cut -d'&' -f1 >"$work/code-$1"
}
glass_as() { HOME="$work/home-$1" "$bin/glass" "${@:2}"; }
for who in dana sam; do
	if login "$who"; then pass "$who: glass login through the browser"; else fail "$who's glass login: $(cat "$work/login-$who.err")"; fi
done
if glass_as dana agents | grep -q Closer && ! glass_as sam agents | grep -q Closer; then
	pass "each person's token reaches their own agents only"
else
	fail "tokens: Dana's $(glass_as dana agents | tr '\n' ' '); Sam's $(glass_as sam agents | tr '\n' ' ')"
fi
expect "a login code used twice" 401 "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $(cat "$work/code-dana")" "$lb/api/v1/tokens")"
expect "a token as a login code" 401 "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["token"])' "$work/home-sam/.config/glasshouse/agents.json")" "$lb/api/v1/tokens")"
dana_token=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["token"])' "$work/home-dana/.config/glasshouse/agents.json")
glass_as dana logout >/dev/null
expect "Dana's token after she signed glass out" 401 "$(direct agents "authorization: Bearer $dana_token")"
if glass_as sam agents >/dev/null; then pass "Sam's token still works after Dana signed out"; else fail "Sam's token stopped working"; fi

# --- glassd's own door ---------------------------------------------------
if rpc root session.as '{"proof":"dev","sub":"sub-dana"}' | grep -q "only the front may say"; then
	pass "glassd: session.as from root, not the front, is refused"
else
	fail "glassd took session.as from root"
fi
if rpc nobody session.as "{\"proof\":\"load_balancer\",\"assertion\":\"$(claims exp_in=60)\"}" | grep -q "only the front may say"; then
	pass "glassd: session.as from another account, even with good claims, is refused"
else
	fail "glassd took session.as from nobody"
fi
if rpc glass-agents task.list null | grep -q "say who you act for first"; then
	pass "glassd: the front's own account gets nothing without a session"
else
	fail "glassd served the front's account without a session"
fi
if rpc glass-agents session.as '{"proof":"dev","sub":"sub-dana"}' | grep -q "development sign-ins are off"; then
	pass "glassd: development sign-ins are off on this host"
else
	fail "glassd took a development sign-in"
fi
refusals=$(grep -c "Refused to act for someone" "$host/state/audit/audit.log" || true)
if [ "$refusals" -ge 4 ]; then pass "every refusal is on the record ($refusals lines)"; else fail "refusals on the record: $refusals"; fi
if grep -q "Acting for Dana Alvarez" "$host/state/audit/audit.log"; then pass "and who the front acted for"; else fail "no session on the record"; fi

# --- at once --------------------------------------------------------------
ask_and_wait dana closer "sleep 1; echo closer-done" &
one=$!
ask_and_wait dana reconciler "sleep 1; echo reconciler-done" &
two=$!
ask_and_wait sam reconciler "sleep 1; echo sams-done" &
three=$!
wait "$one" "$two" "$three"
if screen dana closer | grep -q closer-done && screen dana reconciler | grep -q reconciler-done && screen sam reconciler | grep -q sams-done; then
	pass "two people at once, and two of Dana's agents at once on her computer"
else
	fail "at once: $(screen dana closer | tail -2)"
fi

# --- one identity never leaves the machine ------------------------------
said=$("$bin/glass-agents" --listen 0.0.0.0:7599 --control-socket "$host/run/control.sock" --identity local 2>&1 || true)
if grep -q "loopback address only" <<<"$said"; then
	pass "the front refuses to act as one account off this machine's loopback"
else
	fail "the front served one identity on 0.0.0.0"
fi

kill -TERM -- "-$stack" 2>/dev/null || true
wait "$stack" 2>/dev/null || true
trap - EXIT
rm -rf "$work"
if [ "$failed" = 0 ]; then
	rm -rf "$host" "$host-computers" "$repo/build/lp-computers"
	echo "people: passed"
else
	echo "people: FAILED; logs in $host" >&2
fi
exit "$failed"
