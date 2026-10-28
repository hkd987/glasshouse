#!/usr/bin/env bash
# What should fail does, and many people at once don't meet (ADR 0067,
# 0068, 0070): make aws-local first; make aws-local-test runs it.
#
# One agent host, as the stack runs one, behind the stand-in load balancer.
# IT has turned Mail off for the org and back on for Finance (Dana). Then,
# each check printed:
#   - eight people at once: each signs in, adds an agent and runs work on
#     their own computer at the same moment. Each sees only their own
#     agent, and each one's file only on their own screen.
#   - Mail off: Sam's agent is offered no Mail tools, so nothing of his
#     reaches Mail and nothing is asked. Dana's (Finance has it on) asks
#     her first, as Mail's sends do.
#   - another person's approval can't be answered or seen; one answer
#     only; a deny sends nothing.
#   - another's events don't reach them.
#   - bad input: no text, too much text, not JSON, the wrong JSON, an
#     unknown template, a method a route doesn't take, a 21st agent,
#     names that aren't names.
#   - the keyboard: typing without it, asking while he has it (refused,
#     saying why), asking once it's handed back.
# Needs root: it makes the front's account, as the stack does.
set -Eeuo pipefail
trap 'echo "negative: stopped at line $LINENO: $BASH_COMMAND" >&2' ERR

work=$(mktemp -d)
failed=0
stack=""
# shellcheck source=tests/local-aws/lib.sh
. "$(dirname "$0")/lib.sh"
host="$repo/build/ln"
trap 'stop_host; rm -rf "$work"' EXIT

cat >"$work/finance.toml" <<'EOF'
# Finance keeps Mail (ADR 0070).
[[group]]
name = "finance"
members = ["dana@example.com"]
apps_on = ["mail"]
EOF
GLASS_DEV_APPS_OFF=mail GLASS_DEV_POLICY_EXTRA=$work/finance.toml start_host "$host"
echo "negative: one agent host; Mail off for the org, on for Finance"

# --- eight people at once ------------------------------------------------
people=(ana ben cai dee eli fay gus hal)
# Waits on these jobs only: a bare wait would wait on the host too.
together() {
	local pids=() p
	for p in "${people[@]}"; do
		"$@" "$p" &
		pids+=($!)
	done
	wait "${pids[@]}"
}
# shellcheck disable=SC2317 # called through together
signin_one() { signin "$1" "${1^}"; }
# shellcheck disable=SC2317
add_one() { as "$1" POST agents '{"template":"desktop-agent","name":"Helper"}' >"$work/made.$1"; }
# shellcheck disable=SC2317
work_one() { ask_and_wait "$1" helper "$(run "echo $1-only > /root/mine.txt; cat /root/mine.txt; ls /root")"; }
together signin_one
together add_one
made=0
for p in "${people[@]}"; do [ "$(cat "$work/made.$p")" = 200 ] && made=$((made + 1)); done
expect "eight people add an agent at once" 8 "$made"
together work_one
crossed=0
for p in "${people[@]}"; do
	text=$(screen "$p" helper)
	grep -q "$p-only" <<<"$text" || {
		fail "$p's own file isn't on their screen: $(tail -2 <<<"$text")"
		crossed=1
	}
	for q in "${people[@]}"; do
		[ "$q" = "$p" ] && continue
		if grep -q "$q-only" <<<"$text"; then
			fail "$q's file showed on $p's screen"
			crossed=1
		fi
	done
	as "$p" GET agents >/dev/null
	[ "$(json 'len(d)')" = 1 ] || {
		fail "$p sees $(json 'len(d)') agents"
		crossed=1
	}
done
[ "$crossed" = 0 ] && pass "each of the eight sees only their own agent, file and screen"
computers=$(find "$host/state-computers" -mindepth 2 -maxdepth 2 -name computer -type d | wc -l)
expect "eight computers, one each" 8 "$computers"

# --- Mail off --------------------------------------------------------------
signin dana "Dana Alvarez"
signin sam "Sam Lee"
as dana POST agents '{"template":"desktop-agent","name":"Closer"}' >/dev/null
as sam POST agents '{"template":"desktop-agent","name":"Closer"}' >/dev/null
send='{"messages":[{"to":["vendor@example.com"],"subject":"Invoice 7","body":"Please pay."}]}'
ask_and_wait sam closer "$(use mail_send "$send")"
# Off, Mail's tools aren't offered: his agent has none to call. (One that
# calls it anyway is refused with why: the runtime's adr0070 tests.)
if grep '"user":"sam@example.com"' "$host/state/audit/audit.log" | grep -q '"action":"mail\.'; then
	fail "Sam's agent reached Mail: $(grep '"user":"sam@example.com"' "$host/state/audit/audit.log" | grep '"action":"mail\.' | head -1 | cut -c1-200)"
else
	pass "Sam's agent has no Mail to use: no mail action of his at all"
fi
as sam GET approvals >/dev/null
expect "  nothing asked of Sam" 0 "$(json 'len(d)')"
ask_and_wait dana closer "$(use mail_send "$send")"
as dana GET approvals >/dev/null
expect "Dana's agent (Finance keeps Mail) asks her before sending" 1 "$(json 'len(d)')"
approval=$(json 'd[0]["approval"]["id"]')

# --- another's approval ----------------------------------------------------
expect "Sam can't answer Dana's approval" 404 "$(as sam POST "approvals/$approval" '{"yes":true}')"
as sam GET approvals >/dev/null
expect "  nor see it" 0 "$(json 'len(d)')"
expect "Dana says no" 200 "$(as dana POST "approvals/$approval" '{"yes":false}')"
got=$(as dana POST "approvals/$approval" '{"yes":true}')
if [ "$got" = 409 ] || [ "$got" = 404 ]; then pass "  an approval is answered once ($got)"; else fail "  a second answer gave $got"; fi
if grep -q '"mail.send".*"completed"' "$host/state/audit/audit.log" 2>/dev/null; then fail "a mail went out"; else pass "  no mail went out"; fi

# --- another's events ------------------------------------------------------
as sam GET 'events?since=0' >/dev/null
if json 'd["agents"]' | grep -q helper; then fail "Sam heard of an agent he doesn't have"; else pass "Sam's events are his own"; fi

# --- bad input -------------------------------------------------------------
expect "an ask with nothing in it" 400 "$(as sam POST agents/closer/ask '{"text":"   "}')"
big=$(python3 -c 'import json; print(json.dumps({"text": "x" * 40000}))')
expect "an ask longer than a task takes" 400 "$(as sam POST agents/closer/ask "$big")"
expect "not JSON" 400 "$(as sam POST agents/closer/ask '{"text":')"
expect "JSON of the wrong shape" 400 "$(as sam POST agents/closer/ask '{"words":"hi"}')"
expect "not sent as JSON" 415 "$(curl -s -o "$work/body" -w '%{http_code}' -b "$work/sam.jar" -H 'content-type: text/plain' -d 'hi' "$lb/api/v1/agents/closer/ask")"
expect "a template IT didn't make available" 404 "$(as sam POST agents '{"template":"not-a-template"}')"
expect "a method the route doesn't take" 405 "$(as sam PUT agents/closer '{}')"
expect "an agent that isn't there" 404 "$(as sam GET agents/nobody-here)"
expect "an id that's too long" 404 "$(as sam GET "agents/$(printf 'a%.0s' $(seq 1 300))")"
expect "a name of control characters" 200 "$(as sam POST agents '{"template":"desktop-agent","name":"\u0007\u001b[31mRed"}')"
named=$(json 'd["id"]')
if [[ "$named" =~ ^[a-z0-9-]+$ ]]; then pass "  its id is letters, digits and dashes ($named)"; else fail "  its id is $named"; fi
for i in $(seq 3 20); do as sam POST agents "{\"template\":\"desktop-agent\",\"name\":\"Helper $i\"}" >/dev/null; done
as sam GET agents >/dev/null
expect "twenty agents fill a computer" 20 "$(json 'len(d)')"
expect "a 21st is refused" 409 "$(as sam POST agents '{"template":"desktop-agent","name":"One too many"}')"

# --- the keyboard ------------------------------------------------------------
expect "typing on a computer nobody took over" 409 "$(as sam POST agents/closer/input '{"line":"ls"}')"
expect "Sam takes his computer's keyboard" 200 "$(as sam POST agents/closer/take-over)"
expect "  typing works then" 200 "$(as sam POST agents/closer/input '{"line":"echo typed-by-sam"}')"
expect "  Dana can't type on it" 404 "$(as dana POST agents/helper-3/input '{"line":"ls"}')"
expect "  an ask while he has it is refused" 409 "$(as sam POST agents/closer/ask "$(run 'echo asked-while-taken')")"
if json 'd["error"]' | grep -q keyboard; then pass "    saying he has the keyboard"; else fail "    it said $(json 'd')"; fi
expect "  he hands it back" 200 "$(as sam POST agents/closer/hand-back)"
ask_and_wait sam closer "$(run 'echo asked-after')"
# Read whole before searching: grep -q stopping early would break the pipe.
after=$(screen sam closer)
if grep -q asked-after <<<"$after"; then pass "  and the next ask runs"; else fail "  the next ask didn't run: $(tail -3 <<<"$after")"; fi

stop_host
trap - EXIT
rm -rf "$work"
if [ "$failed" = 0 ]; then
	rm -rf "$host" "$repo/build/ln-computers"
	echo "negative: passed"
else
	echo "negative: FAILED; logs in $host" >&2
fi
exit "$failed"
