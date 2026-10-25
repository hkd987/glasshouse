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

   The update source (`[update] source`) is laid out as that folder,
   wherever it is: a folder on a share or a USB drive, Glasshouse's update
   server over HTTPS, or the org's console (section 9).
3. **An image is a ZFS stream** of a boot environment's `@glass-image`
   snapshot. There are two kinds:
   - **A whole image**, received with `bectl import`.
   - **A delta**: the changes since another version's `@glass-image`,
     received as a clone of the image the computer runs (`zfs receive -o
     origin=`). It is only taken when the computer runs exactly that
     version. A delta is a few megabytes where a whole image is about
     4 GB, and what it makes is the whole new image.
   - **Kept, the new environment is promoted** (`bectl activate`), and
     the image snapshot it was cloned from moves to it, beside its own
     `@glass-image` from the stream. ZFS refuses two of one name, so the
     one moving is renamed first, `@glass-image-of-<environment>`; an
     environment promoted back (an update taken out) gets its name back
     (`glass_platform::bootenv::activate`). The dev VM's first delta found
     this: the promotion failed and the update was never kept.
4. **Installing, step by step** (`glass_runtime::osupdate::apply`; the
   model is `specs/tla/OsUpdate.tla`):
   1. the checks above;
   2. the image is downloaded beside glassd's state, hashed as it arrives
      and checked against the digest, so what's received is what was
      checked even if the source changes. A download that stops (the
      network goes) changes nothing else, and the next try goes on from
      where it stopped;
   3. the record says "staging";
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

9. **Delivery** (added 2026-10-25). Wherever releases come from, the
   computer checks every signature and the image's digest itself: nothing
   is trusted for where it came from.
   - **Sources** (`glass_runtime::osupdate::Source`): `[update] source`
     is a folder (an absolute path), an `https://` address, or `console`.
     A managed computer uses its console when it names none.
   - **HTTPS** (`glass_gateway_providers::UpdateClient`): the one HTTP
     client streams a reply's body to disk, binary, never all in memory.
     It asks for the rest of a download that stopped with a `Range`
     header, and passes over what it has when a server ignores ranges.
     Plain HTTP only to this machine, on the development host.
   - **The console** (`glass-console --updates <folder>`) serves that
     folder to its computers, each its ring's channel: the ring IT sets on
     the Devices page is the channel it updates on. A computer reads a file
     8 MB at a time (`/device/v1/os/file`), each request signed by its key.
     The console signs nothing. It can keep the folder a mirror of the
     update server (`--updates-from`, each hour). A channel's
     `release.json` is written only once its image and rebuilds are whole
     there, so a computer never hears of a release whose files aren't.
   - **Automatic checks.** A sync's answer carries the number of the
     newest release on the computer's ring (`os_release`). Newer than the
     last it took, glassd checks for it and downloads its image in the
     background, once a release, and again half an hour after a download
     that stopped. The update status says it's ready, so Settings does.
     A check names the kernel patch rebuilds that come with a release but
     doesn't download them: installing it does.
   - IT's Devices page shows each computer's OS version and what each
     ring is offered, and how the mirror's last round went.

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

  It passed on 2026-10-25, after it found three bugs the stand-in boot
  environments couldn't:
  - a delta's environment couldn't be promoted, so it was never kept (the
    clashing `@glass-image`, above);
  - removing an update that didn't start (`bectl destroy -o`) destroyed
    the running image's `@glass-image` with it, which the next delta
    comes from: only snapshots `bectl` made go with an environment now;
  - after an update rolled back, undo no longer found the kept one it
    should take out: it looks in the history now.

  An image with no init panicked ("no init"), restarted by itself, and
  the loader started the version before.
- **Delivery, in the VM** (2026-10-25), `UPDATE_VIA=https` and
  `UPDATE_VIA=console`:
  - over HTTPS from this host (`tools/dev/serve-https.py`, the development
    certificate trusted for the run only), an update installs, starts
    once, is kept and undone;
  - from a development console, the VM enrolled and in its pilot ring:
    30 seconds after the release appeared in the console's folder, the VM
    heard of it in its sync and downloaded it by itself, with nobody
    checking, then the same install, keep and undo.

  The first HTTPS run stalled at a start: the QEMU process had been
  paused when the host's disk filled, and every start after hung until a
  fresh QEMU. The test now power-cuts a start that doesn't answer and
  waits for the VM before putting it back.
- **Unit tests**: a download that stops stages nothing and the next try
  goes on from where it stopped; a check downloads no rebuild; streamed
  replies by length, chunks and the connection's end, ranges honoured or
  ignored; the console serving each ring its release in parts, refusing
  other names and other computers; the mirror writing a release only once
  its image is whole.

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
  - telling the person and choosing when to restart: an update is ready,
    and installing it is root's (`glassctl update apply`) or, on a
    computer no org manages, its owner's in Settings;
  - IT holding a release back from a ring, or pausing one: today a ring
    gets what the console's folder has for its channel.
- A whole image is gigabytes over the network: deltas keep most updates to
  megabytes, and the console's mirror means each image crosses the
  internet once per org.
