#!/bin/sh
# Installs host packages on a CI machine: `tools/dev/ci-packages.sh clang lld`.
#
# It tries again when the package servers can't be reached. On 2026-11-11
# two runs in a row failed on every job with "Temporary failure resolving
# 'azure.archive.ubuntu.com'": the machines GitHub runs this on couldn't
# look up their own package mirror for half an hour, nothing was installed,
# and nothing of ours was tested. A short outage is ridden out here; a long
# one still fails, and says that is what it was.
set -eu

tries=${CI_PACKAGE_TRIES:-4}
pause=${CI_PACKAGE_PAUSE:-30}
n=1
while :; do
	# `apt-get update` exits 0 with its lists unread, so the install is
	# what tells whether the servers answered.
	if sudo apt-get update -qq -o Acquire::Retries=3 &&
		sudo apt-get install -y -qq --no-install-recommends -o Acquire::Retries=3 "$@"; then
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
