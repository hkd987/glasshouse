# 0054. Changes on the record before they run, and support roles modelled

Status: **Accepted, built** (2026-10-24). The owner, on the two items ADR
0053 left open: "Ok let's fix the two open items then, making sure to
formally verify them." Follows ADRs 0051 to 0053, whose conventions it
keeps (`specs/tla/README.md`).

## Context
- **The audit log.** After ADR 0053, a record the log couldn't take waited
  in memory until the disk had room. A change's record was written only
  after the change. So two things could leave a change that happened off
  the record for good:
  - glassd stopping between the change and its record;
  - glassd stopping while the record waited for a full disk.
  The AuditLog model said so: its invariant let a change's record be
  "lost".
- **Remote help.** Support roles and signing in to the console by role
  (ADR 0028 slice a) weren't modelled.

## Decision
### 1. A change is on the record before it runs (AU-1)
- **Before it runs.** A call that changes something or sends data off this
  computer writes an `ActionStarted` record first, synced, after the
  log's health check and before its snapshot and its provider. That means
  every side effect but `None`: changes, sends, model calls. Its outcome
  (`Action`) names it (`detail.started`, its `seq`).
- **When it can't be written,** the call doesn't run and the agent reads
  that the log can't take records. That it was refused waits to be written
  instead ("Not done: …", stamped with when). So the log stays down,
  refusing reads too, until there's room, and then the refusal is on the
  record.
- **Undo** does the same: a `RollbackStarted` record before it changes
  files, named by its `Rollback`. While the log can't take records,
  nothing is undone and the person is told why.
- **Reads** write nothing ahead: what a read finds reaches the agent only
  once its record is written (ADR 0053).
- **Reading the log:**
  - `glassctl audit` shows a call once: a start whose outcome names it is
    hidden, and one no outcome names reads "no outcome recorded";
  - glassd's report of blocked actions, and anything else that counts
    outcomes, reads `Action` records, as before;
  - tests that found records by action alone now filter by kind.
- **What can still be lost** if glassd stops while the disk is full: a
  change's outcome (its start is on disk, and says so), and glassd's own
  notes waiting in memory (a sign-in, settings applied). No change an agent,
  an app or a person made can be.

### 2. The model, extended
`specs/tla/AuditLog.tla` follows the new code:
- a change writes its start record through the same append (the lock, the
  bytes landing, the sync, a full disk cutting it short) before its
  provider runs;
- a start the log can't take refuses the call and keeps the refusal
  waiting.

New invariants, holding through every crash and full disk:
- `AU1_AChangeIsOnRecordBeforeItRuns`: every change that ran has its start
  record, written whole, on disk. The old `AU1_WhatRanIsRecorded` still
  holds, and no longer needs its "lost" for changes.
- `AU1_AnOutcomeFollowsItsStart`: a change's outcome follows its start.

The `Old` switches, each bringing its counterexample back:
- `noWriteAhead`, the code before: a change ran with nothing of it on the
  record, so a stop before its outcome left it off.
- `refusalNotKept`: my first version of the fix, which TLC caught. A start
  the log couldn't take left nothing waiting. In the model the log never
  counted as down again, so it never recovered. In the code an empty queue
  read as healthy, so reads ran again while the disk was still full. Both
  have tests that fail on the old code.

| Model | States | TLC, one worker |
|---|---|---|
| AuditLog (two configurations) | 367,632 / 15,710 | 14 / 3 s |

### 3. Support roles and the console, modelled (CF-80, CF-81, ADR 0028)
`specs/tla/SupportAccess.tla` models the console request by request while
IT edits, publishes and rolls back a support role and the console restarts
with a new directory. It found three bugs, all fixed:
1. **A session whose person lost every role came back.** The console said
   "sign in" but kept the session, so once IT gave the role back (by a
   rollback, say) the same session worked again. Such a request ends the
   session now (401, with the sign-in list), and it stays ended.
2. **A Support page could mix two policies.** It decided access by one read
   of the roles in force and built itself from a second, so a publish in
   between mixed them. It reads them once. The model alone shows this one:
   the window is inside one request.
3. **The page named everyone on a computer it showed,** not only the people
   the role may help. It names only those.
- **Speed:** every console request parsed the pilot policy's whole text,
  members included, under the console's lock (1.5 ms on a 6 KB policy in a
  debug build, growing with the org). A version's roles are kept once
  parsed (10 µs).

| Model | States | TLC, one worker |
|---|---|---|
| SupportAccess | 173,835 | 10 s |

**The two gaps ADR 0028 says slice (b) must close first:**
- **glassd authorizing control calls by uid only:** half closed by ADR 0031.
  `seat.active` accepts only the desktop, by peer pid. The consent, frame
  and input calls slice (b) adds must use that check.
- **Signed releases with no expiry or device binding:** still open. Both
  change the signed format on the console and on devices, so they come
  with slice (b).

## Consequences
- **One more synced record per change:** a write with a snapshot first, in
  the emulated VM, went from a median of about 250 ms to about 280 ms. Its
  budget is 750 ms.
  - Written before the snapshot, the record's sync doesn't wait behind the
    snapshot's. Written after it, the median was about 350 ms.
- **The log holds two records per change.** What counts actions counts
  `Action` records.
- **While the log can't take records,** undo waits too.
- **The console** answers a request from someone who lost every role with
  401 and the sign-in list, not 403.

## Not covered
- glassd's own notes waiting in memory when it stops on a full disk.
- Console sessions don't expire: they last until sign-out or a restart.
- Slice (b) of remote help (offers, consent, frames) isn't built, so it
  isn't modelled.
