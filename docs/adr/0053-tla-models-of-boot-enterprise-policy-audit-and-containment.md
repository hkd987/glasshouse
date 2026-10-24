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
