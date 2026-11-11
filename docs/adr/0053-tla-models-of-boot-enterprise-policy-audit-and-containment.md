# 0053. TLA+ models of boot, enterprise policy, the audit log and containment

Status: **Accepted, built** (2026-10-24). The owner: "We should have TLA+ at
the boot level and at the enterprise settings etc…. Making sure key systems
work." Follows ADR 0051 and ADR 0052, whose conventions it keeps
(`specs/tla/README.md`).

## Context
- ADRs 0051 and 0052 modelled what people and agents do once a computer
  is running. What every one of those rests on wasn't modelled:
  - the computer starting, from power on to a person's desktop, through
    crashes and power cuts;
  - an organization's policy from IT's draft to the call it decides;
  - the audit log the OS promises everything is on;
  - a task's containment over its whole life and across restarts.

## Decision
### 1. Five models, checked on every CI run
Each follows the code step by step and is checked by `make tla`. Every fix
below came with a test that failed on the old code, unless it says the
model alone shows it, and each fix was switched off in its model to show
it's needed.

| Model | States | TLC, one worker, four models at once |
|---|---|---|
| Boot | 7,322 | 5 s |
| PolicyApply | 41,294 | 4 s |
| PolicyConsole | 202,328 | 14 s |
| AuditLog (two configurations) | 129,986 / 7,386 | 7 / 4 s |
| Containment | 576,024 | 25 s |

`make tla` checks all 35 configurations in about two and a quarter
minutes on four cores.

### 2. What the models found, and the fixes
**Boot and first-run setup** (ADR 0019, ADR 0021, ADR 0031, ADR 0038,
UX-2):
1. glassd read the computer's owner only as it started, which at first
   boot is before setup made them: for their whole first session the
   computer treated them as IT's, and they couldn't connect AI models or
   change its settings. glassd learns the owner when someone signs in.
   The model alone shows the old behaviour.
2. The sign-in screen told glassd once who signed in, and gave up if
   glassd was starting or restarting: glassd then never knew who was at
   the screen, so the computer didn't sleep for them and USB drives, the
   browser and the owner didn't follow them. The sign-in screen tells
   glassd until it hears, and stops when the session ends; glassd takes
   one session told twice as one sign-in, and refuses one whose session
   has ended.
3. A power cut between first-run setup's steps left a home with no
   account, whose name setup then refused for good, or an account whose
   home wasn't theirs. Setup writes down what it's making first
   (`setup.making.toml`); the next start finishes it or undoes it, and
   `setup.toml` is written whole. The model shows the old behaviour: the
   steps run only on the device.
- Checked and holding: glassd enforces only a policy that validated; the
  last good one only ever holds a policy that was in force; glassd
  refuses to start only with no good copy; nothing an earlier glassd left
  survives into a run where a task starts.

**An organization's policy** (AD-2, AD-3, DV-12, ADR 0021, ADR 0023):
4. An open app's call that waited for the person's answer, or for their
   turn, ran after IT's release blocking it had been applied: approving a
   send IT had just made Never sent it. Once a call has the person's
   turn, a session whose policy changed since its decision decides it
   again; what that blocks doesn't run, and what now asks unasked is
   refused, to be called again. An agent's task keeps the policy it
   started with, as ADR 0023 says.
5. A call read its session's policy twice, for the people-only check and
   for its decision. It reads it once. Harmless so far (people-only
   doesn't change between releases); the model alone shows it.
6. Publish sent the draft as it was, previewed or not: another admin's
   edit made after IT's preview went out unseen. A preview names the
   draft it shows, and publish takes only that draft.
7. Promote sent whatever the ring before it ran: a version published as IT
   clicked went on untried. Promote names the version IT saw run.
8. A rollback replaced the draft, throwing away edits nobody had
   published, without a word. It says so.

**The audit log** (AU-1, AU-2, AU-3):
9. A record being written, or one a full disk or a crash cut short, ends
   the file in part of a line, and every reader took it for corruption:
   `glassctl verify` failed, and glassd reported no blocked actions to the
   console. Readers stop before an unfinished last line, and verify says
   how many bytes it holds.
10. One failed write (a full disk) left the writer unusable until glassd
    restarted. The next record opens the file again as a start does: the
    unfinished line is cut off and noted, and the log takes records again
    once there's room.
11. A record the log couldn't take was dropped, and its call's result went
    back anyway. It waits now, in order, stamped with when it happened,
    and nothing else runs until it's written.
12. What a read found reached the agent though the read wasn't on the
    record. It's withheld; a change's result still goes back, since the
    change happened.
13. Reads went on while the log was down. Nothing an agent or an app asks
    runs until the log takes records again.

**Containment over a task's life and across restarts** (PA-1, PA-2,
AW-11, AU-20):
14. Two starts at once could both fit under the person's live-task limit:
    they were counted and added under separate locks. It's one step now.
15. A task stopped while it was being set up still started its agent,
    killed it at once, and said "Agent running" after "Stopped". The
    supervisor looks for a stop before it makes the sandbox and before it
    starts the agent.
16. A sandbox's spec, which the next start finds it by, was written after
    its root: a crash in between left the root for good. The spec is
    written first. The model alone shows this one.
17. The specs were in `/var/run`, which the boot empties: any power cut,
    panic or empty battery left the roots, and what agents wrote in their
    `/work`, on disk for good. The specs live beside the roots in
    `/var/db/glasshouse/sandboxes`, and the start-up cleanup also removes
    roots without one (never one that still has anything mounted under
    it).
18. A start landing while glassd shut down wasn't stopped, and its agent
    ran on after glassd exited. Starts are refused once shutdown begins.
19. One old snapshot that couldn't be removed stopped every prune after
    it, day after day. The rest are removed.
- Checked by reading, not modelled: a prune never removes a snapshot an
  undo, take over or hand back could still use (each is in a task's
  snapshot list, which a prune keeps), and an undo whose snapshot is gone
  reports the file as not undone.

**Also, while checking:**
- Chat says a post whose answer was lost "may have been posted" as its own
  kind of failure (`ChatFailure::MaybePosted`); Meet keeps such lines to
  send again by that, not by the words.
- `glassctl verify` says when the log ends in a record not finished.
- `make e2e-vm`'s root checks look for sandbox roots left behind too.

### 3. Speed
- **The console's preview of a policy change** looked each member's groups
  up across every group, under the console's lock: 245 ms at 2,000 people
  and 6.2 s at 10,000 in a debug build. Each group is read once now:
  12 ms and 24 ms.
- **Start-up** reads nothing that grows with age: the audit log opens from
  its tail, and the seat and the cleanup are bounded. The day's snapshot
  prune runs in the background after start.
- **Noted:** the audit log's task index holds every task record's offset
  (about 8 MB at a million records), built once per run by a scan; ended
  tasks stay in glassd's memory until the person dismisses them, and
  starts and prunes look through them.

## Consequences
- **While the audit log can't take records** (a full disk), agents and
  apps can't read either, not only change things.
- **An app's call that waited can be refused** after a release: the person
  answered, and the call says IT's policy changed while it waited.
- **The console's API:** publish takes `previewed` (from the preview),
  promote takes `version`, and rollback answers `discarded_draft`.
- **Sandbox state** lives in `/var/db/glasshouse/sandboxes`, beside the
  roots, not `/var/run/glasshouse/jails`.
- **The sign-in screen keeps telling glassd** who signed in, every 2 s,
  until it hears.

## Not covered
- **Boot:** a managed computer's first boot (no setup; accounts from the
  directory aren't built); glassd isn't restarted by rc after a crash
  (`daemon` without `-r`), so a crash leaves the computer without agents
  until it restarts or someone starts glassd.
- **Policy:** a computer moved to a ring behind it keeps its newer version
  until the ring catches up; group membership changes reach computers
  only with the next publish, through the rings; support roles (ADR 0028);
  tailoring (a layer only ever raises a tier, under one lock).
- **Audit:** records waiting in memory when glassd crashes on a full disk
  are lost, as is the record of a call whose provider ran just before a
  crash (the model counts both); write-ahead records would change the
  format. Tampering is the unit tests'; streaming to a SIEM (AU-2) isn't
  built.
- **Containment:** attended tasks and app sessions have no sandbox; tool
  jails; a failing `jail -r` (reported, retried at the next start).

## Amended by ADR 0054
Both items it left open are closed:
- a change is on the record before it runs (`ActionStarted`, and
  `RollbackStarted` for an undo), so glassd stopping, on a full disk or
  not, can't leave it off; only its outcome, or glassd's own notes, can be
  lost (`specs/tla/AuditLog.tla`, `AU1_AChangeIsOnRecordBeforeItRuns`);
- support roles and the console's access by role are modelled
  (`specs/tla/SupportAccess.tla`), and the three bugs it found fixed.

## Amended by ADR 0055
glassd is started again after a crash, by `daemon -R` in rc.d/glassd; the
Boot model's restart step is the supervisor's, and
`ADR0055_GlassdComesBackAfterACrash` checks it.

## Amended 2026-11-10 (audit 4.7)
Two more ways the log's end can be wrong, neither of which stops it now:
- **A write that failed after its whole line landed** (the fsync failed).
  The caller was told the record failed and wrote it again; the first
  writing stayed in the file and was chained onto, so the record stood in
  the log twice, once never acknowledged. The writer now remembers where
  its last acknowledged record ends and, when it opens the file again,
  cuts back to there and notes it, as for a line cut short
  (`AU1_NoRecordTwice`; the model's `wholeLineKept` is the old behaviour).
- **A last record that doesn't match its own hash** (damage: the writer
  never leaves one). `open` was an error, so glassd refused to start at
  every restart with no way back but editing the file by hand. The
  damaged record is now set aside in a file beside the log
  (`audit.log.damaged-<time>`, root's alone), a `log_recovered` record
  says so with the file's name, and the chain goes on from the record
  before it; glassd logs a warning. This gives up nothing the chain
  protected: whoever can write the file can cut its end off without a
  trace, and here what was there is kept. Two damaged records at the end
  are more than a damaged end: glassd still refuses to start, and says
  what a person can do (`glassctl verify`, then move the log aside).

The log being one file for as long as the computer is used is the next
amendment's.

## Amended 2026-11-10 (audit 9.1): records share disk syncs

Each record was synced on its own, one after another under the runtime's
queue lock, whoever asked: with several agents at work the log was where
they all waited, on a lock the async workers weren't told they were
waiting on. Now:

- **`AuditLog::append_all` writes a run of records as one write and one
  sync.** A run is all in the log or, after a failure, none of it is: the
  writer cuts back to the last record it acknowledged when it opens the
  file again, whole lines of the failed run included, as it did for one
  record.
- **The runtime writes everything waiting as one run**
  (`Runtime::write_waiting`). Whoever finds nobody writing writes the
  queue, with its lock let go while the disk works; those who ask meanwhile
  wait for the next run, which carries all of theirs. Each caller still
  returns only once its own record is synced, and the waiting is where the
  runtime allows it (`blocking`).
- **Nothing about what waits and what is refused changes.** One queue, in
  the order asked: records the log couldn't take go first, in the same run
  as what follows them. A run that fails leaves every record of it
  waiting, stamped with when it happened, and the log down. A change
  whose start record was in a failed run doesn't run, and that it was
  refused waits in the start record's place (ADR 0054).

While a run lands, its earlier lines are whole in the file before any is
acknowledged, so `glassctl verify` and the console's count can see lines of
a run that then fails and is cut back. That was so for one record whose
fsync failed; it is so for a run's lines now. A task's records
(`task_records`) come from the index, which only ever has acknowledged
ones.

`specs/tla/AuditLog.tla` has runs: several calls' records landing in
order, acknowledged together or failed together, readers beside them, and
a crash with any part of a run on disk. Its promises hold; with the old
"whole line kept" behaviour switched on it finds a record written twice,
as it should.

Measured: 200 records from eight writers on a disk whose sync takes 5 ms
took 49 syncs and 0.27 s; one sync each is 200 and over a second. In the
VM the action budgets are as before (`glass-e2e perf`: a write with a
snapshot 167 ms median, 216 ms p95, against 750). A person's own changing
actions still take turns from snapshot to record (ADR 0051), so it is
reads, asks, state changes, apps' sessions and other people's tasks that
share syncs with them.

## Amended 2026-11-11 (audit 4.7): the log in segments, and how long records are kept

The log was one file for as long as the computer was used: it only grew,
and nothing could ever be removed from it without breaking the chain.

**Segments.** The log is written to one file, `audit.log`, as before. Once
that file is a month old or 32 MiB it is closed as a **segment**: renamed
beside the log as `audit.log.seg-<its first record's seq>`, and a new file
begun whose first record chains onto the segment's last.

- It happens under the writer's lock, between two runs of records, so a
  segment ends at a record that was acknowledged. Nothing in a segment
  changes again.
- glassd stopped between the rename and the new file leaves no file. The
  next start makes one and reads where the chain stands from the newest
  segment. (Before, an empty file meant "the log's first record".)
- A segment is never written over. If a file by its name is there, or the
  rename is refused, the file stays as it is and goes on taking records:
  nothing is lost and the log isn't stopped for it. It is closed at the
  first record after the way is clear, and glassd's log says why
  meanwhile, once a day. (Renamed and then unable to make the new file,
  there is nothing to write to: records are refused, fail closed, until
  there is.)
- Reading reads all of it as one chain: `glassctl verify`, the console's
  report of the last day (from the end back, through as many files as it
  takes) and a task's records. Each segment's index of its tasks is kept
  beside it (`.tasks`), worked out once: a task's records don't cost a
  read of the whole log after a restart. It is only a help to reading;
  every record read through it is checked against its own hash.

**How long records are kept.** A policy setting, `[org]
audit_retention_days`, which IT publishes from the console like any other
policy change (the preview says "audit records are kept on each computer
for N days, then removed"). IT sets it on the Everyone policy page, under
"How long each computer keeps things" (since 2026-11-11: until then the
setting was in policy and the console had no control for it, so it could
be changed only in the policy's text).

- **The default is 400 days**: thirteen months and a few days. A year is
  what an audit of a year's controls, and the common rules for audit
  trails, ask to be on hand; the rest is room for the audit itself.
- **0 keeps every record.** Anything else is at least 30 days, so nothing
  goes before it has been reported and looked at. An org that must keep
  records for years sets the number, or keeps them in its own store of
  them (AU-2) and leaves this alone.
- The setting is written into a policy only where it isn't the default.
  A policy that leaves it alone still reads on a computer whose OS is
  from before the setting; one that changes it is refused there, whole,
  as any policy with a setting the computer doesn't know is (audit 4.11).
  Change it once the computers have this OS.

**What removing does.** glassd looks as it starts and once a day. Whole
segments go, oldest first, each only once its newest record is older than
the rule; the file being written never does. So records are kept between
the rule and about a month longer.

- A `log_trimmed` record is written first, and on disk: how many records,
  through which one (`through_seq`), that record's hash, and the rule.
  Then the segments are removed, one at a time.
- **The log still verifies.** The oldest record kept must be the log's
  first, or one a `log_trimmed` record covers; where it is exactly the
  record after `through_seq`, it must chain onto the hash that record
  gives. A `log_trimmed` record is a record like any other: it can't be
  added or changed without breaking the chain from there on.
- **Removing records any other way still shows.** A segment deleted by
  hand leaves the log starting at a record nothing accounts for, or a
  break in the middle. `glassctl verify` says which.
- Stopped after the record and before the last segment went, the log
  verifies as it is, and the next pass removes the rest with a record of
  its own.
- `glassctl verify` says how many files the records are in and, once any
  have been removed, which record is the oldest kept.

**Reading beside the writer.** `verify` opens the file being written,
lists and opens the segments, and opens the file being written again; a
file renamed or removed meanwhile still reads to its end, and files are
told apart by what they are, not what they're called. If it then finds a
break and the files changed under it, it looks again, and reports only a
break that is there with the files at rest.

What this doesn't do: export records before they go (AU-2's stream to the
SIEM is still not started), or let IT set the rule per group: it is the
computer's log, and one rule.

Modelled as `specs/tla/AuditSegments.tla` (the writing of records into the
one file stays `AuditLog.tla`): the log on disk verifies at every step,
through closing a segment, a removal and glassd stopping anywhere; only
records past the rule go; `verify` reports no break that isn't there. Its
Old switches are the ways this was or could have been wrong: the chain
begun again on an empty file, segments removed before the record of it,
`verify` asking for exactly the record after `through_seq` (wrong while
segments go one by one, and for good after a stop part way; the tests
found it), and `verify` reporting a break it found while the files
changed.

