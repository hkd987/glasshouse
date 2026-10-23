# TLA+ models of key workflows

Each model is the workflow as the code runs it, checked by TLC
(ADR 0051): `make tla`, or `tools/dev/tla.sh check <Name>` for one.
`tools/dev/tla.sh` fetches TLC (pinned, MIT) into `build/tools/`; it runs
on Java 11 or later and the check passes, saying so, without Java.

## Conventions

- `<Name>.tla` and `<Name>.cfg` side by side. Several configurations of
  one model are `<Name>_<case>.cfg`, all checking `<Name>.tla`.
- A model follows the code, not the design: its header names the files
  and functions it models (`crates/svc/broker/src/desk.rs`, `answer`),
  and each action says which code step it is. One atomic action is one
  step the code takes under one lock or in one call that can't
  interleave; the rest interleaves.
- What the OS promises is an invariant or a property, named for its PRD
  id or ADR where there is one (`PA2_NothingOutlivesTheTask`).
- The `.cfg` constants are as small as still shows the interleavings
  (two tasks, two messages): `make tla` checks every model in about a
  minute.
- A violation is a bug until shown otherwise: reproduced by a test in the
  code, then fixed, and the model changed to the fixed code. A model
  that was wrong is fixed instead, and says why.
- Plain TLA+ (no PlusCal), ASCII, standard modules only.

## Models

| Model | Workflow | Code |
|---|---|---|
| [Approvals](Approvals.tla) (`_app`, `_liveness`) | Asks and answers, grants for a task, the timeout, pause, take over, resume, hand back and stop, the agent exiting, sleep and wake (DA-11, DA-13, AW-12, PA-2, PM-5) | `crates/svc/broker/src/desk.rs`; `crates/svc/runtime/src/execute.rs` (the call's path, the ask), `runtime.rs` (`hold`, `release`, `end_task`, `pause_for_sleep`, `resume_after_sleep`), `supervise.rs`, `task.rs` |
| [Undo](Undo.tla) | Undo of a whole task, one change or to a point, while two tasks and the person write the same file: turns, a fresh snapshot for an Auto step after another task's change, undone changes skipped, each file judged by the change that still stands (AU-21, ADR 0017) | `crates/svc/runtime/src/execute.rs` (`call_tool`), `undo.rs`, `written.rs` |
| [StudioVersions](StudioVersions.tla) | A person's app versions and `mine.json` while two tasks save, remix, revert and undo (GA-3, ADR 0032) | `crates/apps/studio/src/lib.rs` (`plan_save`, `save`, `revert`), `store.rs`; the runtime's turns |
| [Routines](Routines.tla) | Routines started on their schedule or by hand while the person saves, uses, pauses and removes them, a run reading its routine, glassd restarting and the clock stepping back (DA-4, ADR 0047) | `crates/apps/routines/src/{store,lib}.rs`, `crates/svc/runtime/src/routines.rs`, `crates/bin/glassd/src/main.rs` (the scheduler) |
| [ManagedSync](ManagedSync.tla) | Signed releases from the admin console fetched, checked, kept and swapped in through restarts, a failing disk, replayed and forged releases and an org key rotation (DV-1, DV-10, ADR 0021, ADR 0023) | `crates/bin/glassd/src/managed.rs`, `crates/lib/managed/src/{signing,store}.rs` |
| [MailReadState](MailReadState.tla) (`_reset`) | Read state kept in step with the mail server through syncs, marks, moves, undo, the phone, renumbered folders and the network (EM-3, IN-2, ADR 0050) | `crates/apps/mail/src/{handle,marks,moves,cache}.rs`, `crates/svc/gateway-providers/src/mail/mod.rs` (`sync`, `mark`, `move_to`), `crates/svc/runtime/src/undo.rs` (restore, then `revert`) |
| [MailSend](MailSend.tla) | A send that can't leave waits in Drafts and is sent once, never twice: offline sends, drafts sent and saved again, two calls at once, a lost reply said plainly (DP-30, ADR 0037) | `crates/apps/mail/src/lib.rs` (`send`, `keep_unsent`, `draft`), `handle.rs` (`take_drafts`), `apps/mail/src/main.ts` |
| [AppLiveUpdates](AppLiveUpdates.tla) | An open app hears of changes to the person's data: `app.status` waits, `app_changed`, glassd restarting, the page's first load and live loop (NV-3, ADR 0030, ADR 0047) | `crates/svc/runtime/src/apps.rs`, `crates/bin/glass-app-bridge`, `apps/{mail,chat,docs,artifacts,routines}/src` |
