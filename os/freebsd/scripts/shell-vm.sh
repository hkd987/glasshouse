#!/usr/bin/env bash
# Usage: shell-vm.sh start|stop|once [user]
# Puts the desktop shell on the VM's screen (ADR 0016, the first step of M2): copies the
# cross-built glass-shell-fb in and runs it on the first virtual terminal as `user` (default
# demo), drawing that user's live tiles until `stop`. `once` draws one frame and exits.
# See it with `vm.sh screendump`. `make shell-vm` builds the binary and runs `start`.
#
# Development only: the terminal is handed to the user here, where the session started by the
# sign-in screen will own it later.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

cmd=${1:-start}
user=${2:-demo}
bin=$REPO_ROOT/shell/target/x86_64-unknown-freebsd/debug/glass-shell-fb
vm_running || die "VM is not running (vm.sh up && vm.sh wait)"

if [[ $cmd == stop ]]; then
	# SIGTERM: the program puts the terminal back to text on its way out.
	vm_ssh 'pkill -x glass-shell-fb && while pgrep -qx glass-shell-fb; do sleep 0.2; done; true'
	exit
fi
[[ $cmd == start || $cmd == once ]] || die "usage: shell-vm.sh start|stop|once [user]"
[[ -f $bin ]] || die "no ${bin#"$REPO_ROOT"/} (cargo build --manifest-path shell/Cargo.toml" \
	"--target x86_64-unknown-freebsd -p glass-shell-fb)"

vm_scp "$bin" /usr/local/bin/glass-shell-fb.new
vm_ssh sh -eus -- "$cmd" "$user" <<'EOF'
cmd=$1 user=$2
mv /usr/local/bin/glass-shell-fb.new /usr/local/bin/glass-shell-fb
pkill -x glass-shell-fb && while pgrep -qx glass-shell-fb; do sleep 0.2; done
chown "$user" /dev/ttyv0
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
