# 0005. Agents get no user data mounted; file access is brokered

## Context
If a jail mounts a user's folder read-write, the agent can delete files
without the Ask that policy puts on `files.delete`, and read files above its
label limit, because the kernel can't filter a mount by label or tier.

## Decision
By default a task's jail contains no user data at all. Agents read and write
through `files.*` actions, so every access is tiered, label-checked,
snapshotted and audited. The broker performs the I/O as root through
`ConfinedDir`, which can't be steered outside the grant by `..` or symlinks,
and hands ownership of new files to the user.

## Consequences
- Exact enforcement of tiers and labels for native agents.
- Harnesses that want direct file access for their own shell tools (HX-2)
  will need an opt-in read-only mount mode, allowed only after a label scan
  of the folder and only for policies where direct access can't bypass a
  tier. Not built yet.
- Large files cost a copy through the broker; `files.read` caps at 1 MiB.
