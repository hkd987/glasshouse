#!/bin/sh
#
# Runs after every deploy and image bake. Must stay idempotent.
#
# - creates the daemon's state and run directories with the right modes;
# - enables glassd at boot;
# - seeds demo data for the `demo` user, if that user exists and has none.

set -eu

install -d -o root -g wheel -m 0700 /var/db/glasshouse
install -d -o root -g wheel -m 0755 /var/run/glasshouse

sysrc -q glassd_enable=YES >/dev/null

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
