#!/bin/sh
#
# Runs after every deploy and image bake. Must stay idempotent.
#
# - creates the daemon's state and run directories with the right modes;
# - enables glassd and the sign-in screen at boot;
# - brands the system: no base-system name where users look (BR-1);
# - seeds demo data for the `demo` user, if that user exists and has none.

set -eu

install -d -o root -g wheel -m 0700 /var/db/glasshouse
install -d -o root -g wheel -m 0755 /var/run/glasshouse

sysrc -q glassd_enable=YES >/dev/null
# The machine boots into the sign-in screen (ADR 0019).
sysrc -q glasshouse_greeter_enable=YES >/dev/null

# loader_tunable <name> <value>: a dotted loader tunable, which sysrc can't name.
loader_tunable() {
	if ! grep -qx "$1=\"$2\"" /boot/loader.conf; then
		sed -i '' -e "/^$1=/d" /boot/loader.conf
		echo "$1=\"$2\"" >>/boot/loader.conf
	fi
}

# BR-1. The identity comes from rc.d/glasshouse_identity and the login
# message from our branding; the base writers they replace are turned off.
branding=/usr/local/share/glasshouse/branding
sysrc -q osrelease_enable=NO update_motd=NO glasshouse_identity_enable=YES >/dev/null
if [ "$(readlink /etc/motd)" != "$branding/motd" ]; then
	ln -sfh "$branding/motd" /etc/motd
fi
if [ "$(sysrc -n hostname 2>/dev/null)" = freebsd ]; then
	sysrc -q hostname=glasshouse >/dev/null
fi
# Boot: our brand and logo on the menu (the art comes from
# tools/branding/art.py), then a quiet kernel and rc behind our splash. The
# base system's defaults would title the menu and draw its own splash.
# The menu doesn't wait (autoboot_delay=0): holding a key other than Enter
# as the machine starts opens it. To debug boot, set boot_mute=NO in
# /boot/loader.conf and reboot.
install -C -o root -g wheel -m 0444 "$branding/gfx-glasshouse.lua" /boot/lua/gfx-glasshouse.lua
install -C -o root -g wheel -m 0444 "$branding/splash.png" /boot/images/glasshouse-splash.png
install -C -o root -g wheel -m 0444 "$branding/logo.png" /boot/images/glasshouse-logo.png
# (rc.d/glasshouse_identity sets the console for this machine's firmware.)
# The framebuffer console runs at the screen size the shell draws for (ADR 0016).
sysrc -q -f /boot/loader.conf loader_brand=glasshouse loader_logo=glasshouse \
	loader_menu_title="Welcome to Glasshouse" autoboot_delay=0 beastie_disable=NO \
	boot_mute=YES efi_max_resolution=1280x800 vbe_max_resolution=1280x800 \
	splash=/boot/images/glasshouse-splash.png \
	shutdown_splash=/boot/images/glasshouse-splash.png >/dev/null
# A fast boot (docs/dev/performance.md, "Boot"): the root file system is on
# an internal disk, so don't wait for USB to finish probing before mounting
# it (a USB-booted copy still finds its root: the kernel keeps trying for
# ten seconds); devd handles the devices found at boot in the background
# instead of holding up the rest of startup; and the network comes up
# without startup waiting for a DHCP lease or the default route it brings.
# Nothing in startup needs the network: managed settings load from local
# disk (ADR 0021).
loader_tunable hw.usb.no_boot_wait 1
loader_tunable vfs.mountroot.timeout 10
sysrc -q devd_flags="-n" defaultroute_delay=0 >/dev/null
case $(sysrc -n ifconfig_DEFAULT 2>/dev/null) in
*SYNCDHCP*) sysrc -q ifconfig_DEFAULT="$(sysrc -n ifconfig_DEFAULT | sed 's/SYNCDHCP/DHCP/')" >/dev/null ;;
esac
sysrc -q rc_startmsgs=NO >/dev/null
service glasshouse_identity start >/dev/null
# Space ZFS frees goes back to the disk as it's freed: SSDs keep their write
# speed, and a VM's sparse disk doesn't grow with every file rewritten (busy
# agents in the e2e suite rewrite hundreds of megabytes).
if zpool list zroot >/dev/null 2>&1; then
	zpool set autotrim=on zroot
fi
# Console login: the full mark before the prompt, and a banner that names the
# product, not the base system (%s).
install -C -o root -g wheel -m 0644 "$branding/issue" /etc/issue
sed -i '' -e 's|:im=\\r\\n%s/%m (%h) (%t)|:im=\\r\\nGlasshouse (%h) (%t)|' /etc/gettytab
# No base-system tips at login, for new accounts and existing ones.
for f in /usr/share/skel/dot.profile /usr/share/skel/dot.login /home/*/.profile /home/*/.login; do
	[ -f "$f" ] && sed -i '' -e '/^[^#].*fortune freebsd-tips/s/^/# /' "$f"
done
# SSH doesn't announce the base system in its version string. The config
# is only replaced once sshd accepts it, so a bad edit can't lock us out.
if ! grep -q '^VersionAddendum' /etc/ssh/sshd_config; then
	conf=$(mktemp /etc/ssh/sshd_config.XXXXXX)
	cat /etc/ssh/sshd_config >"$conf"
	echo 'VersionAddendum none' >>"$conf"
	if sshd -t -f "$conf"; then
		chmod 0644 "$conf"
		mv "$conf" /etc/ssh/sshd_config
		service sshd reload >/dev/null
	else
		rm -f "$conf"
	fi
fi

seed=/usr/local/share/glasshouse/demo
if pw usershow demo >/dev/null 2>&1; then
	home=$(pw usershow demo | cut -d: -f9)
	if [ -d "$home" ] && [ ! -d "$home/Invoices" ]; then
		install -d -o demo -g demo -m 0755 "$home/Invoices" "$home/Reports" "$home/Finance"
		for f in "$seed"/Invoices/*.txt; do
			install -o demo -g demo -m 0644 "$f" "$home/Invoices/"
		done
		printf 'quarter,revenue\nQ3,1250000\n' > "$home/Finance/q3-board.csv"
		chown demo:demo "$home/Finance/q3-board.csv"
		echo "post-install: seeded demo data in $home"
	fi
fi
