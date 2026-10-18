# 0017. Undo restores what the task changed, found from its change records

Status: Accepted. PRD AU-21, AW-15, AW-21. Supersedes ADR 0006 in part:
which files undo restores, and how it tells what changed. Per-file restore
from snapshots, never a dataset rollback, still stands.

## Context
ADR 0006 picks the files to restore with `zfs diff` under the task's
roots. That has two problems:

- **It restores files the task never touched.** Everything that changed in
  a granted folder since the snapshot goes back: the user's own edits, and
  other tasks' work, such as drafts in the shared `~/Mail/Drafts`. The e2e
  suite hit this as soon as scenarios ran side by side.
- **It costs what the home changed, not what the task changed.** `zfs diff`
  walks every change in the home dataset since the snapshot: 0.8 to 1.3 s
  per undo preview in the VM, growing as the home churns and the task ages.

Agents change files only through actions (ADR 0005), so the runtime already
knows every file a task changed (`Change::paths`). The only changes made on
a task's behalf that the broker doesn't see are the user's own edits while
they drive it (take over).

## Decision
- **Undo restores the files the changes being undone touched, and nothing
  else.** Undo all covers every change since the task's first snapshot; a
  roll back to a marker, the changes since that marker; a per-change Undo,
  those changes. A folder path (a cache cleanup, a moved folder) stands for
  every file under it, in the snapshot or now.
- **Each file goes back to the latest snapshot taken before the first change
  being undone that touched it.** Every change records how many of the
  task's snapshots existed when it ran. So a per-change Undo puts files
  back as they were just before that change, and a roll back finds the
  drafts folder's snapshot for a draft that took none of its own (drafts
  are Auto tier).
- **What to do with a file comes from comparing the snapshot's copy with the
  file now**: missing from the snapshot, it's removed; different or gone,
  it's restored; the same object with the same times, or the same bytes,
  nothing to do. Both sides are read through `ConfinedDir`, the snapshot's
  copy included, and only regular files are restored or removed.
- **The runtime notes how the task left each file**: after every change
  (read in the same blocking call as the action, before anything else can
  touch the file) and after every undo, the file's identity, size, mode and
  times, and a SHA-256 of its content for files up to 1 MiB (what agents
  write through `files.write`). A folder change notes every file left in
  it, by metadata only (a moved folder can be big); a removal notes that
  nothing is there.
- **Conflicts: undo only touches a file still as the task left it**, by
  metadata or, failing that, by content. A file someone changed since (the
  user, another app, another task) keeps their version: undo leaves it,
  lists it under `not_undone` as "changed since this task, so it was left
  as it is", and the change stays undoable for when they've sorted it out.
  We chose leaving over restoring because a rollback that destroys work
  that isn't the task's is the bug this fixes. A new file where the task
  left nothing, and not in the snapshot, is someone else's: undo doesn't
  touch or mention it.
- **Kept changes win too.** When a change this undo leaves in place touched
  a file after the chosen snapshot, restoring would throw that change away,
  so undo leaves the file and says so. Undoing newest first always works.
- **Edits made while driving are changes of the task.** Hand back records
  each file the user changed as a change with action `you.edit` and a
  target ending "(by you)", reversible, on the Changes list, undone back
  to the take-over snapshot. A file that an action already under way at
  take over left, as the record says, isn't counted as the user's.
- **`zfs diff` stays in one place: hand back**, which asks what changed in
  the task's writable folders since the take-over snapshot
  (`Snapshotter::changes_since`). The user's edits don't go through the
  broker, so nothing else can tell. The alternative, walking the granted
  folders and comparing them with the snapshot, costs what the folders
  hold; `zfs diff` costs what the dataset changed during the take over,
  which is usually a short window. Hand back has no latency budget yet;
  revisit if it gets one.

## Consequences
- An undo preview costs a few `stat` calls per file the task changed, plus
  a read when a file's metadata differs, instead of a walk of the home's
  changes (to be measured in the VM; the ceiling in `glass-e2e perf` is 1 s).
- The user's own work and other tasks' work in the same folders survive an
  undo. A rollback no longer means "the folder as it was at the snapshot":
  a file changed after the task keeps the newer version, and the report
  names it.
- "As the task left it" goes by content when metadata differs, so a file
  rewritten with the same bytes, or put back by another task's undo, still
  counts as this task's.
- Limits:
  - Files over 1 MiB, and files under a folder change, are judged by
    metadata alone, so one rewritten with the same bytes counts as changed
    and is left alone (the safe side). An in-place rewrite of the same size
    within one clock tick of the task's own write looks unchanged.
  - The record lives with the task in memory, like the rest of its state.
  - Hand back counts any change in the task's writable folders during the
    take over as the user's, including another task's writes there.
  - Folders a change created stay after undo, empty, as before; links and
    special files aren't restored, and are listed when they differ.
