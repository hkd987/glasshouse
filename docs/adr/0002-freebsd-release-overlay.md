# 0002. Build on unmodified FreeBSD 15.1 releases plus our overlay

## Context
PA-5: the base is upstream FreeBSD plus our layers, tracking releases within
a quarter. We need a fast, reproducible way to build and test the OS on
ordinary Linux CI and laptops, without KVM.

## Decision
- Pin a FreeBSD release (`os/freebsd/release.env`, SHA-256 verified):
  currently 15.1-RELEASE, the ZFS-root cloud-init VM image.
- Our software is cross-compiled from Linux against a sysroot extracted from
  the release's `base.txz`, and installed as an overlay
  (`os/freebsd/overlay` + binaries from `dist.manifest`) by an idempotent
  `post-install.sh`.
- Development and end-to-end tests run in QEMU (`os/freebsd/scripts/vm.sh`),
  provisioned by nuageinit. `make image` bakes a bootable qcow2.

## Consequences
- No forked base system to maintain; bumping FreeBSD is a pin change plus a
  test run.
- Without KVM, boots take about 100 s (software emulation). QEMU 8.2's TCG
  mis-emulates AVX, so the VM runs with AVX off.
- Certified hardware images (DK-12) will need a different image build
  (likely `poudriere-image` or `makefs`), reusing the same overlay.
- EP-1 (October PRD) requires a read-only, signed base verified at boot,
  updated by swapping whole signed images through ZFS boot environments.
  The development VM's writable, stock image is fine for the loop; shipped
  images must move to that model, with the overlay installed at build time
  rather than by `post-install.sh` on a running system.
