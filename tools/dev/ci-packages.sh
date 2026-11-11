#!/bin/sh
# Installs host packages on a CI machine: `tools/dev/ci-packages.sh clang lld`.
#
# It rides out package servers whose names can't be looked up. On
# 2026-11-11, for more than two and a half hours, runs failed on every job
# with "Temporary failure resolving 'azure.archive.ubuntu.com'": the
# machines GitHub runs this on couldn't look up any of their three package
# mirrors (their cloud's, then archive.ubuntu.com and security.ubuntu.com),
# nothing was installed, and nothing of ours was tested. From another
# network the same names were looked up some of the time and not the rest,
# while other sites' names always were: the trouble was with the names, not
# with GitHub's machines, and it came and went.
#
# So, when a mirror's name can't be looked up:
# - the names are asked for again, for up to eight minutes, and the package
#   tool isn't run until one answers: with none it takes minutes to fail,
#   and can't do anything else;
# - an answer is kept for the job (in /etc/hosts): the package tool looks a
#   name up many times, and each of those would have to be lucky;
# - the mirrors still without an answer are left out for that try, while
#   another has one. Any one of them holds every package.
# When every name is looked up the first time, as on any other day, nothing
# is changed and the package tool runs as it always did.
#
# The package tool isn't asked to try again itself: with the servers away
# one attempt already takes it minutes, and the job's step has a time limit
# (ci.yml). This gives up by itself before that, and says why.
set -eu

tries=${CI_PACKAGE_TRIES:-3}
pause=${CI_PACKAGE_PAUSE:-20}
# How long, from the start, before no new try or lookup is begun. The
# step's limit is fifteen minutes; a try begun just inside this can take a
# minute to ask for the names and four and a half to fail.
give_up_after=${CI_PACKAGE_GIVE_UP_AFTER:-480}
# Between two rounds of asking for the names.
lookup_pause=${CI_PACKAGE_LOOKUP_PAUSE:-5}
# The machine's package mirrors, first choice first. GitHub's machines list
# their cloud's own mirror, then Ubuntu's.
mirrors=${CI_PACKAGE_MIRRORS:-/etc/apt/apt-mirrors.txt}
hosts=${CI_PACKAGE_HOSTS:-/etc/hosts}

started=$(date +%s)
# Set once a name wasn't looked up or an install failed: from then on
# answers are kept and mirrors without one left out.
trouble=""

out_of_time() {
	[ $(($(date +%s) - started)) -ge "$give_up_after" ]
}

# The mirrors' names, one a line, from the list as the machine came with it
# (kept beside it once this changes it: a try starts from all of them).
mirror_names() {
	list=$mirrors
	if [ -f "$mirrors.all" ]; then
		list=$mirrors.all
	fi
	[ -f "$list" ] || return 0
	sed -n 's|^[a-z+]*://\([^/:[:space:]]*\).*|\1|p' "$list"
}

# A name's address, or nothing. A name kept in the hosts file is answered
# from there at once.
address_of() {
	timeout 20 getent ahostsv4 "$1" 2>/dev/null | awk '{ print $1; exit }'
}

# Finds a mirror the package tool can reach by name. Fails when none of
# their names was looked up before the time was up; changes nothing when
# all of them were at the first asking and there has been no trouble.
find_mirrors() {
	names=$(mirror_names)
	[ -n "$names" ] || return 0
	found=""
	answers=""
	while :; do
		lost=""
		for name in $names; do
			case " $found " in *" $name "*) continue ;; esac
			address=$(address_of "$name")
			if [ -n "$address" ]; then
				found="$found $name"
				answers="$answers$address $name
"
			else
				lost="$lost $name"
			fi
		done
		[ -z "$lost" ] && break
		if [ -z "$trouble" ]; then
			trouble=yes
			echo "ci-packages: can't look up$lost; asking again, and keeping what answers" >&2
		fi
		# One mirror is enough.
		[ -n "$found" ] && break
		if out_of_time; then
			return 1
		fi
		sleep "$lookup_pause"
	done
	[ -n "$trouble" ] || return 0
	[ -f "$mirrors.all" ] || sudo cp "$mirrors" "$mirrors.all"
	printf '%s' "$answers" | while read -r address name; do
		grep -qE "^[0-9.]+[[:space:]]+$name([[:space:]]|\$)" "$hosts" 2>/dev/null && continue
		echo "ci-packages: $name is $address for this job" >&2
		echo "$address $name" | sudo tee -a "$hosts" >/dev/null
	done
	sudo cp "$mirrors.all" "$mirrors"
	for name in $lost; do
		echo "ci-packages: leaving $name out for this try" >&2
		sudo sed -i "\\|://$(printf '%s' "$name" | sed 's/\./\\./g')[/:]|d" "$mirrors"
	done
}

n=0
while :; do
	if ! find_mirrors; then
		echo "ci-packages: none of the package servers' names could be looked up, $give_up_after s in:" >&2
		echo "ci-packages: $(mirror_names | tr '\n' ' ')" >&2
		echo "ci-packages: nothing was installed; run the job again later" >&2
		exit 1
	fi
	n=$((n + 1))
	# `apt-get update` exits 0 with its lists unread, so the install is
	# what tells whether the servers answered.
	if sudo apt-get update -qq &&
		sudo apt-get install -y -qq --no-install-recommends "$@"; then
		exit 0
	fi
	trouble=yes
	if [ "$n" -ge "$tries" ] || out_of_time; then
		echo "ci-packages: couldn't install $* in $n tries; if the errors above are about" >&2
		echo "ci-packages: reaching or resolving a package server, run the job again later" >&2
		exit 1
	fi
	echo "ci-packages: trying again in $pause s ($((n + 1)) of $tries)" >&2
	sleep "$pause"
done
