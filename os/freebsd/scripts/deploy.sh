#!/usr/bin/env bash
# Usage: deploy.sh
# Installs the staging root build/dist/freebsd/ into the running VM at /, then runs
# /usr/local/libexec/glasshouse/post-install.sh (if present) and restarts glassd (if it has an
# rc.d script and glassd_enable=YES). Idempotent: files are replaced atomically per file, and
# files removed from the payload since the previous deploy are deleted from the guest. Only files
# whose size, time or link target changed since the last deploy are sent (the guest keeps the
# list), so the browser engine's few hundred megabytes go once, not every time.
# Only files and symlinks are shipped; new directories are created 0755 root:wheel, so set any
# special directory modes in post-install.sh.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"
need tar ssh scp

[[ -d $DIST_DIR ]] || die "nothing staged at ${DIST_DIR#"$REPO_ROOT"/} (run stage-dist.sh)"
vm_running || die "VM is not running (vm.sh up && vm.sh wait)"

# What's staged: one line per file, `/path<TAB>size<TAB>mtime<TAB>link target`.
work=$BUILD_DIR/dist/deploy
rm -rf "$work"
mkdir -p "$work"
(cd "$DIST_DIR" && find . ! -type d -printf '/%P\t%s\t%T@\t%l\n') | LC_ALL=C sort >"$work/staged"
# What the guest got last time, in the same form (a guest from before this kept paths only).
vm_ssh 'cat /var/db/glasshouse-deploy.state 2>/dev/null || true' | LC_ALL=C sort >"$work/deployed"
LC_ALL=C comm -23 "$work/staged" "$work/deployed" | cut -f1 >"$work/send"
cut -f1 "$work/staged" >"$work/paths"
tarball=$BUILD_DIR/dist/freebsd.tar
(cd "$DIST_DIR" && sed 's|^/|./|' "$work/send" | tr '\n' '\0' |
	tar --null --no-recursion --owner=0 --group=0 --numeric-owner -cf "$tarball" -T -)
log "deploying $(wc -l <"$work/send") of $(wc -l <"$work/paths") files ($(du -h "$tarball" | cut -f1))"
vm_scp "$tarball" /var/tmp/glasshouse-dist.tar
vm_scp "$work/staged" /var/tmp/glasshouse-deploy.state
rm -f "$tarball"

vm_ssh 'sh -eus' <<'EOF'
tarball=/var/tmp/glasshouse-dist.tar
state=/var/db/glasshouse-deploy.files
tar -xpf "$tarball" -C /
sent=$(mktemp)
tar -tf "$tarball" | sed 's|^\./|/|' >"$sent"
new=$(mktemp)
cut -f1 /var/tmp/glasshouse-deploy.state | LC_ALL=C sort >"$new"
stale=$(mktemp)
if [ -f "$state" ]; then
	LC_ALL=C comm -23 "$state" "$new" >"$stale"
fi
while IFS= read -r f; do
	echo "deploy: removing stale $f"
	rm -f "$f"
done <"$stale"
mv "$new" "$state"
mv /var/tmp/glasshouse-deploy.state /var/db/glasshouse-deploy.state
rm -f "$tarball"

# Every program and library we ship finds the libraries it links: one the system lacks shows
# here, not when someone signs in (images/*.third-party list those from packages). What was just
# sent is checked; everything is when something was removed, which could be what one links.
if [ -s "$stale" ]; then cp "$state" "$sent"; fi
rm -f "$stale"
unlinked=$(grep -E '^/usr/local/(s?bin|lib|libexec/glasshouse)/' "$sent" | while IFS= read -r f; do
	[ -f "$f" ] && [ ! -L "$f" ] || continue
	[ "$(head -c 4 "$f" | od -An -c | tr -d ' ')" = 177ELF ] || continue
	ldd "$f" 2>/dev/null | grep 'not found' | sed "s|^|$f: |"
done || true)
rm -f "$sent"
if [ -n "$unlinked" ]; then
	echo "$unlinked"
	echo "deploy: shipped programs are missing libraries (above)" >&2
	exit 1
fi

# Hooks get stdio on a temp file so daemons they start don't inherit (and pin open) the ssh channel.
hook() {
	out=$(mktemp)
	rc=0
	"$@" </dev/null >"$out" 2>&1 || rc=$?
	cat "$out"
	rm -f "$out"
	return "$rc"
}
post=/usr/local/libexec/glasshouse/post-install.sh
if [ -f "$post" ]; then
	echo "deploy: running $post"
	if [ -x "$post" ]; then hook "$post"; else hook sh "$post"; fi
fi
if [ -f /usr/local/etc/rc.d/glassd ]; then
	if service glassd enabled; then
		# A glassd started before it had a supervisor that starts it again
		# (ADR 0055) isn't the one the new rc script tracks: stop it first.
		if [ ! -f /var/run/glassd_supervisor.pid ] && pgrep -qx glassd; then
			pkill -TERM -x glassd
			for _ in $(jot 120); do pgrep -qx glassd || break; sleep 0.5; done
		fi
		hook service glassd restart
	else
		echo "deploy: glassd_enable is not YES; not restarting glassd"
	fi
fi
# The sign-in screen, on its new binaries (signing out anyone signed in).
if [ -f /usr/local/etc/rc.d/glasshouse_greeter ] && service glasshouse_greeter enabled; then
	hook service glasshouse_greeter restart
fi
EOF
log "ok"
