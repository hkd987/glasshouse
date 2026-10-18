#!/bin/bash
# SessionStart hook for Claude Code cloud sessions: installs what `make test
# lint` needs, plus the FreeBSD VM tooling, so a fresh container is ready to
# work. Idempotent and quick when everything is already there (the container
# is cached after the first run). The FreeBSD images themselves (~4 GB) are
# fetched on demand by `make test-env`, not here.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
	exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/../..}"

# Host packages: compilers and linker for FreeBSD cross-builds, shellcheck
# and python3 for `make lint`, QEMU and friends for the dev VM (ovmf for its
# UEFI mode), Pillow for screenshots and the boot art, and OpenSSL's headers
# for glassd's model gateway on the host (ADR 0018).
packages=(clang lld shellcheck python3 python3-pil qemu-system-x86 qemu-utils ovmf xorriso openssh-client xz-utils curl libssl-dev pkg-config)
missing=()
for pkg in "${packages[@]}"; do
	dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q "install ok installed" || missing+=("$pkg")
done
if [ ${#missing[@]} -gt 0 ]; then
	echo "session-start: installing ${missing[*]}"
	SUDO=""
	[ "$(id -u)" -eq 0 ] || SUDO="sudo"
	$SUDO apt-get update -qq
	DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq --no-install-recommends "${missing[@]}" >/dev/null
fi

# The pinned toolchain, components and the FreeBSD target from
# rust-toolchain.toml.
rustup toolchain install >/dev/null 2>&1 || rustup show >/dev/null
rustup target list --installed | grep -qx x86_64-unknown-freebsd || rustup target add x86_64-unknown-freebsd

# Crates, so the first build doesn't wait on the network.
cargo fetch --locked >/dev/null

echo "session-start: ready (make test lint; make test-env for the FreeBSD VM)"
