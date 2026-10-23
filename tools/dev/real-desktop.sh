#!/usr/bin/env bash
# Usage: tools/dev/real-desktop.sh up [model]   (default claude-haiku-5-5)
#        tools/dev/real-desktop.sh down
#
# The agents on a real Anthropic model, on the dev VM's own desktop, from a cloud session: a
# glassd on this machine (tools/dev/host-run.sh, NOTHING CONTAINED) with a seeded workday (an
# Inbox, this week's calendar, launch notes, the demo invoices; tools/dev/seed-workday.py), its
# control socket forwarded into the VM over ssh, and the desktop shell on the VM's screen
# drawing that glassd's tiles. Type into the command bar (vm.sh key ctrl-spc, vm.sh type, vm.sh
# key ret) and the desktop agent works on the real model; approve, review and roll back on the
# screen; see it with vm.sh screendump. The Mail and Calendar apps for that glassd are at
# http://mail.localhost:7300 (glass-app-bridge, started here too).
#
# Why not in the VM's own glassd: in a cloud session the VM's TLS to api.anthropic.com is
# re-signed by the session's egress proxy, and the VM never trusts that (real-model.sh). On a
# machine without one, `tools/dev/real-model.sh --provider anthropic` runs the agents in the
# VM's jails instead. The key comes from GLASS_ANTHROPIC_API_KEY (never ANTHROPIC_API_KEY) into a
# file only its owner can read, and is never printed; `down` deletes it and puts the VM's
# desktop back on its own glassd.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
# Short, so the sockets' paths stay under 104 bytes.
dev=/tmp/glass-real-desktop
run=$dev/run
bin=${CARGO_TARGET_DIR:-$repo/target}/debug
vm=$repo/os/freebsd/scripts/vm.sh
# The VM side of the forwarded control socket.
inside=/var/run/glass-host.sock
# vm_running, vm_ssh, vm_scp, SSH_OPTS, die.
# shellcheck source=os/freebsd/scripts/lib.sh
. "$repo/os/freebsd/scripts/lib.sh"

down() {
	for pid in "$dev"/*.pid; do
		if [[ -f $pid ]]; then
			kill "$(cat "$pid")" 2>/dev/null || true
			rm -f "$pid"
		fi
	done
	rm -f "$dev/model-secrets.toml"
	if vm_running; then
		vm_ssh "rm -f $inside" || true
		"$repo/os/freebsd/scripts/shell-vm.sh" stop >/dev/null 2>&1 || true
	fi
}

up() {
	local model=${1:-claude-haiku-5-5}
	[[ -n ${GLASS_ANTHROPIC_API_KEY:-} ]] || die "GLASS_ANTHROPIC_API_KEY isn't set: add it in this environment's settings"
	[[ $model =~ ^[a-z0-9.-]+$ ]] || die "'$model' isn't a model id"
	vm_running || die "the VM isn't running: make vm-up vm-wait vm-deploy"
	down
	cargo build -q -p glassd -p glassctl -p glass-desktop-agent -p glass-it-agent -p glass-e2e -p glass-app-bridge
	cargo build -q --manifest-path "$repo/shell/Cargo.toml" --target x86_64-unknown-freebsd -p glass-shell-fb
	mkdir -p "$dev" "$run"
	# The other current models beside it, cheapest first (ADR 0044).
	"$bin/glass-e2e" real-model-policy anthropic "$model" --ladder >"$dev/real-policy.toml"
	(umask 077 && printf '[anthropic]\napi_key = "%s"\n' "$GLASS_ANTHROPIC_API_KEY" >"$dev/model-secrets.toml")
	GLASS_DEV_SKIP_BUILD=1 GLASS_DEV_DIR=$dev GLASS_DEV_RUN=$run GLASS_DEV_POLICY=$dev/real-policy.toml \
		GLASS_DEV_MODEL_SECRETS=$dev/model-secrets.toml nohup "$repo/tools/dev/host-run.sh" >"$dev/glassd.log" 2>&1 &
	echo $! >"$dev/glassd.pid"
	export GLASS_CONTROL_SOCKET=$run/control.sock
	for _ in $(seq 300); do
		"$bin/glassctl" status >/dev/null 2>&1 && break
		kill -0 "$(cat "$dev/glassd.pid")" 2>/dev/null || die "glassd stopped; see $dev/glassd.log"
		sleep 0.2
	done
	python3 -I "$repo/tools/dev/seed-workday.py" "$dev/home" "$(id -un)"
	nohup "$bin/glass-app-bridge" --dev --apps "$repo/apps" --socket "$run/apps.sock" >"$dev/apps.log" 2>&1 &
	echo $! >"$dev/apps.pid"

	# A connection of its own (not the shared one vm_ssh reuses), holding the forward open. ssh
	# keeps an option's first value, so ControlPath=none goes before SSH_OPTS' own.
	nohup ssh -o ControlPath=none "${SSH_OPTS[@]}" -o ExitOnForwardFailure=yes -o StreamLocalBindUnlink=yes \
		-p "$VM_SSH_PORT" -N -R "$inside:$run/control.sock" root@127.0.0.1 >"$dev/tunnel.log" 2>&1 &
	echo $! >"$dev/tunnel.pid"
	for _ in $(seq 50); do vm_ssh "test -S $inside" 2>/dev/null && break; sleep 0.2; done
	# Development only: the desktop runs as demo, and this glassd sees everyone as its own user.
	vm_ssh "chmod 666 $inside"
	vm_scp "$repo/shell/target/x86_64-unknown-freebsd/debug/glass-shell-fb" /usr/local/bin/glass-shell-fb.new
	vm_ssh sh -eus -- "$inside" <<'EOF'
socket=$1
mv /usr/local/bin/glass-shell-fb.new /usr/local/bin/glass-shell-fb
service glasshouse_greeter onestatus >/dev/null 2>&1 && service glasshouse_greeter onestop >/dev/null
pkill -x glass-shell-fb && while pgrep -qx glass-shell-fb; do sleep 0.2; done
chown demo /dev/ttyv0 /dev/input/event*
log=/var/log/glass-shell-fb.log
install -m 0644 -o demo /dev/null "$log"
daemon -u demo -o "$log" /usr/local/bin/glass-shell-fb --socket "$socket" </dev/null >/dev/null 2>&1
for _ in $(jot 150); do grep -q "rows copied" "$log" && break; sleep 0.2; done
EOF
	echo "real-desktop: the VM's desktop shows a glassd on $model (GLASS_CONTROL_SOCKET=$run/control.sock)"
	echo "  try: $vm key ctrl-spc; $vm type \"What's on my calendar tomorrow?\"; $vm key ret; $vm screendump shot.png"
	echo "  apps: http://mail.localhost:7300   stop: $0 down"
}

case ${1:-} in
up) up "${2:-}" ;;
down) down ;;
*) die "usage: real-desktop.sh up [model] | down" ;;
esac
