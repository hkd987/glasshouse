# 0064. The system checked at every start, and read-only

Status: **Accepted, built** (2026-10-25). EP-1's first slice. The owner:
"Ok let's do all these todos", which included "a read-only system verified
at every start".

## Context
- EP-1 asks for a read-only, signed and verified base. ADR 0061 signs OS
  updates and rolls a bad one back. Nothing checked the system the
  computer runs, and anything root did to it stayed.
- ADR 0026 designs the whole of it: Secure Boot verifies the loader, the
  loader the kernel, and veriexec checks every program as it runs,
  against a manifest the OS key signs. That needs our own kernel, loader
  and veriexec builds (BS-2). The manifest is the piece everything else
  needs.
- The system's files are about 4,200 in the dev VM, outside debug
  symbols. Configuration (`/etc`, `/usr/local/etc`) and state (`/var`)
  change as the computer is used: they aren't the system.
- A ZFS boot environment can't simply be made read-only: `/etc`,
  `/var/db` and `/var/run` are in it, and glassd, the base system and
  people's settings write there. Moving them out is the rest of EP-1.

## Decision
1. **The list.** An image carries the list of its system files
   (`/usr/local/share/glasshouse/system.json`): each regular file's
   SHA-256 and size, and where each symbolic link points, under the
   programs', libraries' and kernel's folders
   (`glass_platform::integrity::SYSTEM_DIRS`), debug symbols and the
   kernel's module index aside. The OS key signs it
   (`DocKind::OsManifest`). It is what veriexec will load later.
   - `glassctl system manifest [--root]` lists the files where the image
     is; `glass-release manifest` signs the list on the release side.
   - Beside it, the root's key list as of the image
     (`/usr/local/share/glasshouse/keys.json`), so the first start has
     the OS key. Whichever of it and the list an update brought is newer
     names the keys: an OS key revoked since the image fails the check.
2. **Checked at every start.** glassd checks the system's files after
   the kernel patch and update start checks, off the start path, on as
   many threads as there are cores (`glass_runtime::system`). Expected is
   the list, with the modules kept kernel patches wrote over it: each
   environment's are recorded as they're written
   (`<state>/hardware/files/<env>.json`).
3. **What it decides.**
   - **Verified:** the files are the list's, nothing missing, nothing
     added. Agents start.
   - **Changed:** a file differs, is missing, or a file nobody listed is
     there (a program planted in `/usr/local/bin`). No agent starts
     (`may_start` in `start_task`): the refusal says why. Apps still
     open, so the person can reach Settings. IT puts it right: an update
     taken out, or the computer installed again.
   - **Checking:** agents wait for it, refused with "in a moment".
   - **Unchecked:** no list (a development build) or no root key. Agents
     start. An image built for release sets `[update]
     require_signed_system = true`: then no list, or none that verifies,
     is Changed.

   Each check is on the audit record (`system_checked`) and in `glassctl
   status`.
4. **Read-only.** Verified, any listed file without the system immutable
   flag (`schg`) gets it. Root can't write, rename or remove such a file
   until it takes the flag off. Kernel patches take it off the modules
   they replace, in their own environment. `make vm-deploy` replaces
   flagged files (`tar --clear-nochange-fflags`). The kernel's
   securelevel stays as it is: raising it would also stop modules
   loading when a device is plugged in.

## What it doesn't stop
Root taking the flag off, changing a file, and glassd with it, before the
next start. Nothing on the computer can vouch for itself against its own
root: that needs the start verified from the firmware up (Secure Boot)
and each program checked as it runs (veriexec), ADR 0026's slices. This
slice catches what an agent, a person, a package or a mistake changes,
and leaves a changed system unable to run agents.

## How it's tested
- **Unit:** the files listed and hashed in parallel, debug symbols and the
  module index left out (`ep1_the_system_is_its_programs_libraries_and_kernel`);
  the list opens only with a listed, unrevoked OS key, and what differs
  is named (`ep1_the_system_is_checked_against_the_list_the_os_key_signed`);
  verified, changed (a program changed, one planted, one missing),
  another key, a revoked key, a newer key list, a development build,
  a required check, a patch's modules (`glass_runtime::system`); no agent
  starts while the check runs or after it found the system changed, and
  each check is on the audit record
  (`ep1_no_agent_starts_on_a_system_that_differs_or_while_it_is_checked`).
- **The model:** Containment.tla's `CheckSystem`, `Tamper` and `PutRight`,
  and `EP1_NoAgentStartsOnAChangedSystem`. With the gate switched off
  (`Old = {"noSystemCheck"}`) TLC finds a start before the check.
- **In the VM** (`tests/system/verify-vm.sh`, 2026-10-25): the VM's 4,195
  system files listed (48 s, from disk) and signed with a development OS
  key, the VM trusting the run's root and requiring the check for the run
  only.
  - Verified in 7.0 s, the files made read-only. `printf x
    >>/usr/bin/yes` as root failed. An agent ran.
  - `/usr/bin/yes` changed (its flag taken off first) and
    `/usr/local/bin/glass-planted` added: changed, both named, the agent
    refused with the reason, both checks on the audit record.
  - Put back: verified again. The run put the VM's flags, configuration
    and files back as they were.

## Consequences
- The release pipeline lists each image's system files, signs the list
  with the OS key, and puts it and the root's key list in the image.
  Release images set `require_signed_system`.
- **About 7 s at every start in the emulated VM**, for about 1 GB of
  files. A laptop's disk and CPU should make it well under 2 s
  (`docs/dev/performance.md`); DK-12 measures it. An agent started in
  those seconds is asked to wait.
- **Not built yet:**
  - `/etc`, `/var/db` and the rest of the computer's own state moved out
    of the boot environment, which can then be ZFS read-only. That also
    carries a computer's settings, Wi-Fi and accounts across an update
    from the release pipeline, which ADR 0061's images don't yet;
  - IT seeing a changed computer on the Devices page;
  - Secure Boot and veriexec (ADR 0026).
