#!/bin/sh
# Installs host packages on a CI machine: `tools/dev/ci-packages.sh clang lld`.
#
# It tries again when the package servers can't be reached. On 2026-11-11
# runs failed on every job with "Temporary failure resolving
# 'azure.archive.ubuntu.com'": the machines GitHub runs this on couldn't
# look up their own package mirror for over an hour, nothing was installed,
# and nothing of ours was tested. A short outage is ridden out here; a long
# one still fails, and says that is what it was.
#
# The package tool isn't asked to try again itself: with the servers away
# one attempt already takes it minutes, and the job's step has a time limit
# (ci.yml) so a long outage fails in a quarter of an hour, not an hour.
set -eu

tries=${CI_PACKAGE_TRIES:-3}
pause=${CI_PACKAGE_PAUSE:-20}
# The machine's package mirrors, first choice first. GitHub's machines list
# their cloud's own mirror, then Ubuntu's.
mirrors=${CI_PACKAGE_MIRRORS:-/etc/apt/apt-mirrors.txt}

# Takes a first-choice mirror that can't be looked up out of the list,
# while there is another. The package tool doesn't go on to the next by
# itself: through that outage every download failed on the first mirror's
# name, Ubuntu's own servers answering all the while.
skip_lost_mirror() {
	[ -f "$mirrors" ] || return 0
	while [ "$(grep -c . "$mirrors")" -gt 1 ]; do
		first=$(sed -n '1s|^[a-z+]*://\([^/[:space:]]*\).*|\1|p' "$mirrors")
		[ -n "$first" ] || return 0
		if getent hosts "$first" >/dev/null 2>&1; then
			return 0
		fi
		echo "ci-packages: $first can't be looked up; using the machine's other mirrors" >&2
		sudo sed -i '1d' "$mirrors"
	done
}

n=1
while :; do
	skip_lost_mirror
	# `apt-get update` exits 0 with its lists unread, so the install is
	# what tells whether the servers answered.
	if sudo apt-get update -qq &&
		sudo apt-get install -y -qq --no-install-recommends "$@"; then
		exit 0
	fi
	if [ "$n" -ge "$tries" ]; then
		echo "ci-packages: couldn't install $* in $tries tries; if the errors above are about" >&2
		echo "ci-packages: reaching or resolving a package server, run the job again later" >&2
		exit 1
	fi
	n=$((n + 1))
	echo "ci-packages: trying again in $pause s ($n of $tries)" >&2
	sleep "$pause"
done
