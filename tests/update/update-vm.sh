#!/usr/bin/env bash
# Usage: tests/update/update-vm.sh [good] [refuse] [bad] [undo]
# Signed OS updates end to end in the dev VM (ADR 0061, specs/tla/OsUpdate.tla). With no
# arguments, all of them, in that order:
#
# - good: a release of the next version, a delta from the image the VM runs, signed by the pilot
#   channel's key with its image signed by the OS key, both listed by the root, is installed in a
#   boot environment of its own; the VM restarts onto it once; glassd checks the image is the
#   release's and keeps it.
# - refuse: the same release again, one signed by a key the root doesn't list, an image changed
#   after it was signed, and one after the root revokes the channel's key (and after the list
#   before is played back) are each refused and on the audit record.
# - bad: a release whose image doesn't start (its init is missing): the VM restarts by itself on
#   the version before, and glassd says it rolled back.
# - undo: `glassctl update undo`: the next start is the version before, which removes the update.
#
# The VM trusts this run's development root key, follows the pilot channel and reads releases from
# /var/db/glasshouse-updates for this run only: its glassd.toml is put back however the run ends.
# A whole image is the size of the VM's own (about 4 GB, more than the dev disk spares), so the VM
# makes deltas: the changes since a snapshot of the image it runs, as the release pipeline would
# since the image it shipped. Needs the current build deployed (`make freebsd vm-deploy`).
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
vm="$repo/os/freebsd/scripts/vm.sh"
keys=$repo/build/osupdate
conf=/usr/local/etc/glasshouse/glassd.toml
source=/var/db/glasshouse-updates
snap=glass-image
work=$(mktemp -d)
failures=0

log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
fail() {
	log "FAIL $*"
	failures=$((failures + 1))
}
in_vm() { "$vm" ssh "$@"; }
field() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
status() { in_vm 'glassctl --json update' | field "$1"; }
release() { cargo run -q --manifest-path "$repo/Cargo.toml" -p glass-release -- "$@"; }

mkdir -p "$keys"
for k in root os pilot rogue; do
	[[ -f $keys/$k ]] || release keygen --out "$keys/$k" >/dev/null
done
pub() { release public --key "$keys/$1"; }

# The root's key list, version $1, revoking the keys named after it.
publish_keys() {
	local version=$1 revoke=()
	shift
	for k in "$@"; do revoke+=(--revoke "$(pub "$k")"); done
	release keys --root "$keys/root" --version "$version" --key "os=$(pub os)" \
		--key "channel:pilot=$(pub pilot)" "${revoke[@]}" --out "$work/keys.json" >&2
	"$vm" scp-in "$work/keys.json" "$source/keys.json" >&2
}

root_ds=$(in_vm 'zpool get -H -o value bootfs zroot' | sed 's|/[^/]*$||')
# glassd.toml is in the boot environment, with the build deployed into it.
[[ $(in_vm 'kenv -q vfs.root.mountfrom') == */default ]] ||
	{ log "the VM isn't running its default boot environment"; exit 1; }
in_vm "test ! -e $conf.before-update-test" || {
	log "an earlier run left the VM trusting its root: put $conf.before-update-test back over $conf"
	exit 1
}
in_vm "cp $conf $conf.before-update-test && mkdir -p $source/pilot && chmod 700 $source &&
	printf '\n[update]\nroots = [\"%s\"]\nchannel = \"pilot\"\nsource = \"%s\"\n' '$(pub root)' '$source' >>$conf &&
	service glassd restart >/dev/null"
untrust() {
	in_vm "if [ -f $conf.before-update-test ]; then mv $conf.before-update-test $conf &&
		service glassd restart >/dev/null; fi
		rm -rf $source" || true
	rm -rf "$work"
}
trap untrust EXIT
publish_keys "$(date +%s)"

back() {
	"$vm" ssh-close
	sleep 15
	"$vm" wait 600 >/dev/null
	for _ in $(seq 60); do in_vm 'glassctl status' >/dev/null 2>&1 && return; sleep 2; done
	fail "glassd didn't come back"
}
restart() {
	in_vm 'shutdown -r now' >/dev/null 2>&1 || true
	back
}

# make_release <version> [loader.conf line]: a delta from the image the VM runs to <version>, signed
# as release number $(date +%s) on the pilot channel, by $SIGNER (pilot unless set).
make_release() {
	local next=$1 loader=${2:-} running current image sha bytes number
	running=$(in_vm 'bectl list -H' | awk '$2 ~ /N/ {print $1}')
	current=$(in_vm 'cat /usr/local/share/glasshouse/os-release.json' | field 'd["version"]')
	image="$source/pilot/glasshouse-$next.zfs"
	# The image it runs, as the release pipeline would have it: its snapshot.
	in_vm "zfs list -H -t snapshot $root_ds/$running@$snap >/dev/null 2>&1 || zfs snapshot $root_ds/$running@$snap
		zfs destroy -r $root_ds/glass-build-next 2>/dev/null || true
		zfs clone -o canmount=noauto -o mountpoint=/ $root_ds/$running@$snap $root_ds/glass-build-next
		mkdir -p /tmp/glass-build-next && mount -t zfs $root_ds/glass-build-next /tmp/glass-build-next
		printf '{\"version\": \"%s\", \"base\": \"15.1-RELEASE\"}\n' '$next' >/tmp/glass-build-next/usr/local/share/glasshouse/os-release.json
		printf '%s\n' '$next' >/tmp/glass-build-next/usr/local/share/glasshouse/update-test
		if [ -n '$loader' ]; then printf '%s\n' '$loader' >>/tmp/glass-build-next/boot/loader.conf; fi
		umount /tmp/glass-build-next && zfs snapshot $root_ds/glass-build-next@$snap
		zfs send -c -i $root_ds/$running@$snap $root_ds/glass-build-next@$snap >$image
		zfs destroy -r $root_ds/glass-build-next" >&2
	sha=$(in_vm "sha256 -q $image")
	bytes=$(in_vm "stat -f %z $image")
	number=$(date +%s)
	release release /dev/null --sha256 "$sha" --bytes "$bytes" --delta-from "$current" \
		--source "$work/source" --channel pilot --number "$number" --version "$next" \
		--base 15.1-RELEASE --os-key "$keys/os" --channel-key "$keys/${SIGNER:-pilot}" >&2
	"$vm" scp-in "$work/source/pilot/release.json" "$source/pilot/release.json" >&2
	log "release $number: $next, a $bytes-byte delta from $current"
}

# The update is refused with a message containing $1, and nothing is set up.
refused() {
	local why=$1 before after out
	before=$(in_vm 'bectl list -H | cut -f1 | sort')
	if out=$(in_vm 'glassctl update apply' 2>&1); then
		fail "taken, and should be refused for: $why"
	elif [[ $out != *"$why"* ]]; then
		fail "refused, but not for $why: $out"
	else
		log "refused as it should be: ${out##*: }"
	fi
	after=$(in_vm 'bectl list -H | cut -f1 | sort')
	[[ $before == "$after" ]] || fail "a refused update changed the boot environments"
}

base_version=
good() {
	base_version=$(in_vm 'cat /usr/local/share/glasshouse/os-release.json' | field 'd["version"]')
	local next="$base_version-u1" env
	log "good: $base_version to $next"
	make_release "$next"
	in_vm 'glassctl update check' >&2
	[[ $(in_vm 'glassctl --json update check' | field 'd["available"]["version"]') == "$next" ]] ||
		fail "good: the check doesn't offer $next"
	in_vm 'glassctl update apply' >&2
	[[ $(status 'd["update"]["state"]') == staged ]] || fail "good: not staged"
	env=$(status 'd["update"]["env"]')
	in_vm 'bectl list -H' | grep -q "^$env	T	" || fail "good: $env isn't set to start once"
	restart
	[[ $(status 'd["running"]') == "$env" ]] || fail "good: not running from $env"
	[[ $(status 'd["update"]["state"]') == kept ]] || fail "good: not kept: $(status 'd["update"]')"
	[[ $(status 'd["version"]') == "$next" ]] || fail "good: the image doesn't say $next"
	in_vm 'bectl list -H' | grep -q "^$env	NR	" || fail "good: $env isn't the one every start uses"
	[[ $(in_vm 'cat /usr/local/share/glasshouse/update-test') == "$next" ]] || fail "good: not the new image's files"
}

refuse() {
	log "refuse: what the VM doesn't take"
	refused "not newer"
	SIGNER=rogue make_release "$(status 'd["version"]')-rogue"
	refused "doesn't trust"
	make_release "$(status 'd["version"]')-tampered"
	in_vm "printf 'x' | dd of=$source/pilot/glasshouse-$(status 'd["version"]')-tampered.zfs bs=1 seek=100 conv=notrunc 2>/dev/null"
	refused "doesn't match the digest"
	in_vm 'glassctl update' | grep -q "Rolled back" || true
	make_release "$(status 'd["version"]')-revoked"
	local revoking
	revoking=$(date +%s)
	publish_keys "$revoking" pilot
	refused "no key is listed for the pilot channel"
	# The list before, played back, doesn't bring the key back.
	publish_keys $((revoking - 1))
	refused "no key is listed for the pilot channel"
	local said
	said=$(in_vm 'grep -c "OS update refused" /var/db/glasshouse/audit/audit.log' || true)
	((said >= 4)) || fail "refuse: $said refusals on the audit record, not 4"
}

bad() {
	local from next env
	from=$(status 'd["version"]')
	next="$from-noinit"
	log "bad: an image whose init is missing"
	# The root lists a new pilot key: the revoked one stays revoked.
	release keygen --out "$work/pilot2" >/dev/null
	release keys --root "$keys/root" --version "$(date +%s)" --key "os=$(pub os)" \
		--key "channel:pilot=$(release public --key "$work/pilot2")" --revoke "$(pub pilot)" \
		--out "$work/keys.json" >&2
	"$vm" scp-in "$work/keys.json" "$source/keys.json" >&2
	cp "$work/pilot2" "$keys/pilot2"
	SIGNER=pilot2 make_release "$next" 'init_path="/sbin/glass-no-such-init"'
	in_vm 'glassctl update apply' >&2
	env=$(status 'd["update"]["env"]')
	in_vm 'shutdown -r now' >/dev/null 2>&1 || true
	"$vm" ssh-close
	sleep 20
	# The kernel finds no init, panics and restarts (no debugger on panic), on the version before.
	"$vm" wait 900 >/dev/null || {
		log "bad: no answer; a power cut, as the person would"
		"$vm" power-cut >&2
		back
	}
	for _ in $(seq 60); do in_vm 'glassctl status' >/dev/null 2>&1 && break; sleep 2; done
	[[ $(status 'd["version"]') == "$from" ]] || fail "bad: not back on $from"
	[[ $(status 'd["update"]["state"]') == rolled_back ]] || fail "bad: not rolled back: $(status 'd["update"]')"
	in_vm 'bectl list -H' | grep -q "^$env	" && fail "bad: $env is still there"
	rm -f "$keys/pilot2"
	log "bad: $(status 'd["update"]["note"]')"
}

undo() {
	local env
	env=$(status 'd["running"]')
	log "undo: back to the version before $(status 'd["version"]')"
	in_vm 'glassctl update undo' >&2
	restart
	[[ $(status 'd["update"]["state"]') == undone ]] || fail "undo: not taken out: $(status 'd["update"]')"
	[[ -z $base_version || $(status 'd["version"]') == "$base_version" ]] || fail "undo: not back on $base_version"
	in_vm 'bectl list -H' | grep -q "^$env	" && fail "undo: $env is still there"
	# The snapshot the deltas came from, now no update is made from it.
	in_vm "zfs destroy $root_ds/\$(bectl list -H | awk '\$2 ~ /N/ {print \$1}')@$snap" 2>/dev/null || true
}

wanted=("$@")
((${#wanted[@]})) || wanted=(good refuse bad undo)
for scenario in "${wanted[@]}"; do
	"$scenario"
	in_vm 'glassctl verify' || fail "$scenario: the audit log doesn't verify"
done
in_vm 'glassctl update'
if ((failures)); then
	log "OS updates: $failures failures"
	exit 1
fi
log "OS updates: passed"
