# 0007. Drive jails and ZFS through their admin commands

## Context
Jails and ZFS can be driven by syscalls (`jail_set(2)`, `libzfs_core`) or by
their admin commands (`jail(8)`, `jexec(8)`, `zfs(8)`).

## Decision
Use the commands. Each jail gets a generated `jail.conf` and `fstab` under
`/var/run/glasshouse/jails/`, created and removed with `jail -f <conf>
-c|-r`, so jail(8) owns the mount lifecycle. Snapshots use `zfs snapshot`,
`zfs diff -FH` and `zfs destroy`.

## Consequences
- No `unsafe` FFI for the most security-sensitive code, and every action the
  daemon takes can be reproduced or undone by an operator with the same
  files and commands.
- A few milliseconds of process spawn per operation; fine at task
  granularity. Revisit if per-action overhead ever matters.
- Root deletion is guarded: a jail root is never removed while anything is
  still mounted under it.
