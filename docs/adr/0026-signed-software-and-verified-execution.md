# 0026. Signed software: verified execution from signed manifests, keys by purpose

Status: **Proposed** (design for owner review; nothing enforced yet).
PRD EP-1, EP-2, EP-11, EP-12, BS-2, BS-4, DV-1, DV-10, DW-1, PL-1. Builds
on ADRs 0022 (pkgbase) and 0023 (signed releases).

## Context
EP-2: only signed software runs, packages come from our repository or the
org's approved catalog, enforced with FreeBSD's verified execution
(veriexec) in user and agent jails alike. EP-11 and EP-12 put the signing
keys in HSMs, separate keys for the OS, packages and each release channel,
with rotation and recovery from a compromised key without re-imaging.

What 15.1 gives us (checked on the dev VM and in the release's source):

- `mac_veriexec` is in the source (`sys/security/mac_veriexec`, kernel
  options `MAC_VERIEXEC` and `MAC_VERIEXEC_SHA256`/`384`/`512`), but not in
  the GENERIC kernel or its modules: `/boot/kernel` has no
  `mac_veriexec.ko` and there's no `security.mac.veriexec` sysctl.
- `veriexec(8)` is built only with `WITH_VERIEXEC`, which needs
  `WITH_BEARSSL`; the official base and its 509 packages don't include it.
  The loader verifies manifests only when built `WITH_LOADER_VERIEXEC`.
- `veriexec(8)` checks a manifest's digital signature against trust
  anchors (`-C` directory; `-S` also checks certificate validity), then
  feeds its fingerprints to the kernel. States: *loaded* (first manifest),
  *active* (checks), *enforce* (`execve` and `open(O_VERIFY)` fail for
  anything unverified), *locked* (no more manifests).

So EP-2 can't be met with FreeBSD's packages as they are: it needs our own
kernel, loader and veriexec builds, exactly what BS-2 plans ("rebuild only
the base packages we change: kernel, branding, security configuration").

Today agents already run only programs from read-only `agent_dirs`, and
policy and catalog releases are signed with the org's ed25519 key (ADR
0023). Nothing checks executables.

## Decision
1. **Verified execution is global, so it covers users and agents alike.**
   `mac_veriexec` enforces for every process; agent jails need no separate
   mechanism. glassd's jails keep their read-only mounts as the second
   line.
2. **Every image carries a manifest of everything executable in it**
   (programs, shared libraries, the loader's modules): written at image
   build from the installed files (package-list images make this the
   package inventory's files), SHA-256, signed with the **OS key**. Our
   binaries and overlay go in it like any package's files.
3. **Our base packages that differ from FreeBSD's** (BS-2): a kernel with
   `MAC_VERIEXEC` and `MAC_VERIEXEC_SHA256`, `veriexec(8)` and the loader
   built `WITH_BEARSSL WITH_VERIEXEC WITH_LOADER_VERIEXEC`. They're the
   first entries of our thin fork (BS-1), each a documented build knob, no
   patch.
4. **Boot order:** UEFI Secure Boot verifies our loader; the loader
   verifies the kernel and the base manifest (libsecureboot); rc loads
   the manifest and sets *enforce* before any user or agent process runs.
5. **Catalog software** (agents, apps, adapters, DV-1, DV-10) comes with a
   manifest signed by its publisher's certificate, which chains to our
   developer CA, or by the org's own key for internal items. glassd
   installs an item only through `veriexec`, which checks the signature.
   The kernel stays *enforce*, not *locked*, so installs don't need a
   reboot. Root can therefore still load fingerprints; that is the gap
   until catalog items ship inside images (open question 2).
6. **Keys by purpose** (EP-11): an offline root (HSM, key ceremony) signs
   four online signing certificates: OS (images, manifests), packages
   (our repository, which also re-signs FreeBSD's, BS-2), developer CA
   (publisher certificates), and one per release channel (pilot, early,
   broad; update metadata, EP-12). A compromised online key is revoked by
   the root and replaced through signed update metadata, without
   re-imaging. Orgs keep their own keys for internal items (ADR 0023).
7. **Development images use a development root.** Its key is in the
   repository, marked as such. Images built with it say "development
   build" in About and on the sign-in screen. `make vm-deploy` writes and
   loads a new development manifest for the files it installs, so the
   12 s loop stays.
8. **Where unsigned code runs:** the developer workspace (DW-1), a bhyve
   Linux VM outside the host's veriexec, as the PRD sets out. Nothing on
   the host.
9. **Behind the platform layer** (PL-1): `glass_platform::integrity`
   (state, load a manifest, check a file). Its Linux equivalent is IMA
   appraisal or fs-verity with dm-verity for the base.

## What it changes for people
- Terminal runs what the image and the catalog ship. A downloaded program
  or one built in a home folder won't start ("isn't signed"). Nor will a
  library that isn't listed: the run-time linker opens every library
  `O_VERIFY`, so `LD_PRELOAD` of an unlisted one fails too.
- Scripts depend on the interpreter.
  - The base `sh` has a `verify` option, off by default, that opens
    scripts and sourced files `O_VERIFY`. With it set for everyone
    (`/etc/profile` and `ENV`), unsigned shell scripts fail as well, with
    no patch needed.
  - Interpreters outside the base (Python, Node) don't have this; they
    come only from the catalog, and their scripts run.
  - Developers use DW-1.
- `pkg install` from FreeBSD's repositories installs files that won't run.
  Packages come from our repository or the catalog (EP-2 says this).

## Slices
1. **Spike, in a VM.**
   - Build the kernel and world knobs above.
   - Measure the boot and exec cost of loading a manifest of about 3,000
     files.
   - Confirm enforcement over nullfs mounts (agents' `agent_dirs`), since
     fingerprints are looked up by vnode.
   - Check `sh -o verify` and the run-time linker's `O_VERIFY` under
     enforce.
2. **Enforce in package-list images with the development root.**
   - The manifest is written by `bake-pkgbase.sh`; `vm-deploy` refreshes
     it.
   - A probe check that an unlisted binary dropped into an agent's
     writable folder can't run. This is a new sandbox surface, so it fails
     on the host backend and passes in a jail.
   - An e2e check that a person's unlisted binary can't run either.
3. **Signing chain:**
   - the loader's trust anchors;
   - catalog manifests from publisher certificates;
   - the console signing org items' manifests with the org key.
4. **Keys and ceremony** (EP-11, EP-13):
   - HSM choice;
   - the ceremony runbook;
   - isolated signing infrastructure.
   These are process and infrastructure, not code in this repository.

## Open questions (owner)
1. Should a person's own shell scripts run (`sh` without `verify`) or be
   refused like programs (`set -o verify` for everyone)? Refusing them is
   one line of configuration, and it breaks the scripts people write for
   themselves.
2. Catalog installs at runtime (needs *enforce* without *locked*, so root
   can still load fingerprints) versus only between images (*locked*,
   strongest, but a new agent waits for the next image or a reboot).
3. Is "development build" on the sign-in screen enough to tell
   development images apart, or should they never boot on customer
   hardware (a Secure Boot key only test machines enroll)?

## Consequences
- EP-2 waits on BS-2's first own packages; both move together.
- Every image build writes and signs a manifest; a file missing from it is
  a broken image. The e2e suite on the image catches it, and the bake can
  check that each ELF it installs is listed.
- Faster than it sounds: fingerprints are checked at `execve` and cached
  per vnode. The spike measures it against the boot and app-launch
  budgets (UX-22).
