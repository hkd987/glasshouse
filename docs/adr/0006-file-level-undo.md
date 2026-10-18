# 0006. Undo restores files from snapshots, never rolls back datasets

## Context
`zfs rollback` reverts a whole dataset, including the user's own changes and
everything after the snapshot, and destroys later snapshots.

## Decision
Snapshot whole datasets (cheap, atomic), but undo per file: `zfs diff`
finds what changed under the task's roots, and each file is restored from
`.zfs/snapshot/<name>/…` (or removed if the agent created it). The runtime
snapshots a root before its first change in a task and before every
Notify-tier change (AU-20), so the earliest snapshot covering a root always
predates the agent's changes to it.

## Consequences
- Changes outside the task's roots are never touched.
- Within a root, rollback restores the snapshot state, so a change the user
  made in the same folder during the task is rolled back too. `--dry-run`
  shows exactly what will change. A per-task clone (agent works on a ZFS
  clone, changes merged on accept) would remove this limit at the cost of
  changes not being live; revisit with the shell.
- Development hosts use a copying snapshotter with the same interface.
