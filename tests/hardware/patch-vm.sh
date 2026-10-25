#!/usr/bin/env bash
# Usage: tests/hardware/patch-vm.sh [good] [panic] [late] [hang] [late-hang] [shutdown-hang] [refuse]
# Kernel patches end to end in the dev VM (ADR 0057, specs/tla/HardwarePatch.tla). With no
# arguments, all of them:
#
# - good: the Wi-Fi fix (os/freebsd/patches/lkpi-wlan-suspend) is set up in a boot environment of
#   its own; the VM restarts onto it; glassd checks it (the kernel and modules are the ones signed,
#   and load) and puts it on probation, the hardware watchdog armed. A clean restart on probation
#   comes back to it; the VM suspends and resumes twice (the fix asks for two wakes; vm.sh
#   sleep-wake says why QEMU's firmware can't sleep it for real); after its minutes
#   of use it's kept, the watchdog disarmed, and the fix's sysctl shows the patched kernel runs. A second kernel
#   patch not built on top of it is refused (HardwarePatchStack.tla). Then it's taken out
#   (`glassctl hardware patch undo`): the next start is the VM's own environment, which removes the
#   patch's.
# - panic: a module that panics as it loads (tests/hardware/patches/panic-test): the VM restarts
#   by itself on the environment it was on; glassd says the patch rolled back and removes it.
# - late: a module that passes the check and panics three minutes later (late-panic), on
#   probation: the VM restarts by itself on the environment before, and the patch rolled back.
# - hang: a module that never finishes loading (hang-test): after a power cut the VM starts on
#   the environment before, and the patch rolled back.
# - late-hang: a module that hangs every processor three minutes after it loads (late-hang), on
#   probation: the hardware watchdog (QEMU's ICH9 TCO, ichwd) restarts the VM by itself, nobody
#   at the power button, on the environment before.
# - shutdown-hang: a module that hangs the shutdown (shutdown-hang), whose next start the clean
#   shutdown already set to it: the watchdog, armed through the shutdown, restarts the VM onto
#   it, and glassd fails it from what the kernel says of that start.
# - refuse: bundles changed after signing, signed by a key the VM doesn't trust, a community
#   patch, and one built for another release are refused, set nothing up, and are on the audit
#   record.
#
# The audit log verifies after each. Needs the current build deployed (`make freebsd vm-deploy`)
# and the bundles built (os/freebsd/scripts/kernel-patch.sh build <patch>). Run the VM with BIOS
# firmware (`VM_FIRMWARE=bios vm.sh up`): emulated without KVM, the framebuffer console makes a
# boot after a panic or a power cut take many minutes (vm.sh's waits give up), where the text
# console takes seconds. What's tested is the same: the boot environments, the bootonce, the
# watchdog.
#
# The VM trusts the bundles' development key (build/kpatch/dev-key), with a five-minute probation,
# for this run only: its glassd.toml is put back however the run ends.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm="$repo/os/freebsd/scripts/vm.sh"
kpatch=$repo/build/kpatch
conf=/usr/local/etc/glasshouse/glassd.toml
work=$(mktemp -d)
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
glass_kpatch() { cargo run -q --manifest-path "$repo/Cargo.toml" -p glass-kpatch -- "$@"; }

pub=$(glass_kpatch public --key "$kpatch/dev-key")
in_vm "cp $conf $conf.before-patch-test &&
	printf '\n[hardware]\nkeys = [\"%s\"]\nprobation_minutes = 5\n' '$pub' >>$conf &&
	service glassd restart >/dev/null &&
	for i in \$(jot 60); do glassctl status >/dev/null 2>&1 && break; sleep 1; done"
untrust() {
	in_vm "if [ -f $conf.before-patch-test ]; then mv $conf.before-patch-test $conf &&
		service glassd restart >/dev/null; fi" || true
	rm -rf "$work"
}
trap untrust EXIT

# The serial console since now (the log grows across the VM's restarts): `since=$(console_at)`,
# then `console_since "$since"`. The kernel's own messages are behind the splash; the loader's,
# and panics, aren't.
console_at() { stat -c %s "$repo/build/vm/console.log"; }
console_since() { tail -c +$(($1 + 1)) "$repo/build/vm/console.log"; }

# Until the VM is back and glassd answers.
back() {
	"$vm" ssh-close
	sleep 15
	"$vm" wait >/dev/null
	for _ in $(seq 60); do in_vm 'glassctl status' >/dev/null 2>&1 && return; sleep 2; done
	fail "glassd didn't come back"
}

# A clean restart, as the person makes one: rc.d tells glassd it's shutting down.
restart() {
	in_vm 'shutdown -r now' >/dev/null 2>&1 || true
	back
}

# Until glassd's start check has run: the trial isn't waiting for it any more.
settled() {
	for _ in $(seq 60); do
		[[ $(status 'd["trial"]["state"]') != staged ]] && return
		sleep 2
	done
}

# The hardware watchdog is armed ($1 = yes) or not (no).
watchdog_is() {
	local now=no
	in_vm 'service watchdogd onestatus' >/dev/null 2>&1 && now=yes
	[[ $now == "$1" ]] || fail "$2: watchdogd running: $now, should be $1"
}

# Until the trial is in state $1, for at most $2 seconds, through restarts the VM makes itself.
until_state() {
	local want=$1 deadline=$((SECONDS + $2)) got=
	while ((SECONDS < deadline)); do
		got=$(status 'd["trial"]["state"]' 2>/dev/null) || { "$vm" ssh-close; got=; }
		[[ $got == "$want" ]] && return
		sleep 10
	done
	fail "not $want after $2 s (last: ${got:-no answer})"
	return 1
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

# The bundle is refused with a message containing $2, and nothing is set up.
refused() {
	local bundle=$1 why=$2 before after out
	before=$(in_vm 'bectl list -H | cut -f1 | sort')
	"$vm" scp-in "$bundle" /tmp/patch.bundle.json >&2
	if out=$(in_vm 'glassctl hardware patch apply /tmp/patch.bundle.json' 2>&1); then
		fail "$(basename "$bundle") was taken"
	elif [[ $out != *"$why"* ]]; then
		fail "$(basename "$bundle") refused, but not for $why: $out"
	else
		log "refused as it should be: ${out##*refused: }"
	fi
	in_vm 'rm -f /tmp/patch.bundle.json'
	after=$(in_vm 'bectl list -H | cut -f1 | sort')
	[[ $before == "$after" ]] || fail "a refused bundle changed the boot environments"
}

# The patch's environment is gone once the VM runs from the one before.
gone() {
	if in_vm 'bectl list -H' | grep -q "^$1	"; then fail "$1 is still there"; fi
}

good() {
	log "good: the Wi-Fi fix, on probation, then kept"
	local env
	env=$(apply "$kpatch/lkpi-wlan-suspend.bundle.json") || return
	restart
	settled
	[[ $(status 'd["running"]') == "$env" ]] || fail "good: not running from $env"
	[[ $(status 'd["trial"]["state"]') == probation ]] ||
		fail "good: not on probation: $(status 'd["trial"]')"
	in_vm 'bectl list -H' | grep -q "^$env	N	" || fail "good: $env shouldn't be permanent yet"
	watchdog_is yes "good: on probation"
	log "good: a clean restart on probation comes back to it"
	restart
	[[ $(status 'd["running"]') == "$env" ]] || fail "good: a clean restart left $env"
	watchdog_is yes "good: on probation, restarted"
	log "good: the VM suspends and resumes, twice"
	local wakes
	for wakes in 1 2; do
		"$vm" sleep-wake >&2 || { fail "good: the VM didn't suspend and resume"; return; }
		for _ in $(seq 30); do
			[[ $(status 'd["trial"].get("woke", 0)') -ge $wakes || $(status 'd["trial"]["state"]') == kept ]] && break
			sleep 2
		done
		[[ $(status 'd["trial"].get("woke", 0)') -ge $wakes || $(status 'd["trial"]["state"]') == kept ]] ||
			fail "good: wake $wakes wasn't counted: $(status 'd["trial"]')"
	done
	until_state kept 600 || return
	in_vm 'bectl list -H' | grep -q "^$env	NR	" || fail "good: $env isn't the one every start uses"
	watchdog_is no "good: kept"
	# The VM has no Intel card, so nothing loads the driver by itself: it loads against the kernel
	# running only if that is the patched one (it needs the fix's lkpi_80211_pm_driver).
	in_vm 'kldload -n if_iwlwifi && kldstat -q -n if_iwlwifi' ||
		fail "good: the patched Wi-Fi driver doesn't load against the kernel running"
	in_vm 'sysctl -n compat.linuxkpi.80211.suspend_type' >/dev/null ||
		fail "good: no compat.linuxkpi.80211.suspend_type: the kernel running isn't the patched one"

	log "good: a second kernel patch, not built on top of it, is refused"
	mkdir -p "$work/stack/modules"
	sed -e 's/^id = .*/id = "kernel-test"/' -e 's/^kernel = .*//' \
		"$repo/os/freebsd/patches/lkpi-wlan-suspend/patch.toml" | sed '/^\[\[module\]\]/,$d' >"$work/stack/patch.toml"
	printf 'kernel = true\n' >>"$work/stack/patch.toml"
	cp "$repo/os/freebsd/patches/lkpi-wlan-suspend/patch.diff" "$work/stack/"
	cp "$kpatch/lkpi-wlan-suspend/modules/kernel" "$work/stack/modules/"
	glass_kpatch bundle "$work/stack" --modules "$work/stack/modules" --key "$kpatch/dev-key" \
		--out "$work/stack.bundle.json" >&2
	refused "$work/stack.bundle.json" "must be built on top of lkpi-wlan-suspend"

	log "good: taken out"
	in_vm 'glassctl hardware patch undo' >&2
	in_vm 'bectl list -H' | grep -q "^default	R	" || fail "good: default isn't the one the next start uses"
	restart
	[[ $(status 'd["running"]') == default ]] || fail "good: not back on default"
	[[ $(status 'd["trial"]["state"]') == undone ]] || fail "good: not taken out: $(status 'd["trial"]')"
	gone "$env"
}

panic() {
	log "panic: a module that panics as it loads"
	local env
	env=$(apply "$kpatch/panic-test.bundle.json") || return
	local since
	since=$(console_at)
	restart
	settled
	[[ $(status 'd["running"]') == default ]] || fail "panic: not back on default"
	[[ $(status 'd["trial"]["state"]') == rolled_back ]] || fail "panic: not rolled back: $(status 'd["trial"]')"
	console_since "$since" | grep -aq "glass_panic_test: panics as it loads" ||
		fail "panic: no panic on the console: did it start on $env at all?"
	gone "$env"
	in_vm 'grep -c "^glass_panic_test" /boot/loader.conf' >/dev/null 2>&1 &&
		fail "panic: default's loader.conf loads it"
	log "panic: rolled back, and $env removed"
}

late() {
	log "late: a module that panics three minutes after it loads, on probation"
	local env
	env=$(apply "$kpatch/late-panic.bundle.json") || return
	local since
	since=$(console_at)
	restart
	settled
	[[ $(status 'd["trial"]["state"]') == probation ]] ||
		fail "late: not on probation before it panicked: $(status 'd["trial"]')"
	log "late: on probation; waiting for it to panic"
	until_state rolled_back 600 || return
	console_since "$since" | grep -aq "glass_late_panic: panics 180 seconds after it loads" ||
		fail "late: no panic on the console"
	[[ $(status 'd["running"]') == default ]] || fail "late: not back on default"
	gone "$env"
	log "late: $(status 'd["trial"]["note"]')"
}

hang() {
	log "hang: a module that never finishes loading"
	local env
	env=$(apply "$kpatch/hang-test.bundle.json") || return
	local since loaded=
	since=$(console_at)
	in_vm 'shutdown -r now' >/dev/null 2>&1 || true
	"$vm" ssh-close
	for _ in $(seq 120); do
		if console_since "$since" | grep -aq "glass_hang_test.ko size"; then
			loaded=1
			break
		fi
		sleep 2
	done
	[[ -n $loaded ]] || fail "hang: the loader never loaded the module"
	# Behind the splash, a start that hangs is one SSH never answers.
	if "$vm" wait 240 >/dev/null 2>&1; then
		fail "hang: the VM started anyway"
	fi
	log "hang: the start hangs; a power cut"
	"$vm" power-cut >&2
	back
	settled
	[[ $(status 'd["running"]') == default ]] || fail "hang: not back on default"
	[[ $(status 'd["trial"]["state"]') == rolled_back ]] || fail "hang: not rolled back: $(status 'd["trial"]')"
	gone "$env"
}

late_hang() {
	log "late-hang: a module that hangs every processor three minutes after it loads, on probation"
	local env since
	env=$(apply "$kpatch/late-hang.bundle.json") || return
	since=$(console_at)
	restart
	settled
	[[ $(status 'd["trial"]["state"]') == probation ]] ||
		fail "late-hang: not on probation before it hung: $(status 'd["trial"]')"
	watchdog_is yes "late-hang: on probation"
	log "late-hang: on probation; waiting for it to hang, and the watchdog to restart the VM"
	until_state rolled_back 900 || return
	console_since "$since" | grep -aq "glass_late_hang: hanging every processor now" ||
		fail "late-hang: it never hung"
	[[ $(status 'd["running"]') == default ]] || fail "late-hang: not back on default"
	gone "$env"
	log "late-hang: $(status 'd["trial"]["note"]')"
}

shutdown_hang() {
	log "shutdown-hang: a module that hangs the shutdown, its next start already set to it"
	local env since
	env=$(apply "$kpatch/shutdown-hang.bundle.json") || return
	restart
	settled
	[[ $(status 'd["trial"]["state"]') == probation ]] ||
		fail "shutdown-hang: not on probation: $(status 'd["trial"]')"
	since=$(console_at)
	log "shutdown-hang: a clean restart, which it hangs; the watchdog restarts the VM"
	in_vm 'shutdown -r now' >/dev/null 2>&1 || true
	"$vm" ssh-close
	sleep 30
	"$vm" wait 900 >/dev/null || { fail "shutdown-hang: the VM never came back"; return; }
	for _ in $(seq 60); do in_vm 'glassctl status' >/dev/null 2>&1 && break; sleep 2; done
	console_since "$since" | grep -aq "glass_shutdown_hang: hanging the shutdown" ||
		fail "shutdown-hang: the shutdown never hung"
	[[ $(status 'd["running"]') == "$env" ]] || fail "shutdown-hang: the shutdown had set the next start to $env"
	in_vm 'dmesg | grep -a "resuming after hardware watchdog timeout"' >/dev/null ||
		fail "shutdown-hang: the kernel doesn't say the watchdog restarted it"
	[[ $(status 'd["trial"]["state"]') == failed ]] ||
		fail "shutdown-hang: not failed after the watchdog's restart: $(status 'd["trial"]')"
	log "shutdown-hang: $(status 'd["trial"]["note"]')"
	# Its shutdown hangs again: a power cut back to the environment before.
	"$vm" power-cut >&2
	back
	settled
	[[ $(status 'd["running"]') == default ]] || fail "shutdown-hang: not back on default"
	gone "$env"
}

refuse() {
	log "refuse: what the VM doesn't take"
	local good=$kpatch/panic-test.bundle.json src=$repo/tests/hardware/patches/panic-test
	python3 - "$good" "$work/tampered.bundle.json" <<'PY'
import base64, json, sys
b = json.load(open(sys.argv[1]))
name = next(iter(b["modules"]))
module = bytearray(base64.b64decode(b["modules"][name]))
module[-1] ^= 1
b["modules"][name] = base64.b64encode(bytes(module)).decode()
json.dump(b, open(sys.argv[2], "w"))
PY
	refused "$work/tampered.bundle.json" "doesn't match the hash it was signed with"

	glass_kpatch keygen --out "$work/other-key" >/dev/null
	glass_kpatch bundle "$src" --modules "$kpatch/panic-test/modules" --key "$work/other-key" \
		--out "$work/other.bundle.json" >&2
	refused "$work/other.bundle.json" "refused"

	mkdir -p "$work/community" "$work/release"
	cp "$src/patch.diff" "$work/community/" && cp "$src/patch.diff" "$work/release/"
	sed 's/^review = .*/review = "community"/' "$src/patch.toml" >"$work/community/patch.toml"
	sed 's/^release = .*/release = "15.0-RELEASE"/' "$src/patch.toml" >"$work/release/patch.toml"
	for kind in community release; do
		glass_kpatch bundle "$work/$kind" --modules "$kpatch/panic-test/modules" \
			--key "$kpatch/dev-key" --out "$work/$kind.bundle.json" >&2
	done
	refused "$work/community.bundle.json" "community patch"
	refused "$work/release.bundle.json" "built for 15.0-RELEASE"

	local said
	said=$(in_vm 'grep -c "Kernel patch .* refused" /var/db/glasshouse/audit/audit.log' || true)
	((said >= 4)) || fail "refuse: $said refusals on the audit record, not 4"
}

wanted=("$@")
((${#wanted[@]})) || wanted=(refuse good panic late hang late-hang shutdown-hang)
for scenario in "${wanted[@]}"; do
	"${scenario//-/_}"
	in_vm 'glassctl verify' || fail "$scenario: the audit log doesn't verify"
done
in_vm 'glassctl hardware patch status'
if ((failures)); then
	log "kernel patches: $failures failures"
	exit 1
fi
log "kernel patches: passed"
