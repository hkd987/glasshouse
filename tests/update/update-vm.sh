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
# UPDATE_VIA says how the VM gets the releases (default folder):
# - folder: from a folder, a dataset of its own in the VM, as a share or USB drive is;
# - https: from this host over HTTPS, as from Glasshouse's update server
#   (tools/dev/serve-https.py, with the development certificate, which the VM trusts for this run
#   only);
# - console: from a development admin console holding them in its --updates folder, the VM enrolled
#   with it for this run (enroll-dev.sh, undone as the run ends) and put in its pilot ring. The VM
#   hears of the release as it syncs, and downloads and installs it by itself; `good` checks that it
#   did, then restarts into it as a person would (`glassctl update restart tonight`, then `now`).
#   First, IT holds the pilot ring back as the release comes out, and the VM hears of nothing until
#   IT resumes.
# With https or console, the scenarios are good and undo.
#
# The VM trusts this run's development root key, follows the pilot channel and reads releases from
# /var/db/glasshouse-updates for this run only: its glassd.toml is put back as the run ends. The
# releases are on a dataset of their own, outside the boot environments, as a share or USB drive is.
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
via=${UPDATE_VIA:-folder}
# Where this host keeps the releases it serves, with https or console.
served=$work/served
https_port=7444
tls=$repo/build/console/tls
ca=/usr/local/share/certs/glasshouse-dev-console.pem
console_url=http://127.0.0.1:7400
pids=()
case $via in folder | https | console) ;; *)
	echo "UPDATE_VIA is folder, https or console" >&2
	exit 2
	;;
esac
mkdir -p "$served/pilot"

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
	offer "$work/keys.json" keys.json
}

# offer <file> <name>: puts a file of the releases where the VM reads them, by $via.
offer() {
	if [[ $via == folder ]]; then
		"$vm" scp-in "$1" "$source/$2" >&2
	else
		cp "$1" "$served/$2"
	fi
}

root_ds=$(in_vm 'zpool get -H -o value bootfs zroot' | sed 's|/[^/]*$||')
# glassd.toml is in the boot environment, with the build deployed into it.
[[ $(in_vm 'kenv -q vfs.root.mountfrom') == */default ]] ||
	{ log "the VM isn't running its default boot environment"; exit 1; }
in_vm "test ! -e $conf.before-update-test" || {
	log "an earlier run left the VM trusting its root: put $conf.before-update-test back over $conf"
	exit 1
}
pool=${root_ds%%/*}
if [[ $via == console ]] && curl -fsS -o /dev/null "$console_url/" 2>/dev/null; then
	log "a console is already on $console_url: stop it (make console-dev) first"
	exit 1
fi
case $via in
folder) update_source="source = \"$source\"" ;;
https) update_source="source = \"https://10.0.2.2:$https_port\"" ;;
# A managed computer takes its updates from its console unless it names another source.
console) update_source="" ;;
esac
in_vm "cp $conf $conf.before-update-test &&
	{ zfs destroy -r $pool/glass-update-source 2>/dev/null || true; } &&
	zfs create -o mountpoint=$source $pool/glass-update-source &&
	mkdir -p $source/pilot && chmod 700 $source &&
	printf '\n[update]\nroots = [\"%s\"]\nchannel = \"pilot\"\n%s\n' '$(pub root)' '$update_source' >>$conf &&
	service glassd restart >/dev/null"
untrust() {
	# The VM answers first, whatever stopped the run.
	"$vm" wait 600 >/dev/null 2>&1 || { "$vm" power-cut >&2 && "$vm" wait 900 >/dev/null; } || true
	if [[ $via == console ]]; then
		# A run stopped while IT held the ring back leaves the dev console holding nothing.
		curl -fsS -b "$work/jar" -H 'content-type: application/json' -d '{"ring": "pilot", "hold": false}' \
			"$console_url/api/updates/hold" >/dev/null 2>&1 || true
		"$repo/os/freebsd/scripts/enroll-dev.sh" --undo || log "FAIL enroll-dev.sh --undo"
	fi
	in_vm "zfs destroy -r $pool/glass-update-source
		rm -f $ca && certctl rehash
		case \$(kenv -q vfs.root.mountfrom) in */default) ;; *) exit 3 ;; esac
		if [ -f $conf.before-update-test ]; then mv $conf.before-update-test $conf &&
		service glassd restart >/dev/null; fi" ||
		log "FAIL the VM's glassd.toml isn't back: in its default boot environment, put" \
			"$conf.before-update-test back over $conf"
	((${#pids[@]})) && kill "${pids[@]}" 2>/dev/null
	pkill -f '^python3 .*tools/dev/console-tls.py' 2>/dev/null || true
	rm -rf "$work"
}
trap untrust EXIT

# The VM trusts the development CA for this run only (untrust takes it out).
if [[ $via == https ]]; then
	# shellcheck source=os/freebsd/scripts/lib.sh
	. "$repo/os/freebsd/scripts/lib.sh"
	dev_tls "$tls"
	setsid python3 "$repo/tools/dev/serve-https.py" "$served" "$tls/console.pem" "$tls/console.key" \
		"$https_port" >"$work/serve-https.log" 2>&1 &
	pids+=($!)
	in_vm "mkdir -p ${ca%/*}"
	"$vm" scp-in "$tls/ca.pem" "$ca"
	in_vm 'certctl rehash'
fi
if [[ $via == console ]]; then
	cargo build -q --manifest-path "$repo/Cargo.toml" -p glass-console
	setsid "${CARGO_TARGET_DIR:-$repo/target}/debug/glass-console" --dev --state "$repo/build/console" \
		--web "$repo/apps/console/dist" --updates "$served" >"$work/console.log" 2>&1 &
	pids+=($!)
	for _ in $(seq 100); do curl -fsS -o /dev/null "$console_url/" 2>/dev/null && break; sleep 0.2; done
	"$repo/os/freebsd/scripts/enroll-dev.sh" >&2
	# IT puts the VM in the pilot ring: its channel. The first admin the console offers who may.
	people=$(curl -sS "$console_url/api/session" | field '" ".join(p["user"] for p in d["people"])')
	ringed=
	for _ in $(seq 60); do
		for who in $people; do
			curl -sS -c "$work/jar" -H 'content-type: application/json' -d "{\"user\": \"$who\"}" \
				"$console_url/api/session" >/dev/null
			if curl -fsS -b "$work/jar" -H 'content-type: application/json' -d '{"ring": "pilot"}' \
				"$console_url/api/fleet/glasshouse-vm/ring" >/dev/null 2>&1; then
				ringed=1
				break 2
			fi
		done
		sleep 2
	done
	[[ -n $ringed ]] || { log "couldn't put the VM in the pilot ring"; exit 1; }
	log "the VM is enrolled with the console, in its pilot ring"
fi
publish_keys "$(date +%s)"

back() {
	"$vm" ssh-close
	sleep 15
	# Emulated, a start now and then waits for an interrupt that never comes: then a power cut.
	"$vm" wait 600 >/dev/null || {
		log "no answer; a power cut"
		"$vm" power-cut >&2
		"$vm" wait 900 >/dev/null
	}
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
	# Made in the VM, where the image it comes from is; served from this host otherwise.
	[[ $via == folder ]] || in_vm "cat $image" >"$served/pilot/glasshouse-$next.zfs"
	offer "$work/source/pilot/release.json" pilot/release.json
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
	if [[ $via == console ]]; then
		# IT holds the pilot ring back as the release comes out: the VM hears of nothing.
		it_hold true
		make_release "$next"
		sleep 60
		[[ $(status '(d.get("available") or {}).get("version", "")') != "$next" ]] ||
			fail "good: the VM heard of $next while IT held its ring back"
		[[ $(status '(d.get("update") or {}).get("version", "")') != "$next" ]] ||
			fail "good: the VM installed $next while IT held its ring back"
		it_hold false
	else
		make_release "$next"
	fi
	if [[ $via == console ]]; then
		# Nobody checks: once IT resumes, the VM hears of the release as it syncs, and downloads it
		# by itself.
		local heard=
		for _ in $(seq 90); do
			heard=$(status 'd.get("available", {}).get("version", "")')
			[[ $heard == "$next" ]] && break
			sleep 2
		done
		[[ $heard == "$next" ]] || fail "good: the VM didn't hear of $next from the console"
		# A managed computer installs it by itself; the person picks when to restart.
		local state=
		for _ in $(seq 90); do
			state=$(status '(d.get("update") or {}).get("state", "")')
			[[ $state == staged ]] && break
			sleep 2
		done
		in_vm 'grep "OS update" /var/log/glassd.log | tail -3' >&2 || true
		[[ $state == staged ]] || fail "good: the VM didn't install $next by itself"
		in_vm 'glassctl update restart tonight' >&2
		[[ -n $(status 'd.get("restart_at") or ""') ]] || fail "good: no restart set for tonight"
		env=$(status 'd["update"]["env"]')
		in_vm 'bectl list -H' | grep -q "^$env	T	" || fail "good: $env isn't set to start once"
		in_vm 'glassctl update restart now' >/dev/null 2>&1 || true
		back
	else
		in_vm 'glassctl update check' >&2
		[[ $(in_vm 'glassctl --json update check' | field 'd["available"]["version"]') == "$next" ]] ||
			fail "good: the check doesn't offer $next"
		in_vm 'glassctl update apply' >&2
		[[ $(status 'd["update"]["state"]') == staged ]] || fail "good: not staged"
		env=$(status 'd["update"]["env"]')
		in_vm 'bectl list -H' | grep -q "^$env	T	" || fail "good: $env isn't set to start once"
		restart
	fi
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

# it_hold <true|false>: IT holds the pilot ring's OS updates back, or resumes them (ADR 0061 §11).
it_hold() {
	curl -fsS -b "$work/jar" -H 'content-type: application/json' -d "{\"ring\": \"pilot\", \"hold\": $1}" \
		"$console_url/api/updates/hold" >/dev/null || fail "the console didn't take hold=$1"
	log "IT $([[ $1 == true ]] && echo holds back || echo resumes) the pilot ring's OS updates"
}

undo() {
	local env
	env=$(status 'd["running"]')
	log "undo: back to the version before $(status 'd["version"]')"
	in_vm 'glassctl update undo' >&2
	in_vm 'shutdown -r now' >/dev/null 2>&1 || true
	"$vm" ssh-close
	sleep 20
	# Emulated, a start now and then waits for an interrupt that never comes: then a power cut,
	# which starts the version before all the same.
	"$vm" wait 600 >/dev/null || {
		log "undo: no answer; a power cut"
		"$vm" power-cut >&2
	}
	back
	[[ $(status 'd["update"]["state"]') == undone ]] || fail "undo: not taken out: $(status 'd["update"]')"
	[[ -z $base_version || $(status 'd["version"]') == "$base_version" ]] || fail "undo: not back on $base_version"
	in_vm 'bectl list -H' | grep -q "^$env	" && fail "undo: $env is still there"
	# The snapshot the deltas came from, now no update is made from it.
	in_vm "zfs destroy $root_ds/\$(bectl list -H | awk '\$2 ~ /N/ {print \$1}')@$snap" 2>/dev/null || true
}

wanted=("$@")
if ((!${#wanted[@]})); then
	if [[ $via == folder ]]; then wanted=(good refuse bad undo); else wanted=(good undo); fi
fi
for scenario in "${wanted[@]}"; do
	"$scenario"
	in_vm 'glassctl verify' || fail "$scenario: the audit log doesn't verify"
done
in_vm 'glassctl update'
if ((failures)); then
	log "OS updates: $failures failures"
	exit 1
fi
log "OS updates ($via): passed"
