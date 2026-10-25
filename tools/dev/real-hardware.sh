#!/usr/bin/env bash
# Usage: tools/dev/real-hardware.sh up [model] [--reviewed-only]   (default claude-sonnet-5-5)
#        tools/dev/real-hardware.sh down
#
# The hardware agent (ADR 0057) in the dev VM's jails on a real Anthropic model, from a cloud
# session. On this host it starts:
# - the build service, glass-kbuild, which builds in the VM through kernel-patch.sh and signs
#   agents' patches with build/kpatch/agent-key;
# - the model relay, tools/dev/model-relay.py, which puts in the key from
#   GLASS_ANTHROPIC_API_KEY (never ANTHROPIC_API_KEY): it never enters the VM;
# - an ssh tunnel that gives the VM both on its own loopback.
#
# In the VM, for the run only, it writes:
# - glassd.toml: `model_relay = true`, and [hardware] naming the build service, its token and
#   its key as a community key;
# - a real-model policy whose anthropic provider is the relay;
# - a placeholder model secret.
#
# The VM takes community patches, unless --reviewed-only says it takes only reviewed ones: then
# an agent's patch goes to IT for review, on a VM enrolled with a console
# (os/freebsd/scripts/enroll-dev.sh). Then, as demo:
#   glassctl run hardware-agent "The sound stops working after the computer wakes from sleep"
# `down` stops it all and puts each file back as it was.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
# vm_running, vm_ssh, vm_scp, SSH_OPTS, VM_SSH_PORT, die, log.
# shellcheck source=os/freebsd/scripts/lib.sh
. "$repo/os/freebsd/scripts/lib.sh"
dev=$repo/build/real-hardware
bin=${CARGO_TARGET_DIR:-$repo/target}/debug
etc=/usr/local/etc/glasshouse
saved=real-hardware-saved
files="glassd.toml policy.toml model-secrets.toml"

stop() {
	for pid in "$dev"/*.pid; do
		if [[ -f $pid ]]; then
			kill "$(cat "$pid")" 2>/dev/null || true
			rm -f "$pid"
		fi
	done
}

down() {
	stop
	rm -f "$dev/kbuild-token"
	vm_running || return 0
	vm_ssh "rm -f $etc/kbuild-token
for f in $files; do
	if [ -e $etc/\$f.$saved ]; then mv $etc/\$f.$saved $etc/\$f; elif [ \$f != glassd.toml ]; then rm -f $etc/\$f; fi
done
service glassd restart </dev/null >/dev/null 2>&1" || die "couldn't put the VM back; look in $etc"
	log "the VM is back as it was"
}

up() {
	local model=claude-sonnet-5-5 community=true
	for arg in "$@"; do
		case $arg in
		--reviewed-only) community=false ;;
		*) model=$arg ;;
		esac
	done
	[[ -n ${GLASS_ANTHROPIC_API_KEY:-} ]] || die "GLASS_ANTHROPIC_API_KEY isn't set: add it in this environment's settings"
	vm_running || die "VM is not running (vm.sh up && vm.sh wait)"
	vm_ssh "test ! -e $etc/glassd.toml.$saved" || die "it's up already: real-hardware.sh down first"
	mkdir -p "$dev"
	cargo build -q -p glass-kbuild -p glass-kpatch -p glass-e2e
	local key=$repo/build/kpatch/agent-key
	[[ -f $key ]] || "$bin/glass-kpatch" keygen --out "$key" >/dev/null
	local public
	public=$("$bin/glass-kpatch" public --key "$key")
	# A new token each run, as `down` removes it.
	rm -f "$dev/kbuild-token"
	"$bin/glass-kbuild" token --out "$dev/kbuild-token"

	# Set first: anything after this point is undone.
	trap 'down' ERR
	# Each in a session of its own, outliving the shell that started it.
	setsid "$bin/glass-kbuild" serve --token-file "$dev/kbuild-token" --release "$FREEBSD_RELEASE" \
		--repo "$repo" --key "$key" </dev/null >"$dev/kbuild.log" 2>&1 &
	echo $! >"$dev/kbuild.pid"
	setsid python3 "$repo/tools/dev/model-relay.py" 8787 </dev/null >"$dev/relay.log" 2>&1 &
	echo $! >"$dev/relay.pid"
	# A connection of its own, not the shared one (ssh takes an option's first value, so these
	# come before SSH_OPTS): `vm.sh ssh-close`, which deploys run, would take the tunnel with it.
	setsid ssh -o ControlMaster=no -o ControlPath=none -o ExitOnForwardFailure=yes "${SSH_OPTS[@]}" \
		-p "$VM_SSH_PORT" -N -R 7500:127.0.0.1:7500 -R 8787:127.0.0.1:8787 root@127.0.0.1 \
		</dev/null >"$dev/tunnel.log" 2>&1 &
	echo $! >"$dev/tunnel.pid"
	sleep 3
	for name in kbuild relay tunnel; do
		kill -0 "$(cat "$dev/$name.pid")" 2>/dev/null || die "the $name didn't start: $(cat "$dev/$name.log")"
	done

	# The real-model policy, its anthropic provider the relay.
	"$bin/glass-e2e" real-model-policy anthropic "$model" |
		awk '{ print } /^kind = "anthropic"$/ { print "endpoint = \"http://127.0.0.1:8787\"" }' >"$dev/policy.toml"
	grep -q '^endpoint = "http://127.0.0.1:8787"$' "$dev/policy.toml" || die "the policy names no relay"
	vm_scp "$dev/kbuild-token" "$etc/kbuild-token.new"
	vm_ssh "set -e
for f in $files; do
	if [ -e $etc/\$f ]; then cp -p $etc/\$f $etc/\$f.$saved; fi
done
{ echo 'model_relay = true'; cat $etc/glassd.toml.$saved; } >$etc/glassd.toml
cat >>$etc/glassd.toml <<TOML

[hardware]
community_keys = [\"$public\"]
community = $community
probation_minutes = 5
build_service = \"http://127.0.0.1:7500\"
build_token = \"$etc/kbuild-token\"
build_allow_insecure_http = true
TOML
mv $etc/kbuild-token.new $etc/kbuild-token
chmod 600 $etc/kbuild-token
(umask 077; printf '[anthropic]\napi_key = \"relayed\"\n' >$etc/model-secrets.toml)
service glassd restart </dev/null >/dev/null 2>&1
for i in \$(jot 120); do glassctl status >/dev/null 2>&1 && break; sleep 0.5; done"
	vm_scp "$dev/policy.toml" "$etc/policy.toml"
	vm_ssh "service glassd restart </dev/null >/dev/null 2>&1
for i in \$(jot 120); do glassctl status >/dev/null 2>&1 && break; sleep 0.5; done"
	trap - ERR
	log "the hardware agent is on $model in the VM (community patches: $community); 'real-hardware.sh down' after"
}

case ${1:-} in
up)
	shift
	up "$@"
	;;
down) down ;;
*) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2 && exit 2 ;;
esac; exit
