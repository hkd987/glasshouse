---
name: cleanup
description: Free disk and tidy up after work - merged agent worktrees, build caches, stale VMs and processes, a VM paused by a full disk. Use when `df -h /` shows under 8 GB free, after merging an agent's branch, before starting big builds or a second VM, when a build dies with "No space left on device" or a VM stops answering, and before ending a session.
---

# Cleanup

Disk is the scarce resource here: each Rust target dir is several GB, each
VM disk a few more, and the session's allowance is small. When the disk
fills, builds die mid-link, lld crashes in `memcpy`, and QEMU **pauses**
the VM (status `io-error`), which looks like a hung VM.

## Every time you finish a piece of work

1. `tools/cleanup.sh`: a report only: disk use, merged worktrees, caches,
   VM dirs. Read it.
2. `tools/cleanup.sh --apply` removes what it listed as safe:
   - worktrees whose branch is merged into main (an agent's worktree stays
     locked while the agent runs; once it has reported and you merged it,
     `git worktree unlock <path>` first);
   - debug-build incremental caches (rebuilt, slower once);
   - test binaries left by earlier builds of the same test (only the newest stays);
   - `build/dist`, `build/logs`;
   - VM directories other than `build/vm` with no QEMU running.
   It also resumes any VM QEMU paused because the disk filled.
3. Still short? `tools/cleanup.sh --apply --deep` also drops the `vm`
   profile's incremental caches: the next `make freebsd vm-deploy` takes
   minutes instead of seconds. Past that, ask before deleting
   `build/cache` (the FreeBSD downloads, slow to fetch again) or
   `build/vm` (the dev VM: `make vm-reset` rebuilds it).

## Never

- Delete `build/vm`, `build/cache` or `build/freebsd` (the sysroot) to save
  space without asking.
- Remove a worktree whose agent is still running, or one with commits not
  on main.
- `pkill -f` a pattern that matches your own shell's command line (it
  kills the shell; exit 144). Kill by PID or `pkill -x <name>`.
- Leave a second VM running when you're done with it: `VM_DIR=... vm.sh
  down`.

## Other leftovers

- Processes: `pgrep -lf 'qemu|glass-'` on the host; in the VM,
  `os/freebsd/scripts/desk.sh log` and `ps -axo pid,ppid,command | grep
  glass` (an orphaned `glass-greeter --screen` holds the screen).
- The VM's own disk grows with snapshots and logs: `vm.sh ssh 'zpool trim
  zroot'` gives freed blocks back to the host image.
- Background agents: tell them how much disk they may use, to share the
  main checkout's built binaries instead of building in their worktree,
  and to delete their VMs and scratch files when they finish.
