#!/bin/sh
# BR-1: no base-system name anywhere a user looks. Runs in the VM as root,
# after a deploy. Surfaces that only our own base build can change (kernel
# identity, base tools) print PENDING instead of failing; see ADR 0011.
set -u

fail=0
check() { # <surface> <text a user would see>
	hit=$(printf '%s\n' "$2" | grep -i -m1 freebsd)
	if [ -n "$hit" ]; then
		echo "FAIL $1: $hit"
		fail=1
	else
		echo "PASS $1"
	fi
}
require() { # <surface> <command...>
	surface=$1
	shift
	if "$@" >/dev/null 2>&1; then echo "PASS $surface"; else echo "FAIL $surface" && fail=1; fi
}

check "OS identity (/etc/os-release)" "$(cat /etc/os-release)"
require "OS identity names Glasshouse" grep -qx 'NAME=Glasshouse' /etc/os-release
check "login message (/etc/motd)" "$(cat /etc/motd)"
check "console login banner" "$(sed -n '/^default:/,/^$/p' /etc/gettytab | grep 'im=')"
check "login shell output" "$(su -l demo -c true 2>&1)"
check "hostname" "$(hostname)"
check "SSH version string" "$(sshd -T 2>/dev/null | grep -i '^versionaddendum')"
require "SSH version string has no addendum" sh -c "sshd -T | grep -qix 'versionaddendum none'"
require "boot loader brand" grep -qx 'loader_brand="glasshouse"' /boot/loader.conf
require "boot messages muted" grep -qx 'boot_mute="YES"' /boot/loader.conf
check "glassctl" "$(su demo -c 'glassctl status; glassctl agents; glassctl --help' 2>&1)"
require "legal notices ship (BR-2)" test -s /usr/local/share/glasshouse/legal/NOTICES.txt
require "base-system notice reproduced (BR-2)" \
	grep -q 'The FreeBSD Project' /usr/local/share/glasshouse/legal/NOTICES.txt

echo "PENDING kernel identity: uname -s says $(uname -s) (needs our own base build)"
echo "PENDING base tools: freebsd-version, freebsd-update and pkg keep their names"

exit $fail
