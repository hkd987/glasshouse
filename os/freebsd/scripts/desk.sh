#!/usr/bin/env bash
# Usage: desk.sh <command> [args]
# Drives the sign-in screen and desktop on the dev VM's screen, each step waiting for the screen to
# have drawn the result, so a test is a few short commands instead of sleep loops (CLAUDE.md:
# script what you repeat). Prints one line per step; logs stay in the VM.
#
#   desk.sh ready [timeout]         wait until the sign-in screen or desktop has drawn
#   desk.sh signin [password]       sign in (password demo); prints "desktop up in ..."
#   desk.sh signout                 Shift Super Q, wait for the sign-in screen
#   desk.sh restart                 restart the sign-in service on its binaries, wait for it
#   desk.sh click <x> <y>           click, and wait for the screen to redraw (if it does)
#   desk.sh key <keys>              press keys (vm.sh key names: ret, ctrl-spc, alt-meta_l-esc),
#                                   and wait for the redraw
#   desk.sh point <x> <y>           move the pointer there, and wait for any redraw (hover)
#   desk.sh wait <regex> [timeout]  wait for a new line in the greeter's log matching <regex>
#   desk.sh shot <png>              save the screen (vm.sh screendump)
#   desk.sh log [n]                 the greeter's log, without draw lines (default 20)
#
# The greeter's log (/var/log/glass-greeter.log) carries the desktop's too. Timeouts are seconds.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

vm=$FREEBSD_SCRIPTS_DIR/vm.sh
log=/var/log/glass-greeter.log
cmd=${1:-}
(($# > 0)) && shift
vm_running || die "VM is not running (vm.sh up && vm.sh wait)"

lines() { vm_ssh "wc -l <$log 2>/dev/null || echo 0" | tr -d ' '; }

# ssh hands the remote shell one string: quote each argument for it.
quote() { printf "'%s' " "${@//\'/\'\\\'\'}"; }

# wait_for <after-line> <regex> <timeout>: a line past <after-line> matching <regex>; prints it.
wait_for() {
	local after=$1 pattern=$2 timeout=$3
	vm_ssh "sh -s -- $(quote "$after" "$pattern" "$timeout" "$log")" <<'EOF'
after=$1 pattern=$2 timeout=$3 log=$4
end=$(($(date +%s) + timeout))
while [ "$(date +%s)" -lt "$end" ]; do
	line=$(tail -n "+$((after + 1))" "$log" 2>/dev/null | grep -E -m1 -- "$pattern") && {
		echo "$line"
		exit 0
	}
	sleep 0.3
done
echo "desk.sh: no '$pattern' in $log after ${timeout}s" >&2
exit 1
EOF
}

case $cmd in
ready)
	vm_ssh "pgrep -qf 'glass-greeter --screen' || pgrep -qx glass-shell-fb" ||
		die "neither the sign-in screen nor the desktop is running (desk.sh restart)"
	# Ready once the screen that opened last (sign-in or desktop) reads its input devices: keys
	# typed before that are lost.
	opened=$(vm_ssh "grep -n '^/dev/ttyv0:' $log | tail -1 | cut -d: -f1")
	wait_for "$((${opened:-1} - 1))" "^input: " "${1:-60}" >/dev/null
	sleep 0.5 # the rest of the devices open in the same moment
	echo "ready"
	;;
signin)
	vm_ssh "pgrep -qf 'glass-greeter --screen'" || die "the sign-in screen isn't showing"
	n=$(lines)
	"$vm" type "${1:-demo}" >/dev/null
	"$vm" key ret >/dev/null
	if ! wait_for "$n" "desktop up|didn't work|signed in but" 60; then
		die "no desktop after signing in (desk.sh log)"
	fi
	wait_for "$n" "tiles: drawn" 30 >/dev/null
	;;
signout)
	n=$(lines)
	"$vm" key shift-meta_l-q >/dev/null
	wait_for "$n" "sign-in screen: drawn" 60 >/dev/null
	echo "signed out"
	;;
restart)
	n=$(lines)
	vm_ssh 'service glasshouse_greeter restart >/dev/null'
	wait_for "$n" "sign-in screen: drawn" 60 >/dev/null
	echo "sign-in screen up"
	;;
click | key | point)
	n=$(lines)
	"$vm" "$cmd" "$@" >/dev/null
	# Whatever it changed is drawn within a few seconds even in the emulated VM (a redraw that
	# copied nothing doesn't count), then the log goes quiet; nothing drawn is an answer too (a
	# click on the wallpaper).
	if wait_for "$n" "drawn in [0-9]+ ms, [1-9]" "${DESK_SETTLE:-12}" 2>/dev/null | sed 's/^/drew: /'; then
		vm_ssh "n=0; while [ \$n -lt 20 ] && [ \"\$(wc -l <$log)\" != \"\$(sleep 0.5; wc -l <$log)\" ]; do n=\$((n + 1)); done"
	else
		echo "nothing redrawn"
	fi
	;;
wait)
	(($# >= 1)) || die "usage: desk.sh wait <regex> [timeout]"
	wait_for "$(lines)" "$1" "${2:-30}"
	;;
shot)
	(($# == 1)) || die "usage: desk.sh shot <png>"
	"$vm" screendump "$1" >/dev/null
	echo "$1"
	;;
log)
	vm_ssh "grep -v 'drawn in' $log | tail -n ${1:-20}"
	;;
*) die "usage: desk.sh ready|signin [password]|signout|restart|click x y|key k|point x y|wait <regex>|shot <png>|log [n]" ;;
esac
