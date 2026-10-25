# 0061. Signed OS updates through boot environments

Status: **Accepted, built** (2026-10-25). EP-1, EP-11, EP-12, BS-4. The
owner, after the gaps ADR 0056 to 0058 left: "All the above let's get them
all done."

## Context
- EP-1 and BS-4: updates swap in a whole new signed image through ZFS boot
  environments, with rollback if it doesn't start. Nothing is compiled on
  the computer.
- EP-11: separate keys for the OS and for each release channel.
- EP-12: update metadata is signed, and keys can be rotated and a stolen
  one revoked without re-imaging the fleet.
- ADR 0057 built the first half for kernel patches: a boot environment of
  its own, started once (`bectl activate -t`), checked by glassd, then
  made permanent. Its record is on glassd's dataset, outside the
  environments.
- ADR 0026 proposed the key layout: an offline root over online keys by
  purpose.
- A computer may keep a kernel patch (ADR 0057). A new image doesn't carry
  it, and losing it silently would break what it fixed, Wi-Fi after sleep
  on the first one.

## Decision
1. **Three kinds of key** (`glass_managed::osupdate`):
   - **The root** (offline, a key ceremony) signs the key list: the keys
     in use, by purpose, and those revoked. An image carries only the
     root's public key (`[update] roots`).
   - **The OS key** signs each image's digest: SHA-256, size, the version
     the image says it is, and, for a delta, the version it applies to.
   - **Each channel's key** (`pilot`, `early`, `broad`) signs that
     channel's release, which carries the OS key's signed digest.

   A computer takes a release only if all of these hold:
   - the key list it holds names the channel's key and the OS key, and
     neither is revoked;
   - both signatures verify;
   - the digest is for the same release number;
   - the number is above the last release it took.

   A newer key list replaces the one it holds whatever happens to the
   release. Revoking a key, or bringing in a new one, is publishing a new
   list. An old list played back changes nothing.
2. **A release is a folder:**
   - `keys.json`;
   - `<channel>/release.json`, with its image beside it;
   - and any kernel patches rebuilt for it.

   The update source (`[update] source`) is that folder, on a share or a
   USB drive. Updates the console brings will be put in one too. Fetching
   over HTTPS, with images downloaded as they're read, is next.
3. **An image is a ZFS stream** of a boot environment's `@glass-image`
   snapshot. There are two kinds:
   - **A whole image**, received with `bectl import`.
   - **A delta**: the changes since another version's `@glass-image`,
     received as a clone of the image the computer runs (`zfs receive -o
     origin=`). It is only taken when the computer runs exactly that
     version. A delta is a few megabytes where a whole image is about
     4 GB, and what it makes is the whole new image.
4. **Installing, step by step** (`glass_runtime::osupdate::apply`; the
   model is `specs/tla/OsUpdate.tla`):
   1. the checks above;
   2. the record says "staging";
   3. the image is copied in beside glassd's state, hashed as it goes and
      checked against the digest, so what's received is what was checked
      even if the source changes;
   4. it's received into `glass-os-<version>`;
   5. the kept kernel patches' rebuilds are written into it;
   6. the release's number is kept as the last taken;
   7. the next start is set to it once;
   8. the record says "staged".

   An install that fails before step 6 can be tried again.
5. **At the next start** glassd checks the image says it's the release's
   version (`/usr/local/share/glasshouse/os-release.json`). Then it makes
   the image the permanent one, then the record says kept.
   - An image that doesn't start (it panics, hangs, or its init is
     missing) never gets that far. The start-once is spent, so the next
     start, by itself after a panic or after the power button after a
     hang, is the version before, and the update rolled back.
   - An image that starts but isn't the release's fails, and goes back.
   - The version before stays, to go back to (`glassctl update undo`), and
     so does the computer's first. The updates in between are removed.
6. **Kernel patches the computer keeps survive the update**, or the
   update is refused. For each one kept, the release must do one of:
   - fix the same thing (`fixes`): the patch goes;
   - bring a rebuild (`patches`), signed by a key the computer takes
     patches from and built for the image's base: written into the new
     image and kept there.

   Otherwise `apply` refuses and says which patch and what it fixes.
   `--drop-patches` takes the update anyway, on the record. No update goes
   while a kernel patch is being tried, nor a patch while an update is.
7. **Root only:** `glassctl update` (status), `check`, `apply`, `cancel`,
   `undo`. Each step and each refusal is on the audit record (`os_update`
   events). Nothing asks the person yet. The console sending updates to
   devices, and Settings telling the person, come with the update server.
8. **The release side:** `glass-release` makes keys, signs key lists, and
   signs a release of an image or of a digest made where the image is. The
   development keys are in `build/osupdate`. The pipeline's live in
   hardware security modules (EP-11, EP-13, ADR 0026's slice 4).

## How it's tested
- **The model** has five Old switches, each a version that would be
  wrong, and each fails:
  - `activateFirst`: a half-received image starts;
  - `permanent`: an image that doesn't start is what every start uses;
  - `replay`: an old release undoes a fix;
  - `trustAnyKey`: a revoked key still signs;
  - `patchesIgnored`: a kept patch's fix is lost.
- **Unit tests** against the stand-in boot environments: whole images and
  deltas; tampered, unlisted, revoked and replayed releases; a kept
  patch's fix, rebuild and drop; an image that doesn't start; undo and
  cancel.
- **In the VM** (`tests/update/update-vm.sh`), with deltas, since a whole
  image is larger than the dev disk spares:
  - an update installs, starts once and is kept;
  - refusals;
  - an image whose init is missing rolls back by itself;
  - undo.

## Consequences
- The release pipeline makes each image from a `@glass-image` snapshot,
  writes its os-release.json, and keeps the previous release's snapshot
  to make deltas from.
- **EP-1 is half done.** Updates are signed and roll back, but the base
  isn't read-only or verified at every boot yet. That's ADR 0026's
  verified execution and Secure Boot.
- An update's environment is a whole image: two versions and the first
  take about 12 GB on a laptop.
- **Not built yet:**
  - fetching over HTTPS;
  - the console's update server and channels per group;
  - telling the person and choosing when to restart;
  - automatic checks.
