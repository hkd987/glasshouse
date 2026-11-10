#!/usr/bin/env bash
# Usage: check-host-deps.sh [--install]
# Checks the Linux host tools the FreeBSD dev loop needs. Prints the apt-get line for anything
# missing (or runs it with --install). Exits non-zero if something required is missing.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

# command -> Debian/Ubuntu package
declare -A pkg=(
	[qemu-system-x86_64]=qemu-system-x86 [qemu-img]=qemu-utils
	[ssh]=openssh-client [scp]=openssh-client [ssh-keygen]=openssh-client
	[xorriso]=xorriso [xz]=xz-utils [zstd]=zstd [curl]=curl [tar]=tar
	[clang]=clang [ld.lld]=lld [shellcheck]=shellcheck [pkg-config]=pkg-config
	[swtpm]=swtpm # the emulated TPM in tests/hardware/matrix.sh
)
missing=()
for cmd in $(printf '%s\n' "${!pkg[@]}" | sort); do
	if command -v "$cmd" >/dev/null 2>&1; then
		printf '  ok       %s\n' "$cmd"
	else
		printf '  MISSING  %s (apt package %s)\n' "$cmd" "${pkg[$cmd]}"
		missing+=("${pkg[$cmd]}")
	fi
done

# The model gateway links OpenSSL; host builds find it with pkg-config (ADR 0018).
if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists openssl; then
	echo "  ok       OpenSSL headers"
else
	echo "  MISSING  OpenSSL headers (apt package libssl-dev)"
	missing+=(libssl-dev)
fi

if command -v rustup >/dev/null 2>&1; then
	if (cd "$REPO_ROOT" && rustup target list --installed) | grep -qx x86_64-unknown-freebsd; then
		echo "  ok       rust target x86_64-unknown-freebsd"
	else
		echo "  MISSING  rust target (rustup target add x86_64-unknown-freebsd)"
	fi
fi
[[ -e /dev/kvm ]] || echo "  note     no /dev/kvm: QEMU uses TCG emulation (slow boots; see README)"

if ((${#missing[@]})); then
	mapfile -t missing < <(printf '%s\n' "${missing[@]}" | sort -u)
	if [[ ${1:-} == --install ]]; then
		apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${missing[@]}"
	else
		die "install with: apt-get install -y ${missing[*]}"
	fi
fi
