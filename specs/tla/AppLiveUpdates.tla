--------------------------- MODULE AppLiveUpdates ---------------------------
(***************************************************************************)
(* An open app hearing of changes to the person's data in it (ADR 0030     *)
(* "live updates", ADR 0037, ADR 0047; NV-3): `app.status` waits with a    *)
(* cursor, glassd's `app_changed`, and the page's loop.                    *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/svc/runtime/src/apps.rs  AppChanges, app_changed, app_status,  *)
(*                                   app_status_wait                       *)
(*   crates/bin/glassd/src/main.rs   chat's and mail's notices become      *)
(*                                   app_changed, after the data is kept   *)
(*   crates/bin/glass-app-bridge     a session glassd lost (restarted) is  *)
(*                                   opened again, the wait relayed as is  *)
(*   apps/{mail,chat,docs,artifacts,routines}/src  start (the first load   *)
(*                                   and cursor) and the live loop         *)
(*                                                                         *)
(* One person, one app, one page. Someone else changes the data (a        *)
(* colleague's message, an agent, a run): the data is written, then       *)
(* app_changed runs. Other apps' changes move the counter too. glassd may  *)
(* stop and start again; a wait in flight then fails and the page tries    *)
(* again. The page may be busy with its own action (Chat posting,         *)
(* Artifacts commenting, Routines acting).                                *)
(*                                                                         *)
(* Old is the set of the code's behaviours before ADR 0051 that a          *)
(* configuration keeps; {} is the code now.                                *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    MaxChanges,   \* changes to the person's data in the app
    MaxOther,     \* changes in other apps
    MaxRestarts,  \* times glassd stops and starts again
    MaxBusy,      \* the page's own actions
    Old           \* "cursorAfterLoad": every app read its cursor after its
                  \*   first load;
                  \* "skipWhileBusy": Chat and Artifacts dropped a change
                  \*   that came while they were busy;
                  \* "stopWhenDown": Mail never waited again if glassd
                  \*   didn't answer as it opened;
                  \* "counterResets": a restarted glassd counted from 0

VARIABLES
    ver,        \* the person's data in the app: 1, then one more a change
    unsaid,     \* changes written whose app_changed hasn't run
    up,         \* glassd running
    seq,        \* AppChanges.seq
    last,       \* AppChanges.last for this person and app (0: none this run)
    base,       \* where this run's counter started
    shown,      \* the data the page shows (its last load), 0 for none
    cursor,     \* the page's cursor
    pc,         \* the page's step
    since,      \* the wait in flight: its `since`
    answer,     \* glassd's answer: [cursor, changed]
    busy,       \* the page's own action in flight
    changes, others, restarts, acts

vars == <<ver, unsaid, up, seq, last, base, shown, cursor, pc, since, answer,
          busy, changes, others, restarts, acts>>

Init ==
    /\ ver = 1 /\ unsaid = 0
    /\ up = TRUE /\ seq = 1 /\ last = 0 /\ base = 1
    /\ shown = 0 /\ cursor = 0
    /\ pc = IF "cursorAfterLoad" \in Old THEN "load0" ELSE "cursor0"
    /\ since = 0 /\ answer = [cursor |-> 0, changed |-> FALSE]
    /\ busy = FALSE
    /\ changes = 0 /\ others = 0 /\ restarts = 0 /\ acts = 0

-----------------------------------------------------------------------------
\* glassd and the world.

\* Someone else changes the data; glassd keeps it, then says so.
Write ==
    /\ up /\ changes < MaxChanges
    /\ ver' = ver + 1 /\ unsaid' = unsaid + 1 /\ changes' = changes + 1
    /\ UNCHANGED <<up, seq, last, base, shown, cursor, pc, since, answer, busy,
                   others, restarts, acts>>

\* app_changed: the counter moves on and remembers this app's change.
Notify ==
    /\ up /\ unsaid > 0
    /\ unsaid' = unsaid - 1
    /\ seq' = seq + 1 /\ last' = seq + 1
    /\ UNCHANGED <<ver, up, base, shown, cursor, pc, since, answer, busy,
                   changes, others, restarts, acts>>

Other ==
    /\ up /\ others < MaxOther
    /\ seq' = seq + 1 /\ others' = others + 1
    /\ UNCHANGED <<ver, unsaid, up, last, base, shown, cursor, pc, since,
                   answer, busy, changes, restarts, acts>>

\* glassd stops: what it hadn't said yet is never said, and a wait in
\* flight fails (the page tries again in 3 s).
Stop ==
    /\ up /\ restarts < MaxRestarts
    /\ up' = FALSE /\ unsaid' = 0 /\ restarts' = restarts + 1
    /\ pc' = IF pc \in {"waiting", "answering"} THEN "wait" ELSE pc
    /\ UNCHANGED <<ver, seq, last, base, shown, cursor, since, answer, busy,
                   changes, others, acts>>

\* It starts again with nothing in memory. Before the fix its counter
\* began at 0; now at the time in microseconds, past every cursor an
\* earlier run gave (the clock has moved on).
Start ==
    /\ ~up /\ up' = TRUE
    /\ last' = 0
    /\ IF "counterResets" \in Old
       THEN /\ seq' = 1 /\ base' = 1
       ELSE /\ seq' = seq + 1 /\ base' = seq + 1
    /\ UNCHANGED <<ver, unsaid, shown, cursor, pc, since, answer, busy, changes,
                   others, restarts, acts>>

-----------------------------------------------------------------------------
\* The page.

\* Its first cursor (app.status without a wait). Fixed: before the first
\* load, so a change between the two is after the cursor. Mail before the
\* fix waited for nothing more when glassd didn't answer here.
Cursor0 ==
    /\ pc = "cursor0"
    /\ IF up THEN cursor' = seq ELSE UNCHANGED cursor
    /\ pc' = IF ~up /\ "stopWhenDown" \in Old THEN "stopped"
             ELSE IF "cursorAfterLoad" \in Old THEN "wait"
             ELSE "load0"
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, shown, since, answer, busy,
                   changes, others, restarts, acts>>

\* A load reads the data as it is; with glassd down it shows what it had
\* (Mail, kept on the device) or nothing.
Read == IF up THEN shown' = ver ELSE UNCHANGED shown

Load0 ==
    /\ pc = "load0"
    /\ Read
    /\ pc' = IF "cursorAfterLoad" \in Old THEN "cursor0" ELSE "wait"
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, cursor, since, answer,
                   busy, changes, others, restarts, acts>>

\* app.status with wait_secs and since.
Ask ==
    /\ pc = "wait" /\ up
    /\ since' = cursor /\ pc' = "waiting"
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, shown, cursor, answer,
                   busy, changes, others, restarts, acts>>

\* glassd answers, on a change after `since` or after 25 s: app_status's
\* cursor first, then whether this app changed after `since`. Fixed: a
\* cursor from before this run (or none) is news, since what changed
\* meanwhile isn't in this run's memory.
Answer1 ==
    /\ pc = "waiting" /\ up
    /\ answer' = [answer EXCEPT !.cursor = seq]
    /\ pc' = "answering"
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, shown, cursor, since,
                   busy, changes, others, restarts, acts>>

Answer2 ==
    /\ pc = "answering" /\ up
    /\ answer' = [answer EXCEPT !.changed =
                     \/ last > since
                     \/ ("counterResets" \notin Old /\ since < base)]
    /\ pc' = "got"
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, shown, cursor, since,
                   busy, changes, others, restarts, acts>>

\* The page takes the cursor; a change means a load, once the page's own
\* action is done (before the fix Chat and Artifacts dropped it).
Got ==
    /\ pc = "got"
    /\ cursor' = answer.cursor
    /\ pc' = IF ~answer.changed THEN "wait"
             ELSE IF ~busy THEN "load"
             ELSE IF "skipWhileBusy" \in Old THEN "wait"
             ELSE "settle"
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, shown, since, answer, busy,
                   changes, others, restarts, acts>>

Settle ==
    /\ pc = "settle" /\ ~busy /\ pc' = "load"
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, shown, cursor, since,
                   answer, busy, changes, others, restarts, acts>>

Load ==
    /\ pc = "load"
    /\ Read
    /\ pc' = "wait"
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, cursor, since, answer,
                   busy, changes, others, restarts, acts>>

\* The page's own action: Chat's post, say, waiting for approval. It ends
\* with a load when it went (Chat loads the conversation), or with none.
Act ==
    /\ ~busy /\ acts < MaxBusy /\ pc # "stopped"
    /\ busy' = TRUE /\ acts' = acts + 1
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, shown, cursor, pc, since,
                   answer, changes, others, restarts>>

Done ==
    /\ busy /\ busy' = FALSE
    /\ \/ Read
       \/ UNCHANGED shown
    /\ UNCHANGED <<ver, unsaid, up, seq, last, base, cursor, pc, since, answer,
                   changes, others, restarts, acts>>

Next ==
    \/ Write \/ Notify \/ Other \/ Stop \/ Start
    \/ Cursor0 \/ Load0 \/ Ask \/ Answer1 \/ Answer2 \/ Got \/ Settle \/ Load
    \/ Act \/ Done

Spec == Init /\ [][Next]_vars

\* glassd says what it keeps and comes back; the page goes on; its own
\* action ends.
Fairness ==
    /\ WF_vars(Notify) /\ WF_vars(Start)
    /\ WF_vars(Cursor0) /\ WF_vars(Load0) /\ WF_vars(Ask) /\ WF_vars(Answer1)
    /\ WF_vars(Answer2) /\ WF_vars(Got) /\ WF_vars(Settle) /\ WF_vars(Load)
    /\ WF_vars(Done)

LiveSpec == Spec /\ Fairness

-----------------------------------------------------------------------------
\* NV-3 (ADR 0030): an open app comes to show the person's data as it is,
\* whatever happened meanwhile, and stays so until it changes again.
NV3_TheOpenAppCatchesUp == <>[](shown = ver)

=============================================================================
