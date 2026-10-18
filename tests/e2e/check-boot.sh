#!/bin/sh
# A fast boot (docs/dev/performance.md, "Boot"): nothing slow on the way to
# the login screen. Runs in the VM as root, after a deploy, without
# rebooting: it checks the settings the boot budget depends on and times our
# own startup script. tools/boot/timeline.py measures a whole boot.
set -u

fail=0
pass() { echo "PASS $1"; }
flunk() {
	echo "FAIL $1"
	fail=1
}
loader() { grep -qx "$1" /boot/loader.conf; }

if loader 'autoboot_delay="0"'; then pass "boot menu doesn't wait"; else flunk "boot menu waits: autoboot_delay isn't 0"; fi
if loader 'hw.usb.no_boot_wait="1"'; then
	pass "root file system doesn't wait for USB"
else
	flunk "root file system waits for USB: hw.usb.no_boot_wait isn't 1"
fi
if [ "$(sysrc -n devd_flags)" = "-n" ]; then
	pass "devices found at boot are handled in the background"
else
	flunk "startup waits for devd to handle every device found at boot (devd_flags)"
fi
case $(sysrc -n ifconfig_DEFAULT 2>/dev/null) in
*SYNCDHCP*) flunk "startup waits for a DHCP lease (ifconfig_DEFAULT has SYNCDHCP)" ;;
*)
	if [ "$(sysrc -n defaultroute_delay)" = 0 ]; then
		pass "startup doesn't wait for the network"
	else
		flunk "startup waits for DHCP's default route (defaultroute_delay isn't 0)"
	fi
	;;
esac
if grep -q 'rc_fast.*return 0' /usr/local/etc/rc.d/glassd; then
	pass "glassd checks its config once at boot, as it starts"
else
	flunk "rc.d/glassd runs glassd --check at boot, a second glassd on the way to login"
fi
# Our identity script runs on every boot. Report what it takes; fail only on
# a large regression (about 0.7 s a run in the emulated VM, most of it
# rc.subr reading the rc configuration; 1.9 s before it used builtins).
start=$(date +%s)
i=0
while [ $i -lt 5 ]; do
	/usr/local/etc/rc.d/glasshouse_identity faststart >/dev/null 2>&1
	i=$((i + 1))
done
took=$(($(date +%s) - start))
if [ "$took" -le 6 ]; then
	pass "glasshouse_identity: 5 runs in ${took}s"
else
	flunk "glasshouse_identity: 5 runs took ${took}s (budget 6s in the emulated VM)"
fi

exit $fail
