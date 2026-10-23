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

## Not built yet
- Editing a routine's code in the app. A new version still comes from the
  agent, with an approval.
- Hearing of a run's end without asking: the page asks again every 2 s
  while a run it started is going, and when it comes back to the front.
- A routine's versions side by side.
