# 0047. Routines, an app to see what runs for you

Status: **Accepted, built** (2026-10-23). The owner: "we also need a way
for users to see their routines maybe a routines app on our os". PRD DA-4
(routines), NV-21 (replay code). Builds on ADR 0044 (routines), ADR 0046
(the code this computer writes) and ADR 0024 (apps).

## Context
- Routines had no screen. A person saw them only through `glassctl
  routines`, or by asking their agent. They couldn't see when one would
  run next, how it last went, or the code that runs for them, and
  couldn't run, pause or reschedule one themselves.
- ADR 0046 notices a request done the same way twice, but says so only in
  the task's activity, with a `glassctl` command.
- Starting a routine runs a task with the scope it was saved with. An
  agent that could start one could reach what its own task can't.

## Decision
1. **A built-in app, Routines** (`apps/routines`), in everyone's apps. It
   keeps nothing: every view is an action, every change one too, through
   the bridge and the audit log, by Routines for the person.
   - **Your routines:**
     - each with what it does and when it runs: "Runs every Friday at
       17:00 · next today at 17:00", or "Paused";
     - how it last ran: when, whether you or its schedule started it,
       "Ran with no model" or "The agent stepped in", and what it said;
     - Run now, and Pause or Resume.
   - **Opened:**
     - the request, and when it runs (Change, Pause);
     - what it reads and changes and the tools it calls;
     - the code itself, line by line, with the note of who wrote it;
     - its runs, and Remove (it asks first; undo puts it back).
   - **Done the same way more than once:** each repeat ADR 0046 noticed,
     with its steps and code, and Keep as a routine (a name, and when it
     runs) or Not now.
2. **New actions in `glass-app-routines`:**
   - `routines.pause` (Notify): pauses a routine's schedule, or resumes
     it. It's the person's choice, in their routines file, undoable.
   - `routines.schedule` (Ask): saves the routine's next version with a
     new schedule, and its script and scope as they were.
   - `routines.run`: starts it now, as `glassctl routine run` does.
   - `routines.repeats`, `routines.keep`, `routines.dismiss`: the
     repeats; keeping one, with the code this computer wrote; and no
     longer offering one until it's done the same way twice again.
   - `routines.use` (Notify): makes another of a routine's saved versions
     the one that runs, such as the one before a change that didn't work
     out. Each was approved when it was saved, so it's a choice among
     them, in the person's routines file, undoable, as pausing is. Its
     notice says what changes: when it runs, and whether its code does.
   - `routines.read` lists a routine's versions (newest first, what each
     was, when it runs, which one is in use) and, with `version`, reads
     one of them, its code included.
   - The provider reaches the runtime through `glass_app_routines::Host`,
     which glassd sets.
3. **Starting and keeping are the person's own.** `routines.run`,
   `routines.repeats`, `routines.keep` and `routines.dismiss` are in
   `PERSONS_OWN`:
   - Never for every agent, whatever its rules (the desktop agent's
     `routines.*` included). Pausing and rescheduling stay the agent's,
     at Notify and Ask.
   - The pipeline refuses a people-only or person's-own action before the
     provider sees the agent's input at all. Today it ran the preview
     first, so an agent's input reached code that reads the person's
     routines.
   - Routines' `[[app]]` tiers make the person's own calls Auto: their
     own act, on the record.
4. **Runs are kept, and paused routines skip their schedule.**
   - At a routine run's end, glassd keeps how it went beside the
     routine: root only, the last 20 (`runs.json`).
   - A paused routine runs only when started.
   - When one next runs comes from its schedule in the computer's local
     time (`Schedule::minutes_until`).
5. **Only agents that run routines get repeats.** A repeat is offered,
   and `glassctl routine save` accepted, only for an agent that policy
   lets read routines and that runs by itself. The person's own harness
   (`claude-code`) waits for them, and can't run one.

## Consequences
- The routines the desktop agent saves, and the repeats it noticed, are
  on a screen, as code the person can read before it runs for them.
- A person can stop a routine for a while without removing it, and see
  when the agent had to step in (a script that broke on new mail).
- The apps suite's dev glassd has the scripted model
  (`GLASS_DEV_SCRIPTED_MODEL`), so the desktop agent can do a request in
  it: `routines.spec.ts` has it draft the same email twice, then keeps,
  runs, reschedules, pauses and removes it in Routines.
- Cold launch 78 ms, 95 MB (UI-7).
- **Live on Claude Haiku 5.5** (2026-10-23), on the VM's desktop:
  - Haiku archived the Vendor News newsletters two weeks running, for
    $0.0013 and $0.0007.
  - Routines offered the request with the code this computer wrote (list
    the Inbox, move the messages from Vendor News).
  - Kept as "Archive newsletters", every Friday at 17:00, it ran on week
    three's mail with no model call: 2 moved, a colleague's left.
  - Typing a time a key at a time lost the field to a redraw. The picker
    now keeps what's typed, and the apps test types it that way.

6. **Routines hears of changes; it never asks again on a timer.** glassd
   wakes the person's Routines windows (`app.status`, ADR 0030's waits)
   when a routine's run starts or ends, wherever it was started (its
   schedule, `glassctl`, the agent), when a request is done the same way
   twice, and when any `routines.*` call changes a routine. `routines.list`
   and `routines.read` say which routine is running now, and its run.
7. **Versions, and going back.** Opened, a routine lists its versions;
   each one not in use shows its changes against the one in use (when it
   runs, then its code, line by line, added and removed lines marked) and
   "Go back to this version".

## Consequences, added (2026-10-23)
- A scheduled run shows as running, then how it went, in an open
  Routines, as it happens. The page's 2 s polling while a run it started
  was going is gone.
- A change the person regrets is one click back, without their agent or
  an approval: the version they go back to was approved when saved.
- Tests: `glass-app-routines`
  `adr0047_a_routine_goes_back_to_an_earlier_version_and_any_version_reads`,
  `adr0047_routines_windows_hear_of_every_change_and_see_what_runs`;
  runtime `flows::adr0047_the_person_keeps_runs_pauses_and_reschedules_…`
  (the window wakes at a run's start and end, and after going back);
  `routines.spec.ts` (a run started with `glassctl` shows in the open
  page; version 1 against version 2, then back to it); the line diff in
  `routines.test.ts`.

## Not built yet
- Editing a routine's code in the app. A new version still comes from the
  agent, with an approval.
- Removing a version by hand. Versions are never changed, and since the
  audit of 2026-11 (5.3) the old ones go by themselves: a version stays
  while it is one of the routine's newest 50 (what `routines.read`
  lists), the one in use, or was in use in the last three months, which
  is what undoing a save, a going back or a removal can name
  (`Routines::moved`; the rule and its model are ADR 0032's amendment of
  2026-11-11). A removed routine's last versions stay: undo puts it back,
  and nothing removes the folder after.
