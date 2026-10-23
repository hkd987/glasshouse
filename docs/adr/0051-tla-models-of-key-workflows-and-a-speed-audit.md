# 0051. TLA+ models of key workflows, and a speed audit

Status: **Accepted, built** (2026-10-23). The owner: "in order to work out
the bugs let's use TLA+ to do formal verification on key workflows to
ensure they are fully working. Any bugs we find we need to fix, we also
need to do a full speed audit on all features to ensure the OS is as fast
as it can be."

## Context
- The workflows where a person's trust is at stake run concurrently:
  - an agent's calls race the person's answers, stop, pause, sleep and
    the agent's own exit;
  - undo races other tasks' writes;
  - routines run on a clock that restarts and steps;
  - releases from the console arrive replayed, forged or half-applied;
  - mail changes on the server and on this computer at once.
- Tests show the interleavings someone thought of. A model checker tries
  all of them.
- Speed was budgeted where a person waits on one thing: `glass-e2e perf`
  in the VM, apps' launches, the console, boot and redraws. Nothing
  measured what grows with a person's data or with the computer's age.

## Decision
### 1. TLA+ models, checked by TLC on every CI run
- **Where:** each model lives in `specs/tla` (conventions in its README).
- **What a model follows:** the code as it runs, step by step. Its header
  names the functions it models, and each action is one step the code
  takes under one lock.
- **What it checks:** what the OS promises, as invariants and properties
  named for PRD ids.
- **How it runs:** `tools/dev/tla.sh` fetches TLC (1.7.4, MIT, pinned by
  SHA-256) into `build/tools`, a build-time tool like mox. `make tla`
  checks every model in about a minute and is part of `make ci`. Without
  Java it says so and passes.
- **When a model fails:** a violation is a bug until shown otherwise.
  It's reproduced by a test that fails on the old code, then fixed, and
  the model changed to the fixed code. Where a model said "fixed" without
  the fix being needed, each fix was switched off one at a time to show
  it is.

| Model | States | TLC, one worker |
|---|---|---|
| Approvals (three configurations) | 565,171 / 377,835 / 32,444 | 19 / 11 / 5 s |
| Undo | 123,714 | 5 s |
| StudioVersions | 4,227 | 2 s |
| Routines | 846,984 | 20 s |
| ManagedSync | 8,785 | 2 s |
| MailReadState (two configurations) | 871,700 / 280,208 | 34 / 11 s |
| MailSend | 17,094 | 2 s |
| AppLiveUpdates | 32,395 | 3 s |

### 2. What the models found, and the fixes
**Approvals and the task lifecycle** (DA-11, DA-13, AW-12, PA-2, PM-5):
1. Waking restarted a task that was stopped, or that finished, while
   paused for sleep. Ending a task now clears its sleep mark, and pausing
   for sleep skips ended tasks.
2. An ended task's gate could reopen when a pause or resume landed while
   its agent was exiting, or through the timed-out ask's hold. The gate
   never leaves Ended now, and hold and release run under the task's
   lock.
3. An ask could open after its task ended, leaving a phantom approval and
   a grant that outlived the task (PA-2). The desk remembers the tasks it
   ended and withdraws such an ask at once.
4. The tile said Working while another ask was still open (AW-12). It
   changes to Working only when no ask is open, read under the task's
   lock.
5. A resume landing while the computer went to sleep ran the agent before
   it slept (PM-5). While going to sleep, a released task waits for the
   wake.

**Undo** (AU-21, ADR 0017), where an undo could silently take back
another task's change:
6. The check-then-write race: undo checked a file, another task wrote
   it, and undo restored over that write.
7. The gap from a step's snapshot to its write.
8. The gap from its write to its record.
9. An Auto step reused its task's start snapshot after another task had
   changed the file.
10. An undo's own restore was noted as the task's version of the file, so
   undoing an earlier change then wiped another task's later one.
11. Undoing an undone change ran the provider's revert again (a mail
   move moved back twice).
   - **Fixes for 6–8, and for 7 in the studio below:** a person's
     reversible steps across their tasks, and undos, take turns
     (`writing_turn`).
   - **Fix for 9:** a snapshot stops covering once another task changed
     something under it.
   - **Fix for 10:** each change keeps how it left its files.
   - **Fix for 11:** undone changes are skipped.

**Studio versions** (GA-3):
12. Two remixes made at once became one app's two versions. A revert
   could reverse an undo of a save. Both are fixed by the same turns.

**Routines** (DA-4, ADR 0047):
13. Run now twice, or the schedule during a run, ran a routine twice at
   once. One run at a time now; a second start is refused, with the
   reason.
14. A restart, the clock stepping back, or the end of daylight saving,
   inside a minute a routine started, started it again. Each scheduled
   minute is claimed on disk.
15. A pause just after the scheduler read the routine didn't stop that
   minute's run. The scheduled start rechecks under the store's lock.
16. A run recorded one version and ran the next when a new one was saved
   before it read the routine. A run reads the version it started with.
17. A pause and a save at once lost one of them. The choices file has one
   lock now, and a version is numbered and chosen in one step.
18. A run cut off by a restart was never recorded. It's recorded as
   failed: "This computer restarted during the run."

**What the console sends a managed computer** (DV-1, DV-10, ADR 0023):
19. A refused release was fetched and refused again on every sync, with
   no pause, flooding the audit log.
   - A properly signed release with bad content is asked past now.
   - Every other refusal backs off and retries. TLC caught a first fix
     that let a forger move what the computer asks for.
20. Rollback: with a release kept but not yet in force when glassd
   stopped, a failed write at the next start forgot the version, and a
   replayed older release was applied. A kept release now sets the floor
   and goes in force from disk.

**Mail's read state** (EM-3, ADR 0050):
21. A mark of mail the phone had filed away went to a UID the server no
   longer had, which answered OK. `mark` now says which UIDs it found, and
   the person hears the message isn't there any more.
22. After a folder was renumbered (a new UIDVALIDITY), a mark or a move
   could land on another message. Both name the numbering they mean; the
   server refuses a mismatch with "list it again".
23. An undone mark was lost when a sync came between the undo's restore
   and its hook. `revert` is told the change's kind, and sends what the
   undo put back.
24. An older mark still waiting overrode a newer one once it reached the
   server. A mark that reaches the server forgets the one waiting.
25. A sync could run between a move on the server and its rename here.
   Moves hold the person's lock.
26. A mark still waiting was lost when its message was filed. It goes to
   the server before the move.
27. Undoing a move brought the message back unread. A move's change names
   the read markers too.
28. An undo the server refused left the files restored here, showing a
   message where the server didn't have it. They're put back as the move
   left them.
29. The quiet sync's shortcut never repaired a cache that had lost or
   gained a file. It also requires the count kept to match.

**Sending** (DP-30, ADR 0037):
30. A draft that waited and was then sent stayed in Drafts, to be sent
   again. A sent draft leaves Drafts.
31. Each failed send of a draft made another copy. The same draft is
   kept.
32. Two calls, or one call naming it twice, could send one draft twice. A
   draft is taken by one call at a time.
33. Editing a draft never replaced it. `mail.draft` naming the draft
   replaces it, as one undoable change.
34. When the server took the whole message and its answer was lost, Mail
   said "Nothing left this computer" and kept it to send again: a
   message sent twice in good faith. It now says the message may have
   been sent and to check before sending it again. It keeps it in Drafts
   in case it didn't go, and records it as maybe sent. This isn't an
   error, which an agent would retry (`LinkError::Unknown`).

**Live updates to open apps** (NV-3, ADR 0030):
35. After glassd restarted, open apps heard nothing: its change counter
   began again at 0, below every page's cursor. It begins at the time now,
   and a cursor from an earlier run counts as news.
36. All five apps that listen read their cursor after their first load,
   missing a change in between. They read it first.
37. Chat and Artifacts dropped a change that came while a post or
   comment of the page's own was under way. Mail never started listening
   when glassd didn't answer as it opened.
38. What an agent drafted, sent, filed or marked didn't show in an open
   Mail; only a sync woke it. Each mail action that changes something
   wakes it now.

### 3. The speed audit
- **Method:** a native benchmark of every feature an agent or app calls,
  through the whole path (task socket, policy, audit record, snapshot,
  provider), on a person's worth of data. That's a 500-message inbox, a
  2,000-file folder, a 300-meeting month and a 2,000-person directory
  (`crates/svc/runtime/tests/speed.rs`, run by hand with `--profile vm`).
  The VM's budgets measured before and after.
- **What grew with the data or the computer's age, and is fixed:**
  - Mail read the start of every message in a folder on every listing,
    even for one (500 messages: 100 ms in the debug build, for 50 listed
    or for one).
    - It keeps what it parsed, by file, while the file's size and time
      stay the same. A listing reads the names and parses only what's
      new or changed: 10.9 ms for 50 and 5.6 ms for a count.
    - Synced files never change, and a draft rewritten changes its time.
  - Viewing a task's audit read, parsed and hash-checked the whole log,
    every user's, from the computer's first day. The log now keeps an
    index of where each task's records are, built once and kept up by
    every append.
  - glassd's report to the console, every minute, counted the last day's
    blocked actions by reading the whole log. It reads from the end back
    now, and stops at the first record older than a day.
- **What the desktop agent waits on:** each step waits on the model.
  Asked to read four files, Claude Haiku 5.5 already read them in one step
  (three model calls). An instruction to do that changed nothing, so the
  prompt is unchanged.

## Consequences
- **Writes take turns:** a person's reversible steps across their tasks
  take turns, the provider's run time included (an IMAP move holds the
  turn), and an undo holds the turn through its revert hooks. Reads
  don't take turns.
- **The limit no lock can fix:** writers glassd doesn't run (the person's
  editor, other programs) can still land between a snapshot and a write.
  AU-20 promises snapshots before Notify steps only, and the Undo model
  leaves these writers out of the invariant, saying so.
- **Routines:** a second start of a running routine is refused.
- **Models change with the code:** a change to a modelled workflow
  changes its model, and `make tla` says when they disagree.

## Not covered
- **Approvals:** tasks starting during the sleep window, several tasks
  at once, attended tasks, retry and dismiss.
- **Routines and managed sync:** rings and the console's own logic. Disk
  damage together with a replay is assumed away.
