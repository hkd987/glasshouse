#!/usr/bin/env bash
# Usage: tests/system/verify-vm.sh
# The system's files checked at every start, in the dev VM (ADR 0064, EP-1, specs/tla/Boot.tla):
#
# - the release side lists the VM's system files (`glassctl system manifest`, in the VM, where the
#   files are) and signs the list with this run's development OS key (`glass-release manifest`,
#   here); the VM trusts the run's development root for the run only, and requires the check, as
#   an image built for release does;
# - glassd starts again: the check says verified, the files are made read-only (the system
#   immutable flag: not even root writes one) and an agent runs;
# - a listed program changed (as root, the flag taken off first) and a program nobody listed
#   planted: glassd's next start says changed, names both, puts it on the audit record, and
#   refuses to start an agent;
# - put back, verified again.
#
# As the run ends, everything it changed goes back: glassd.toml, the list and the key list, the
# flags it set, the planted program. Needs the current build deployed (`make freebsd vm-deploy`).
# SHOTS=<dir>: screenshots of each step there (tools/screenshot/termshot.py).
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm="$repo/os/freebsd/scripts/vm.sh"
keys=$repo/build/osupdate
conf=/usr/local/etc/glasshouse/glassd.toml
share=/usr/local/share/glasshouse
dirs="/bin /sbin /lib /libexec /usr/bin /usr/sbin /usr/lib /usr/lib32 /usr/libexec /usr/local/bin \
/usr/local/sbin /usr/local/lib /usr/local/libexec /boot/kernel"
e2e=/usr/local/libexec/glasshouse-tests/glass-e2e
work=$(mktemp -d)
failures=0

log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
fail() {
	log "FAIL $*"
	failures=$((failures + 1))
}
in_vm() { "$vm" ssh "$@"; }
field() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
check() { in_vm 'glassctl --json status' | field "(d.get('system_files') or {}).get($1)"; }
release() { cargo run -q --manifest-path "$repo/Cargo.toml" -p glass-release -- "$@"; }
# shot <name> <title> <user> <command...>: what the commands print in the VM, as a picture.
shot() {
	[[ -n ${SHOTS:-} ]] || return 0
	local name=$1 title=$2 user=$3
	shift 3
	mkdir -p "$SHOTS"
	"$repo/tools/screenshot/termshot.py" --out "$SHOTS/$name.png" --title "$title" --vm --user "$user" -- "$@" >/dev/null
}
pub() { release public --key "$keys/$1"; }

# glassd started again, and its check of the system done.
restart() {
	in_vm 'service glassd restart >/dev/null'
	local state=
	for _ in $(seq 300); do
		state=$(check "'state'" 2>/dev/null || true)
		[[ $state == verified || $state == changed || $state == unchecked ]] && break
		sleep 2
	done
	log "glassd's check: $state in $(check "'took_ms'") ms, $(check "'files'") files"
}

mkdir -p "$keys"
for k in root os; do
	[[ -f $keys/$k ]] || release keygen --out "$keys/$k" >/dev/null
done
[[ $(in_vm "test -x $e2e && echo yes" || true) == yes ]] ||
	{ log "no $e2e in the VM: make e2e-vm puts it there"; exit 1; }

untrust() {
	in_vm "set +e
		[ -f /var/tmp/verify-yes ] && { chflags noschg /usr/bin/yes; cp /var/tmp/verify-yes /usr/bin/yes; rm -f /var/tmp/verify-yes; }
		rm -f /usr/local/bin/glass-planted $share/system.json $share/keys.json
		if [ -f /var/tmp/verify-schg-before ]; then
			find $dirs -type f -flags +schg 2>/dev/null | sort >/var/tmp/verify-schg-after
			comm -13 /var/tmp/verify-schg-before /var/tmp/verify-schg-after | tr '\n' '\0' | xargs -0 -r chflags noschg
			rm -f /var/tmp/verify-schg-before /var/tmp/verify-schg-after
		fi
		[ -f $conf.before-verify-test ] && mv $conf.before-verify-test $conf
		service glassd restart >/dev/null" ||
		log "FAIL the VM isn't back as it was: put $conf.before-verify-test back over $conf"
	rm -rf "$work"
}
trap untrust EXIT

# The flags the system had before the run: the run takes off only those it set. And the program
# it changes, as it was.
in_vm "find $dirs -type f -flags +schg 2>/dev/null | sort >/var/tmp/verify-schg-before &&
	cp /usr/bin/yes /var/tmp/verify-yes"
log "the release side lists the VM's system files"
started=$(date +%s)
in_vm 'glassctl system manifest' >"$work/list.json"
log "listed $(field 'len(d["files"])' <"$work/list.json") files in $(($(date +%s) - started)) s"
number=$(date +%s)
release keys --root "$keys/root" --version "$number" --key "os=$(pub os)" --out "$work/keys.json" >&2
release manifest "$work/list.json" --os-key "$keys/os" --number "$number" --out "$work/system.json" >&2
"$vm" scp-in "$work/system.json" "$share/system.json"
"$vm" scp-in "$work/keys.json" "$share/keys.json"
in_vm "cp $conf $conf.before-verify-test &&
	printf '\n[update]\nroots = [\"%s\"]\nrequire_signed_system = true\n' '$(pub root)' >>$conf"

log "verified: the system is the image's"
restart
[[ $(check "'state'") == verified ]] || fail "not verified: $(check "'note'")"
(($(check "'sealed'") > 0)) || fail "no file made read-only"
in_vm 'ls -lo /usr/bin/yes' | grep -q schg || fail "/usr/bin/yes isn't read-only"
if in_vm 'printf x >>/usr/bin/yes' 2>/dev/null; then fail "root wrote a read-only system file"; fi
in_vm "su demo -c '$e2e invoices'" >&2 || fail "an agent didn't run on a verified system"
shot 01-verified "1 · Verified at glassd's start: the system is the image's, read-only even to root (ADR 0064)" root \
	"glassctl status" "ls -lo /usr/bin/yes" "printf x >>/usr/bin/yes"

log "changed: a listed program changed, one nobody listed planted"
in_vm "chflags noschg /usr/bin/yes && printf x >>/usr/bin/yes &&
	printf '#!/bin/sh\necho planted\n' >/usr/local/bin/glass-planted && chmod 755 /usr/local/bin/glass-planted"
restart
in_vm 'glassctl status' >&2
[[ $(check "'state'") == changed ]] || fail "not changed: $(check "'state'")"
[[ $(check "'different'") == 2 ]] || fail "$(check "'different'") differences, not 2"
differences=$(check "'differences'")
[[ $differences == *"/usr/bin/yes (changed)"* && $differences == *"/usr/local/bin/glass-planted (not listed)"* ]] ||
	fail "the differences don't name both: $differences"
if in_vm "su demo -c '$e2e invoices'" >"$work/refused.log" 2>&1; then
	fail "an agent ran on a changed system"
fi
grep -q "changed since it was signed" "$work/refused.log" || fail "the refusal doesn't say why: $(tail -3 "$work/refused.log")"
said=$(in_vm 'grep -c system_checked /var/db/glasshouse/audit/audit.log' || true)
shot 02-changed "2 · A program changed and one planted: glassd's next start says so, and no agent runs (ADR 0064)" root \
	"glassctl status" "su demo -c '$e2e invoices' 2>&1 | grep -E '^FAIL|scenarios' | cut -c1-200" \
	"grep system_checked /var/db/glasshouse/audit/audit.log | tail -2 | sed -E 's/.*\"summary\":\"([^\"]*)\".*/\\1/' | cut -c1-200"
((said >= 2)) || fail "$said checks on the audit record, not 2"

log "put back: verified again"
in_vm "cp /var/tmp/verify-yes /usr/bin/yes && rm -f /usr/local/bin/glass-planted"
restart
[[ $(check "'state'") == verified ]] || fail "not verified once put back: $(check "'note'")"
shot 03-put-back "3 · Put back: verified again at the next start (ADR 0064)" root "glassctl status"
in_vm 'glassctl verify' >/dev/null || fail "the audit log doesn't verify"

if ((failures)); then
	log "system check: $failures failures"
	exit 1
fi
log "system check: passed"
