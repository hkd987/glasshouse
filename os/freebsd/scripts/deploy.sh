#!/usr/bin/env bash
# Usage: deploy.sh
# Installs the staging root build/dist/freebsd/ into the running VM at /, then runs
# /usr/local/libexec/glasshouse/post-install.sh (if present) and restarts glassd (if it has an
# rc.d script and glassd_enable=YES). Idempotent: files are replaced atomically per file, and
# files removed from the payload since the previous deploy are deleted from the guest.
# Only files and symlinks are shipped; new directories are created 0755 root:wheel, so set any
# special directory modes in post-install.sh.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"
need tar ssh scp

[[ -d $DIST_DIR ]] || die "nothing staged at ${DIST_DIR#"$REPO_ROOT"/} (run stage-dist.sh)"
vm_running || die "VM is not running (vm.sh up && vm.sh wait)"

tarball=$BUILD_DIR/dist/freebsd.tar
(cd "$DIST_DIR" && find . ! -type d -print0 | LC_ALL=C sort -z |
	tar --null --no-recursion --owner=0 --group=0 --numeric-owner -cf "$tarball" -T -)
log "deploying $(tar -tf "$tarball" | wc -l) files ($(du -h "$tarball" | cut -f1))"
vm_scp "$tarball" /var/tmp/glasshouse-dist.tar

vm_ssh 'sh -eus' <<'EOF'
tarball=/var/tmp/glasshouse-dist.tar
state=/var/db/glasshouse-deploy.files
new=$(mktemp)
tar -tf "$tarball" | sed 's|^\./|/|' | LC_ALL=C sort >"$new"
tar -xpf "$tarball" -C /
if [ -f "$state" ]; then
	LC_ALL=C comm -23 "$state" "$new" | while IFS= read -r f; do
		echo "deploy: removing stale $f"
		rm -f "$f"
	done
fi
mv "$new" "$state"
rm -f "$tarball"

# Every program and library we ship finds the libraries it links: one the system lacks shows
# here, not when someone signs in (images/desktop.third-party lists those from packages).
unlinked=$(grep -E '^/usr/local/(s?bin|lib|libexec/glasshouse)/' "$state" | while IFS= read -r f; do
	[ -f "$f" ] && [ ! -L "$f" ] || continue
	[ "$(head -c 4 "$f" | od -An -c | tr -d ' ')" = 177ELF ] || continue
	ldd "$f" 2>/dev/null | grep 'not found' | sed "s|^|$f: |"
done || true)
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
