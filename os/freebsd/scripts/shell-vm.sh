#!/usr/bin/env bash
# Usage: shell-vm.sh start|stop|once [user]
#        shell-vm.sh greeter|greeter-off
# Puts the desktop shell on the VM's screen (ADR 0016): copies the cross-built glass-shell-fb in
# and runs it on the first virtual terminal as `user` (default demo), with that user's live
# tiles and the VM's keyboard and pointer, until `stop`. `once` draws one frame and exits.
# See it with `vm.sh screendump`; drive it with `vm.sh click`, `vm.sh key` and `vm.sh type`.
# `make shell-vm` builds the binary and runs `start`.
#
# `greeter` installs the sign-in screen too (glass-greeter, ADR 0019) and turns it on, as a
# machine boots: sign in as demo (password demo on development VMs) and the desktop starts;
# Shift Super Q (`vm.sh key shift-meta_l-q`) signs out. `greeter-off` gives ttyv0 back to the
# console. `make greeter-vm` builds both and runs `greeter`.
#
# Development only: `start` hands the terminal and input devices to the user directly, where
# the greeter does it for a real sign-in.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

cmd=${1:-start}
user=${2:-demo}
bins=$REPO_ROOT/shell/target/x86_64-unknown-freebsd/debug
vm_running || die "VM is not running (vm.sh up && vm.sh wait)"

need_bin() {
	[[ -f $bins/$1 ]] || die "no ${bins#"$REPO_ROOT"/}/$1 (cargo build --manifest-path" \
		"shell/Cargo.toml --target x86_64-unknown-freebsd -p $1)"
}

case $cmd in
stop)
	# SIGTERM: the program puts the terminal back to text on its way out.
	vm_ssh 'pkill -x glass-shell-fb && while pgrep -qx glass-shell-fb; do sleep 0.2; done; true'
	exit
	;;
greeter-off)
	# Disabled first, so stopping gives ttyv0 back to getty.
	vm_ssh 'sysrc -q glasshouse_greeter_enable=NO >/dev/null; service glasshouse_greeter onestop >/dev/null 2>&1
		pkill -x glass-shell-fb; true'
	exit
	;;
greeter)
	need_bin glass-shell-fb
	need_bin glass-greeter
	vm_scp "$bins/glass-shell-fb" "$bins/glass-greeter" /var/tmp/
	vm_ssh sh -eus <<'EOF'
for b in glass-shell-fb glass-greeter; do
	install -m 0755 "/var/tmp/$b" "/usr/local/bin/$b"
	rm -f "/var/tmp/$b"
done
pkill -x glass-shell-fb || true
sysrc -q glasshouse_greeter_enable=YES >/dev/null
service glasshouse_greeter restart
EOF
	exit
	;;
start | once) ;;
*) die "usage: shell-vm.sh start|stop|once [user] | greeter|greeter-off" ;;
esac

need_bin glass-shell-fb
vm_scp "$bins/glass-shell-fb" /usr/local/bin/glass-shell-fb.new
vm_ssh sh -eus -- "$cmd" "$user" <<'EOF'
cmd=$1 user=$2
mv /usr/local/bin/glass-shell-fb.new /usr/local/bin/glass-shell-fb
# The sign-in screen owns the terminal when it's on; this runs the desktop directly instead.
service glasshouse_greeter onestatus >/dev/null 2>&1 && service glasshouse_greeter onestop >/dev/null
pkill -x glass-shell-fb && while pgrep -qx glass-shell-fb; do sleep 0.2; done
chown "$user" /dev/ttyv0 /dev/input/event*
log=/var/log/glass-shell-fb.log
install -m 0644 -o "$user" /dev/null "$log"
if [ "$cmd" = once ]; then
	su -m "$user" -c "/usr/local/bin/glass-shell-fb --once" 2>&1
else
	# Its own stdio on /dev/null, or it would hold the ssh channel open.
	daemon -u "$user" -o "$log" /usr/local/bin/glass-shell-fb </dev/null >/dev/null 2>&1
	# The first frame is drawn once the tiles are in.
	for _ in $(jot 100); do grep -q "rows copied" "$log" && break; sleep 0.2; done
	cat "$log"
fi
EOF
