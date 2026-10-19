#!/bin/sh
# Usage: pkgbase-root.sh <disk> <repository-url> <package-list>
# Runs as root inside a throwaway build VM (bake-pkgbase.sh copies it in): partitions <disk>,
# makes a new ZFS pool on it, installs exactly the packages of <package-list> (an image's .pkgs
# file) from the repository at <repository-url> into that fresh root with `pkg --rootdir`, and
# makes it bootable (UEFI and BIOS). The pool is named zroot on disk but imported here as
# glassimage, beside the build VM's own zroot. ADR 0022 item 1.
#
# Writes the image's package inventory (BS-6) to /usr/local/share/glasshouse/packages.txt in the
# image, and prints it on stdout after a line "inventory:". Everything else goes to stderr.
set -eu

disk=${1:?usage: pkgbase-root.sh <disk> <repository-url> <package-list>}
url=${2:?}
list=${3:?}
pool=zroot
tmpname=glassimage
root=/mnt/glassimage
work=/tmp/glassimage
major=$(uname -U | cut -c1-2)
abi="FreeBSD:$major:$(uname -p)"
osversion=$(uname -U)

log() { echo "[pkgbase-root] $*" >&2; }
die() {
	log "error: $*"
	exit 1
}

[ -c "/dev/$disk" ] || die "no disk /dev/$disk"
zpool list "$tmpname" >/dev/null 2>&1 && die "a pool $tmpname is already imported"

# The list: one package per line, '#' comments.
mkdir -p "$work"
sed -e 's/#.*//' -e 's/[[:space:]]//g' -e '/^$/d' "$list" | sort -u >"$work/want"
[ -s "$work/want" ] || die "empty package list $list"

log "partitioning $disk"
gpart destroy -F "$disk" >/dev/null 2>&1 || true
gpart create -s gpt "$disk" >/dev/null
# EFI system partition, BIOS boot code, swap, then the pool last so growfs(8)
# can grow it into a larger disk on first boot. No partition or file names
# the base system (BR-1: firmware boot menus show them).
gpart add -t efi -l efiboot0 -a 1m -s 64m "$disk" >/dev/null
gpart add -t freebsd-boot -l bootfs -a 4k -s 512k "$disk" >/dev/null
gpart add -t freebsd-swap -l swapfs -a 1m -s 1g "$disk" >/dev/null
gpart add -t freebsd-zfs -l rootfs -a 1m "$disk" >/dev/null
newfs_msdos -F 32 -c 1 -L EFISYS "/dev/${disk}p1" >/dev/null 2>&1

log "creating pool $pool (as $tmpname) on ${disk}p4"
sysctl -q vfs.zfs.vdev.min_auto_ashift=12 >/dev/null
# As an installed system has it (bsdinstall's defaults): compression on,
# no access times.
zpool create -f -o altroot="$root" -o cachefile=none -o autotrim=on \
	-O compression=on -O atime=off -O mountpoint=none -t "$tmpname" "$pool" "/dev/${disk}p4"
zfs create -o mountpoint=none "$tmpname/ROOT"
zfs create -o mountpoint=/ -o canmount=noauto "$tmpname/ROOT/default"
zfs mount "$tmpname/ROOT/default"
zfs create -o mountpoint=/home "$tmpname/home"
zfs create -o mountpoint=/tmp -o exec=on -o setuid=off "$tmpname/tmp"
zfs create -o mountpoint=/var -o canmount=off "$tmpname/var"
zfs create -o exec=off -o setuid=off "$tmpname/var/audit"
zfs create -o exec=off -o setuid=off "$tmpname/var/crash"
zfs create -o exec=off -o setuid=off "$tmpname/var/log"
zfs create -o atime=on "$tmpname/var/mail"
zfs create -o setuid=off "$tmpname/var/tmp"
zpool set bootfs="$tmpname/ROOT/default" "$tmpname"
chmod 1777 "$root/tmp" "$root/var/tmp"

# pkg reads the repository's signing keys inside the new root; they're the
# build VM's, which runs the same release.
mkdir -p "$root/usr/share/keys"
cp -Rp "/usr/share/keys/pkgbase-$major" "$root/usr/share/keys/"
mkdir -p "$work/repos" "$work/cache"
cat >"$work/repos/base.conf" <<EOF
FreeBSD-base: {
  url: "$url",
  signature_type: "fingerprints",
  fingerprints: "/usr/share/keys/pkgbase-$major",
  enabled: yes
}
EOF
# Downloads go to memory, not the image.
mount -t tmpfs -o size=2g tmpfs "$work/cache"
pkg_r() {
	pkg --rootdir "$root" -o REPOS_DIR="$work/repos" -o PKG_CACHEDIR="$work/cache" \
		-o ABI="$abi" -o OSVERSION="$osversion" -o ASSUME_ALWAYS_YES=yes "$@"
}

log "installing $(wc -l <"$work/want" | tr -d ' ') packages from $url"
pkg_r update -f -r FreeBSD-base >&2
# shellcheck disable=SC2046  # one argument per package
pkg_r install -r FreeBSD-base $(cat "$work/want") >&2

# Exactly the list: a dependency it doesn't name fails the build, so the
# list stays the whole truth about the image.
pkg_r query '%n' | sort >"$work/have"
extra=$(comm -13 "$work/want" "$work/have" | tr '\n' ' ')
missing=$(comm -23 "$work/want" "$work/have" | tr '\n' ' ')
[ -z "$missing" ] || die "not installed: $missing"
[ -z "$extra" ] || die "installed but not in $list (add them, or drop what needs them): $extra"

log "configuring"
# The image is updated as a whole (ADR 0022, item 5): no repository
# catalog or downloads stay behind.
rm -rf "$root/var/db/pkg/repos" "$root/var/cache/pkg"
umount "$work/cache"

cat >"$root/etc/rc.conf" <<'EOF'
hostname="glasshouse"
zfs_enable="YES"
# Copies of one image share a pool id: each takes a new one on first boot.
zpool_reguid="zroot"
growfs_enable="YES"
dumpdev="AUTO"
ifconfig_DEFAULT="DHCP accept_rtadv"
# Development images: SSH (root with the dev key) and first-boot provisioning
# from the VM's seed (os/freebsd/vm/user-data.in). Not for a product image.
sshd_enable="YES"
sshd_rsa_enable="NO"
nuageinit_enable="YES"
EOF
cat >"$root/boot/loader.conf" <<'EOF'
zfs_load="YES"
kern.geom.label.disk_ident.enable="0"
console="comconsole,vidconsole"
EOF
cat >"$root/etc/fstab" <<'EOF'
# Device		Mountpoint	FStype	Options	Dump	Pass#
/dev/gpt/swapfs		none		swap	sw	0	0
/dev/gpt/efiboot0	/boot/efi	msdosfs	rw	2	2
EOF
mkdir -p "$root/boot/efi"
# First boot: grow into the disk, new pool id, provisioning.
touch "$root/firstboot"

# Databases the package scripts may not have built in a fresh root.
[ -f "$root/etc/pwd.db" ] || pwd_mkdb -d "$root/etc" -p "$root/etc/master.passwd"
[ -f "$root/etc/login.conf.db" ] || cap_mkdb "$root/etc/login.conf"
[ -f "$root/boot/kernel/linker.hints" ] || kldxref "$root/boot/kernel"

# Nothing in the image may be missing a library it links.
log "checking every program's libraries"
# shellcheck disable=SC2016  # the image's own shell expands these
chroot "$root" /bin/sh -c '
	find /bin /sbin /lib /libexec /usr/bin /usr/sbin /usr/lib /usr/libexec /usr/local -type f \
		\( -perm -u+x -o -name "*.so*" \) 2>/dev/null | while read -r f; do
		case $(head -c 4 "$f" 2>/dev/null | od -An -c | tr -d " ") in
		177ELF) ldd "$f" 2>/dev/null | grep "not found" | sed "s|^|$f: |" ;;
		esac
	done' >"$work/unlinked"
if [ -s "$work/unlinked" ]; then
	cat "$work/unlinked" >&2
	die "programs in the image are missing libraries (above): add the packages that provide them"
fi

log "boot code"
gpart bootcode -b "$root/boot/pmbr" -p "$root/boot/gptzfsboot" -i 2 "$disk" >/dev/null
mkdir -p "$work/esp"
mount -t msdosfs "/dev/${disk}p1" "$work/esp"
mkdir -p "$work/esp/EFI/BOOT"
cp "$root/boot/loader.efi" "$work/esp/EFI/BOOT/BOOTX64.EFI"
umount "$work/esp"

# BS-6: what the image is made of, by name and version.
pkg_r query '%n %v' | sort >"$work/packages.txt"
install -d -m 0755 "$root/usr/local/share/glasshouse"
install -m 0600 "$work/packages.txt" "$root/usr/local/share/glasshouse/packages.txt"

used=$(zfs list -Hpo used "$tmpname")
log "trimming and exporting ($((used / 1048576)) MiB used)"
zpool trim -w "$tmpname"
zpool export "$tmpname"
echo "inventory:"
cat "$work/packages.txt"
