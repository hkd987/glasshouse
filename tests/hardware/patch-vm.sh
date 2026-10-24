#!/usr/bin/env bash
# Usage: tests/hardware/patch-vm.sh [good] [panic]
# Kernel patches end to end in the dev VM (ADR 0057). With no arguments, both:
#
# - good: the Wi-Fi fix (os/freebsd/patches/lkpi-wlan-suspend) is set up in a boot environment of
#   its own; the VM restarts onto it; glassd checks it (its modules are the ones signed, and load)
#   and keeps it. The fix's sysctl shows the patched module is the one running. The VM is then
#   started from its own environment again and the patch's is removed.
# - panic: a module that panics as it loads (tests/hardware/patches/panic-test) is set up; the VM
#   restarts onto it, panics, restarts by itself on the environment it was on; glassd says the
#   patch rolled back and removes its environment.
#
# The audit log verifies after each. Needs the current build deployed (`make freebsd vm-deploy`)
# and the bundles built (os/freebsd/scripts/kernel-patch.sh build <patch>).
#
# The VM trusts the bundles' development key (build/kpatch/dev-key) for this run only: its
# glassd.toml is put back however the run ends.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm="$repo/os/freebsd/scripts/vm.sh"
kpatch=$repo/build/kpatch
conf=/usr/local/etc/glasshouse/glassd.toml
failures=0

# On stderr: stdout carries what functions return.
log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
fail() {
	log "FAIL $*"
	failures=$((failures + 1))
}
in_vm() { "$vm" ssh "$@"; }
field() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
status() { in_vm 'glassctl --json hardware patch status' | field "$1"; }

pub=$(cargo run -q --manifest-path "$repo/Cargo.toml" -p glass-kpatch -- public --key "$kpatch/dev-key")
in_vm "cp $conf $conf.before-patch-test && printf '\n[hardware]\nkeys = [\"%s\"]\n' '$pub' >>$conf &&
	service glassd restart >/dev/null"
untrust() {
	in_vm "if [ -f $conf.before-patch-test ]; then mv $conf.before-patch-test $conf &&
		service glassd restart >/dev/null; fi" || true
}
trap untrust EXIT

# A clean restart; back once glassd answers.
restart() {
	in_vm 'shutdown -r now' >/dev/null 2>&1 || true
	"$vm" ssh-close
	sleep 15
	"$vm" wait >/dev/null
	for _ in $(seq 60); do in_vm 'glassctl status' >/dev/null 2>&1 && return; sleep 2; done
	fail "glassd didn't come back after the restart"
}

# Until glassd's start check has run: the trial isn't open any more.
settled() {
	for _ in $(seq 60); do
		[[ $(status 'd["trial"]["state"]') != staged ]] && return
		sleep 2
	done
}

apply() {
	local bundle=$1
	[[ -f $bundle ]] || { fail "no $bundle: os/freebsd/scripts/kernel-patch.sh build <patch>"; return 1; }
	"$vm" scp-in "$bundle" /tmp/patch.bundle.json >&2
	in_vm 'glassctl hardware patch apply /tmp/patch.bundle.json && rm /tmp/patch.bundle.json' >&2
	[[ $(status 'd["trial"]["state"]') == staged ]] || fail "not staged"
	local env
	env=$(status 'd["trial"]["env"]')
	in_vm "bectl list -H" | grep -q "^$env	T	" || fail "$env isn't set to start once"
	echo "$env"
}

good() {
	log "good: the Wi-Fi fix"
	local env
	env=$(apply "$kpatch/lkpi-wlan-suspend.bundle.json") || return
	restart
	settled
	[[ $(status 'd["running"]') == "$env" ]] || fail "good: not running from $env"
	[[ $(status 'd["trial"]["state"]') == kept ]] || fail "good: not kept: $(status 'd["trial"]')"
	in_vm 'kldstat -q -m linuxkpi_wlan && kldstat -q -n if_iwlwifi' || fail "good: its modules aren't loaded"
	in_vm 'sysctl -n compat.linuxkpi.80211.suspend_type' >/dev/null ||
		fail "good: no compat.linuxkpi.80211.suspend_type: the module running isn't the patched one"
	in_vm 'bectl list -H' | grep -q "^$env	NR	" || fail "good: $env isn't the one every start uses"
	log "good: kept, running from $env; back to the VM's own environment"
	in_vm 'bectl activate default' >/dev/null
	restart
	if in_vm 'bectl list -H' | grep -q "^$env	"; then
		in_vm "bectl destroy -o $env" >/dev/null || fail "good: couldn't remove $env"
	fi
}

panic() {
	log "panic: a module that panics as it loads"
	local env
	env=$(apply "$kpatch/panic-test.bundle.json") || return
	restart
	settled
	[[ $(status 'd["running"]') == default ]] || fail "panic: not back on default"
	[[ $(status 'd["trial"]["state"]') == rolled_back ]] || fail "panic: not rolled back: $(status 'd["trial"]')"
	grep -aq "glass_panic_test: panics as it loads" "$repo/build/vm/console.log" ||
		fail "panic: no panic on the console: did it start on $env at all?"
	if in_vm 'bectl list -H' | grep -q "^$env	"; then fail "panic: $env is still there"; fi
	in_vm 'grep -c "^glass_panic_test" /boot/loader.conf' >/dev/null 2>&1 &&
		fail "panic: default's loader.conf loads it"
	log "panic: rolled back, and $env removed"
}

wanted=("$@")
((${#wanted[@]})) || wanted=(good panic)
for scenario in "${wanted[@]}"; do
	"$scenario"
	in_vm 'glassctl verify' || fail "$scenario: the audit log doesn't verify"
done
in_vm 'glassctl hardware patch status'
if ((failures)); then
	log "kernel patches: $failures failures"
	exit 1
fi
log "kernel patches: passed"
