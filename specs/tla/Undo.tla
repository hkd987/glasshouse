-------------------------------- MODULE Undo --------------------------------
(***************************************************************************)
(* Undo (AU-20, AU-21, AW-15, ADR 0017) as the runtime runs it, with two  *)
(* tasks of one person and the person themselves writing the same file.   *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/svc/runtime/src/execute.rs, `call_tool`: a reversible action  *)
(*    takes a snapshot first when its tier is Notify or above, or when no  *)
(*    snapshot of the task covers its roots (`Task::covered`); counts the  *)
(*    task's snapshots (`before`); invokes the provider; right after,      *)
(*    reads how it left each changed file (`written::look`); then records  *)
(*    the change. StartAction, Write, Record.                              *)
(*  - crates/svc/runtime/src/undo.rs: `plan` (under the task's lock: the   *)
(*    changes undone; for each file the latest snapshot from before the    *)
(*    first of them that touched it; whether a change this undo keeps      *)
(*    touched it after that snapshot), `apply` (per file, `step` compares  *)
(*    the snapshot's copy, the file now and how the task left it, then     *)
(*    `restore` writes), and the end of `undo` (a change with a file left  *)
(*    alone stays undoable; `revert` hooks; the rest marked undone; how    *)
(*    undo left the files noted). UndoPlan, UndoCheck, UndoAct, UndoFinish.*)
(*                                                                         *)
(* Abstractions: a snapshot covers every file; a change touches one file;  *)
(* files are never removed; each write makes a value nobody else writes,   *)
(* so "the same bytes" means the same write; a task runs one action at a   *)
(* time (a task's own concurrent actions are what `kept` is for). The     *)
(* person stands for every writer glassd doesn't run: their editor, other  *)
(* programs, a mail sync.                                                  *)
(*                                                                         *)
(* Fixed = FALSE is the code before ADR 0051; TRUE is the code now:        *)
(*  1. A person's reversible actions and undos take turns, from snapshot   *)
(*     to record (`Runtime::writing`): nothing of another task's lands     *)
(*     between a task's snapshot and its write, between its write and     *)
(*     `look`, or between undo's check of a file and its write.            *)
(*  2. A snapshot stops covering a task's roots once another task          *)
(*     recorded a change after it, so an Auto step snapshots afresh.       *)
(*  3. Undo skips changes already undone: their revert hooks don't run     *)
(*     again.                                                              *)
(*  4. Undo compares a file with how the task's latest change to it still  *)
(*     standing left it (`Written::left_after`), not with how the task's   *)
(*     last undo left it: put back to another task's version, it isn't     *)
(*     this task's to take further back.                                   *)
(*                                                                         *)
(* glassd stopping and starting again (ADR 0020, `kept.rs`): each task's   *)
(* snapshots, changes and how it left its files are written to disk        *)
(* (Keep), at any step after a change, so the record can be behind its     *)
(* task; an undo's end writes it in the same step. Restart is glassd       *)
(* stopping at any step, with actions and undos in flight, and coming back *)
(* with every task ended and as its record says. The promises below hold   *)
(* through it: a record that is behind only leaves more files alone.       *)
(* Not modelled: glassd stopping between an undo's last step and its       *)
(* record's write, a few milliseconds in `undo`. That undo is offered      *)
(* again after the restart; it finds the files other than the task left    *)
(* them and leaves them, and a revert hook may be asked a second time.     *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Tasks,          \* the person's tasks
    Files,          \* files their actions change
    MaxActions,     \* actions per task
    MaxEdits,       \* the person's own edits, outside any task
    MaxUndos,       \* undo requests, all tasks together
    RevertMayFail,  \* a provider's revert hook may fail
    MaxRestarts,    \* glassd stopping (a crash, a power cut) and starting again
    Fixed           \* the code with ADR 0051's fixes

Person == "person"
Untouched == [by |-> "untouched", n |-> 0] \* `Written::left` is None
Original == [by |-> "original", n |-> 0]
None == "none"
NoValue == [by |-> "no value", n |-> 0]

VARIABLES
    fs,       \* each file's value now
    snaps,    \* each task's snapshots, oldest first: [at, copy]
    chg,      \* each task's changes, in order
    left,     \* how each task left each file (`Written::files`)
    act,      \* each task's action in flight
    undos,    \* undo requests in flight
    clock,    \* one clock for every write, snapshot and record
    count,    \* actions, edits and undos so far
    lock,     \* Fixed: the task or undo holding the person's turn, or None
    bad,      \* what went wrong, for the invariants
    disk      \* each task's record on disk (`kept.rs`): snaps, chg and left
              \* as they were when it was last written

vars == <<fs, snaps, chg, left, act, undos, clock, count, lock, bad, disk>>

Idle == [phase |-> "idle"]

Init ==
    /\ fs = [f \in Files |-> Original]
    /\ snaps = [t \in Tasks |-> <<>>]
    /\ chg = [t \in Tasks |-> <<>>]
    /\ left = [t \in Tasks |-> [f \in Files |-> Untouched]]
    /\ act = [t \in Tasks |-> Idle]
    /\ undos = {}
    /\ clock = 1
    /\ count = [a |-> [t \in Tasks |-> 0], edits |-> 0, undos |-> 0, restarts |-> 0]
    /\ lock = None
    /\ bad = {}
    /\ disk = [t \in Tasks |-> [snaps |-> <<>>, chg |-> <<>>,
                                 left |-> [f \in Files |-> Untouched]]]

Undoing(t) == \E u \in undos : u.task = t

Free == ~Fixed \/ lock = None
Take(who) == IF Fixed THEN lock' = who ELSE UNCHANGED lock
Give == IF Fixed THEN lock' = None ELSE UNCHANGED lock

\* Another task recorded a change after t's latest snapshot.
ForeignSince(t) ==
    snaps[t] # <<>> /\
    \E o \in Tasks \ {t} : \E i \in 1..Len(chg[o]) :
        chg[o][i].at > snaps[t][Len(snaps[t])].at

(***************************************************************************)
(* An action of task t on file f: Notify-tier steps snapshot first, Auto   *)
(* steps only when nothing covers them.                                    *)
(***************************************************************************)
StartAction(t, f, notify) ==
    /\ act[t] = Idle
    /\ ~Undoing(t)                 \* undo holds the task's gate
    /\ count.a[t] < MaxActions
    /\ Free
    /\ LET fresh == notify \/ snaps[t] = <<>> \/ (Fixed /\ ForeignSince(t))
           s2 == IF fresh THEN Append(snaps[t], [at |-> clock, copy |-> fs])
                 ELSE snaps[t]
       IN /\ snaps' = [snaps EXCEPT ![t] = s2]
          /\ act' = [act EXCEPT ![t] = [phase |-> "snapped", file |-> f,
                        before |-> Len(s2), prev |-> NoValue, wrote |-> NoValue]]
    /\ clock' = clock + 1
    /\ count' = [count EXCEPT !.a[t] = @ + 1]
    /\ Take(t)
    /\ UNCHANGED <<fs, chg, left, undos, bad, disk>>

\* The provider writes.
Write(t) ==
    /\ act[t].phase = "snapped"
    /\ LET f == act[t].file
           v == [by |-> t, n |-> clock]
       IN /\ fs' = [fs EXCEPT ![f] = v]
          /\ act' = [act EXCEPT ![t].phase = "wrote", ![t].prev = fs[f],
                                ![t].wrote = v]
    /\ clock' = clock + 1
    /\ UNCHANGED <<snaps, chg, left, undos, count, lock, bad, disk>>

\* `written::look` right after the provider returns, then the record.
Record(t) ==
    /\ act[t].phase = "wrote"
    /\ LET a == act[t] IN
       /\ left' = [left EXCEPT ![t][a.file] = fs[a.file]]
       /\ chg' = [chg EXCEPT ![t] = Append(@,
                 [file |-> a.file, before |-> a.before, undone |-> FALSE,
                  prev |-> a.prev, at |-> clock, left |-> fs[a.file]])]
       /\ bad' = IF fs[a.file] # a.wrote /\ fs[a.file].by \in Tasks
                 THEN bad \union {"a task noted another's write as its own"}
                 ELSE bad
    /\ act' = [act EXCEPT ![t] = Idle]
    /\ clock' = clock + 1
    /\ Give
    /\ UNCHANGED <<fs, snaps, undos, count, disk>>

\* Anyone glassd doesn't run changes the file.
Edit(f) ==
    /\ count.edits < MaxEdits
    /\ fs' = [fs EXCEPT ![f] = [by |-> Person, n |-> clock]]
    /\ clock' = clock + 1
    /\ count' = [count EXCEPT !.edits = @ + 1]
    /\ UNCHANGED <<snaps, chg, left, act, undos, lock, bad, disk>>

(***************************************************************************)
(* Undo of task t: scope 0 is everything not undone yet ("Undo all"); a   *)
(* number is that one change (the Changes view's Undo, or glassctl undo    *)
(* --change, which may name one already undone).                           *)
(***************************************************************************)
Min(S) == CHOOSE x \in S : \A y \in S : x <= y
Max(S) == IF S = {} THEN 0 ELSE CHOOSE x \in S : \A y \in S : x >= y

UndoPlan(t, scope) ==
    /\ count.undos < MaxUndos
    /\ Free
    /\ LET n == Len(chg[t])
           named == IF scope = 0
                    THEN {i \in 1..n : ~chg[t][i].undone}
                    ELSE {scope}
           ids == IF Fixed THEN {i \in named : ~chg[t][i].undone} ELSE named
           files == {chg[t][i].file : i \in ids}
           first(f) == Min({i \in ids : chg[t][i].file = f})
           b(f) == chg[t][first(f)].before
           kept(f) == \E j \in (1..n) \ ids :
                        /\ ~chg[t][j].undone
                        /\ chg[t][j].file = f
                        /\ chg[t][j].before >= b(f)
           \* Fixed: how the task's latest change on f still standing left
           \* it; a kept one's is the kept step's.
           latest(f) == Max({i \in 1..n : ~chg[t][i].undone /\ chg[t][i].file = f})
           ref(f) == IF ~Fixed THEN left[t][f]
                     ELSE IF latest(f) \in ids THEN chg[t][latest(f)].left
                     ELSE Untouched
       IN undos' = undos \union {[
            id |-> count.undos + 1, task |-> t, ids |-> ids,
            items |-> [f \in files |-> [b |-> b(f), kept |-> kept(f),
                                        prev |-> chg[t][first(f)].prev,
                                        ref |-> ref(f)]],
            dec |-> [f \in files |-> None],
            done |-> {}, alone |-> {},
            restored |-> [f \in files |-> NoValue]]}
    /\ count' = [count EXCEPT !.undos = @ + 1]
    /\ Take("undo")
    /\ UNCHANGED <<fs, snaps, chg, left, act, clock, bad, disk>>

\* `step`: what the file needs, from the snapshot's copy, the file now and
\* how the task left it when undo planned.
UndoCheck(u, f) ==
    /\ u \in undos
    /\ f \in DOMAIN u.items
    /\ u.dec[f] = None
    /\ LET it == u.items[f]
           then == snaps[u.task][it.b].copy[f]
           now == fs[f]
           w == it.ref
           dec == IF then = now THEN "nothing"
                  ELSE IF w = Untouched THEN "leave"     \* no record
                  ELSE IF now # w THEN "leave"           \* changed since
                  ELSE IF it.kept THEN "leave"           \* a kept step's
                  ELSE "restore"
       IN undos' = (undos \ {u}) \union {[u EXCEPT !.dec[f] = dec]}
    /\ UNCHANGED <<fs, snaps, chg, left, act, clock, count, lock, bad, disk>>

\* `restore` (or nothing), a step after the check.
UndoAct(u, f) ==
    /\ u \in undos
    /\ f \in DOMAIN u.items
    /\ u.dec[f] # None
    /\ f \notin u.done
    /\ LET it == u.items[f]
           then == snaps[u.task][it.b].copy[f]
           over == fs[f]
       IN IF u.dec[f] = "restore"
          THEN /\ fs' = [fs EXCEPT ![f] = then]
               /\ bad' = bad
                     \union (IF over.by \in Tasks \ {u.task}
                             THEN {"undo overwrote another task's change"}
                             ELSE {})
                     \union (IF over.by = Person
                             THEN {"undo overwrote the person's edit"}
                             ELSE {})
                     \union (IF then # it.prev /\ it.prev.by # Person
                             THEN {"undo put back other than the file before the change"}
                             ELSE {})
               /\ undos' = (undos \ {u}) \union
                    {[u EXCEPT !.done = @ \union {f}, !.restored[f] = then]}
          ELSE /\ undos' = (undos \ {u}) \union
                    {[u EXCEPT !.done = @ \union {f},
                               !.alone = IF u.dec[f] = "leave" THEN @ \union {f}
                                         ELSE @]}
               /\ UNCHANGED <<fs, bad, disk>>
    /\ UNCHANGED <<snaps, chg, left, act, clock, count, lock, disk>>

\* The end of `undo`: revert hooks, changes marked, what undo left noted.
UndoFinish(u, failed) ==
    /\ u \in undos
    /\ u.done = DOMAIN u.items
    /\ LET t == u.task
           blocked == {i \in u.ids : chg[t][i].file \in u.alone}
           reverted == u.ids \ blocked
       IN /\ failed \subseteq reverted
          /\ RevertMayFail \/ failed = {}
          /\ chg' = [chg EXCEPT ![t] = [i \in 1..Len(@) |->
                        IF i \in reverted \ failed
                        THEN [@[i] EXCEPT !.undone = TRUE] ELSE @[i]]]
          /\ left' = [left EXCEPT ![t] = [f \in Files |->
                        IF f \in DOMAIN u.restored /\ u.restored[f] # NoValue
                        THEN u.restored[f] ELSE @[f]]]
          /\ bad' = IF \E i \in reverted : chg[t][i].undone
                    THEN bad \union {"a revert hook ran for a change already undone"}
                    ELSE bad
    /\ undos' = undos \ {u}
    /\ Give
    \* `undo` writes the task's record before it answers.
    /\ disk' = [disk EXCEPT ![u.task] =
                    [snaps |-> snaps[u.task], chg |-> chg'[u.task], left |-> left'[u.task]]]
    /\ UNCHANGED <<fs, snaps, act, clock, count>>

(***************************************************************************)
(* The record on disk, and glassd starting again from it (ADR 0020).      *)
(***************************************************************************)
\* The task's record is written: within a second of a change, so at any
\* step after it, or not before glassd stops.
Keep(t) ==
    /\ disk[t] # [snaps |-> snaps[t], chg |-> chg[t], left |-> left[t]]
    /\ disk' = [disk EXCEPT ![t] = [snaps |-> snaps[t], chg |-> chg[t], left |-> left[t]]]
    /\ UNCHANGED <<fs, snaps, chg, left, act, undos, clock, count, lock, bad>>

\* glassd stops, at any step, and starts again: every task is ended, and is
\* what its record says. What was in flight is gone from memory (a step's
\* write that landed stays in the file, on no record; an undo's restores
\* so far stay too), and so is whoever held the person's turn.
Restart ==
    /\ count.restarts < MaxRestarts
    /\ snaps' = [t \in Tasks |-> disk[t].snaps]
    /\ chg' = [t \in Tasks |-> disk[t].chg]
    /\ left' = [t \in Tasks |-> disk[t].left]
    /\ act' = [t \in Tasks |-> Idle]
    /\ undos' = {}
    /\ lock' = None
    /\ count' = [count EXCEPT !.restarts = @ + 1,
                              !.a = [t \in Tasks |-> MaxActions]]
    /\ UNCHANGED <<fs, clock, bad, disk>>

\* Every action, edit and undo spent and nothing in flight: the model's end,
\* not a deadlock. A turn never given back would be one.
Finished ==
    /\ \A t \in Tasks : act[t] = Idle /\ count.a[t] = MaxActions
    /\ undos = {}
    /\ count.edits = MaxEdits
    /\ count.undos = MaxUndos
    /\ UNCHANGED vars

Next ==
    \/ Finished
    \/ \E t \in Tasks, f \in Files, notify \in BOOLEAN : StartAction(t, f, notify)
    \/ \E t \in Tasks : Write(t) \/ Record(t)
    \/ \E f \in Files : Edit(f)
    \/ \E t \in Tasks : \E scope \in 0..Len(chg[t]) : UndoPlan(t, scope)
    \/ \E u \in undos, f \in Files : UndoCheck(u, f) \/ UndoAct(u, f)
    \/ \E u \in undos : \E failed \in SUBSET u.ids : UndoFinish(u, failed)
    \/ \E t \in Tasks : Keep(t)
    \/ Restart

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* What the OS promises.                                                   *)
(***************************************************************************)
\* AU-21: undo never silently overwrites a change another task made: it
\* leaves the file and lists it instead.
AU21_NoOtherTasksChangeOverwritten ==
    "undo overwrote another task's change" \notin bad

\* AU-21, AW-15: what undo puts back is the file as it was before the
\* change, whoever wrote that; the person's own writes excepted, since
\* nothing can stop their editor between a snapshot and a step.
AU21_PutsBackTheFileBeforeTheChange ==
    "undo put back other than the file before the change" \notin bad

\* ADR 0017: a task's record of its files is its own writes.
ADR0017_TheRecordIsTheTasksOwn ==
    "a task noted another's write as its own" \notin bad

\* Undoing an undone change does nothing: no hook runs twice.
UndoingAnUndoneChangeDoesNothing ==
    "a revert hook ran for a change already undone" \notin bad

TypeOK ==
    /\ \A t \in Tasks : act[t].phase \in {"idle", "snapped", "wrote"}
    /\ lock = None \/ Fixed
=============================================================================
