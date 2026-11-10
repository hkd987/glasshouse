# 0020. Agents that may run elsewhere: what the architecture commits to now

Status: **Proposed**. PRD FD-1 to FD-4 (P0 design commitments, no
user-facing feature), "Future direction: always-on agents with local
hands". Builds on ADRs 0004, 0008, 0015, 0017.

## Context
The PRD's future direction moves an agent's reasoning to the cloud (a
managed Glasshouse cloud, or inside the customer's AWS) while its actions
still run on the laptop through the same brokers, approvals and audit; when
the laptop sleeps, work continues on a cloud copy of the workspace and syncs
back. None of that ships in Pilot 1, but four commitments are P0 now so the
architecture doesn't block it:

- FD-1. Brokers are location-independent.
- FD-2. An agent's work can be checkpointed and moved mid-task; the same
  mechanism handles sleep (PM-5).
- FD-3. Agent identity is never tied to one machine (PA-7).
- FD-4. Snapshots and undo history can follow a task across machines.

Where the code stands:

- **FD-1, mostly true.** An agent reaches glassd only through its task's
  MCP socket. The pipeline (`decide()`, labels, asks, snapshots, audit)
  works on the task and the call, never on how the call arrived, and paths
  are relative to the task's grants (`/data/<name>` in the jail), not to
  the agent's file system.
- **FD-2, not yet.** Sleep (PM-5) holds each agent at its next call; the
  process stays in memory. That survives a suspend, not a move.
- **FD-3, not yet.** An agent is its catalog id and version (ADR 0008
  proposes directory identities).
- **FD-4, not yet.** Change records (ADR 0017) live in glassd's memory, and
  snapshots are this machine's ZFS snapshots.

## Decision
Rules for new code, checked in review (`containment-review`):

1. **The task socket is the only agent interface, and the pipeline never
   asks where a caller runs.** No decision may depend on the transport, the
   peer's pid or the jail beyond identifying the task. A remote agent will
   arrive over an authenticated channel bound to the device's hardware keys
   and the user (the PRD's security rules) and be handed to the same
   `call_tool`.
2. **Task state is data.** Everything glassd knows about a task (request,
   scope, grants, activity, asks, change records, snapshots taken, the
   tailoring layer in force) stays in serializable types, so a task can be
   written out and read back. Nothing a decision needs may live only in a
   process or a file descriptor.
3. **Change records are persisted with the task** (FD-4), next to the audit
   trail, so undo survives a glassd restart; the snapshots they point to
   are named per task so they can be sent (`zfs send`) with it.
4. **Checkpoints, not frozen processes (FD-2).** Agents get a checkpoint
   call in the task API: at a safe point (between actions, as PM-5 already
   uses) glassd asks the agent for its state, records it with the task, and
   can start the agent again from it, here or elsewhere. Sleep keeps today's
   pause, and takes a checkpoint first once agents support it.
5. **Identity is the catalog id plus the directory identity (FD-3,
   ADR 0008)**, never a host name, a uid that differs per machine, or a
   path.
6. **A remote agent is a tile like any other**, and the kill switch stops it
   at once: stopping a task closes its channel and refuses its calls.

## Consequences
- Item 3 is the next concrete work: today a glassd restart loses undo for
  running tasks. It belongs with the task-state persistence that crash
  recovery already needs.
- The checkpoint call (item 4) changes the agent SDK and the add-agent
  skill; first-party agents get it first.
- Nothing here changes what an agent can reach today.

## Amended 2026-11-10 (audit 5.1): tasks are kept on disk (item 3, in part)

Nothing of a task was on disk but its agent's output. Stopping glassd, a
crash or a power cut took every task with it, and the undo of all they had
changed, though the snapshots to undo from stayed: `glassctl undo` said "no
task". Found again by cutting the power under a task and asking for its undo.

- **A task's record is written beside its agent's output**,
  `<state>/tasks/<id>/task.json`, readable by root alone, through the
  durable state-file helper: what was fixed when the task started (whose it
  is, its agent's catalog entry, its request, scope and grants) and its
  record so far (its tile, activity, changes, snapshot handles, and how it
  left each file, which is what undo judges by). Not what only a running
  task has: its gate, its socket, the calls a routine would be made of.
- **When.** As it starts waiting for the person; within a second of a change
  it records (one write covers a burst); after an undo; when the person's
  edits are recorded at hand back; when it ends.
- **At start glassd reads the records back as ended tasks**, before it takes
  calls and before snapshots are pruned (a snapshot is kept while a task in
  memory could undo from it). A task that was still working comes back as
  failed, "the system restarted while it was working", and that end goes on
  the audit record, where the task's records used to just stop. Its rules
  are ones that allow nothing (`Policy::for_ended`): an ended task makes no
  calls.
- **A record can be behind its task** by the changes of its last second. An
  undo after a restart knows nothing of such a change and leaves its files
  alone: the record says how the task left each file, and a file that is
  another way now was changed since. Being behind only ever leaves more as
  it is; the audit log has every step. `specs/tla/Undo.tla` checks undo's
  promises with glassd stopping at any step and a record that is behind.
- **A record goes with its task**: `task.dismiss`, the retention of ended
  tasks (ADR 0055's 30 days and 200 a person) and an app's session that
  changed nothing remove it. A record this glassd can't read (damaged, or a
  later glassd's) is passed over, said in the log, and its folder goes after
  seven days as task output does.

- **The desktop can let an ended task go.** A restart used to clear every
  ended tile, and nothing on the desktop sent `task.dismiss`. Ended tasks
  now stay through restarts, so an ended task's expanded view has Dismiss
  in its header and the agents menu has "Clear finished" for all of them.

Still open in item 3: snapshots are named by their own ids, not per task, so
they can't yet be sent with one (`zfs send`). And one window stays: glassd
stopping in the few milliseconds between an undo's last step and its
record's write. That undo is offered again; it finds the files other than
the task left them and leaves them, saying so, and an app's revert may be
asked a second time.
