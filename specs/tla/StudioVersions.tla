--------------------------- MODULE StudioVersions ---------------------------
(***************************************************************************)
(* The versions of apps a person makes, and the choice of which they use   *)
(* (GA-3, ADR 0032), as two of their tasks save, remix, go back and undo   *)
(* at once.                                                                *)
(*                                                                         *)
(* The code (crates/apps/studio/src):                                      *)
(*  - lib.rs `plan_save`: a remix's app id (an app of theirs it undid      *)
(*    goes on where it was; else the next free id, my-mail, my-mail-2) and *)
(*    a new version's number, read from the store and mine.json. PlanSave. *)
(*  - lib.rs `save`: under `saving` (one save at a time on the computer):  *)
(*    the version numbered again (latest + 1), the store written whole     *)
(*    (store.rs `Store::save`: a staging folder renamed into place), then  *)
(*    mine.json read, changed and written (`chosen`, `choose`). Save.      *)
(*  - lib.rs `revert`: mine.json read, changed and written, without        *)
(*    `saving`. RevertRead, RevertWrite.                                   *)
(*  - Undo of any of them puts mine.json back from the snapshot taken      *)
(*    before, if it is still as the step left it (Undo.tla has the         *)
(*    details; here one step). Undo.                                       *)
(*                                                                         *)
(*  - Old versions go (audit 5.3; store.rs `Store::moved`, and the rule in  *)
(*    crates/lib/state/src/kept.rs): each save and each going back notes   *)
(*    the version it leaves and the one it moves to as in use now, then    *)
(*    removes the app's versions that are not among the newest, not the    *)
(*    one in use, and last noted more than Keep ago. A version with no     *)
(*    note counts from when it was saved. Undo reaches back Reach (30      *)
(*    days, for a task that has ended); Keep is 90.                        *)
(*    Routines' versions go by the same rule (crates/apps/routines/src/    *)
(*    store.rs `Routines::moved`); Routines.tla is about runs, in minutes. *)
(*                                                                         *)
(* Turns = FALSE is the code before ADR 0051; TRUE is the code now: these  *)
(* are reversible steps, so a person's take turns from snapshot to record  *)
(* (`Runtime::writing_turn`), and each one below runs whole.               *)
(*                                                                         *)
(* Old holds a way the removal could be wrong:                             *)
(*   "fromSaved"  a version counted from when it was saved, with no note   *)
(*                of when it was last in use: the one a save leaves, in    *)
(*                use until that moment, goes, and undoing the save names  *)
(*                it                                                       *)
(* And Keep must be at least Reach: with Keep = 0 and Reach = 1 TLC finds  *)
(* the undo that names a version removed since its step.                   *)
(*                                                                         *)
(* Not modelled: a task still open can be undone further back than Reach.  *)
(* One open for more than three months, whose change of version is undone  *)
(* after twenty newer versions were saved, names a version that is gone;   *)
(* the app then isn't offered until the person picks a version.            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    Tasks,       \* the person's tasks
    FirstId,     \* the ids a remix can take: my-mail,
    SecondId,    \* then my-mail-2
    MaxSteps,    \* steps per task
    MaxUndos,
    Turns,       \* the code with ADR 0051's turns
    Newest,      \* versions of an app kept whatever their age (VERSIONS_KEPT)
    Keep,        \* how long after its last use a version stays (AFTER_USE)
    Reach,       \* how far back an undo goes
    MaxClock,    \* the last moment modelled
    Old

None == 0
NoApp == "no app"
IdOrder == <<FirstId, SecondId>>
Ids == {IdOrder[k] : k \in 1..Len(IdOrder)}
Order(i) == CHOOSE k \in 1..Len(IdOrder) : IdOrder[k] = i

\* The most versions an app can come to have here.
MaxV == Cardinality(Tasks) * MaxSteps

VARIABLES
    store,      \* each id's versions: a set of numbers
    mine,       \* mine.json: each id's chosen version, 0 for none
    op,         \* each task's step in flight
    done,       \* steps finished, for undo, each with when
    turn,       \* Turns: who holds the person's turn, or NoApp
    count,
    intended,   \* ghost: mine.json as the finished steps meant it
    bad,
    clock,
    used        \* each id's versions: when last noted in use, or saved

vars == <<store, mine, op, done, turn, count, intended, bad, clock, used>>

Idle == [kind |-> "idle"]

Init ==
    /\ store = [i \in Ids |-> {}]
    /\ mine = [i \in Ids |-> None]
    /\ op = [t \in Tasks |-> Idle]
    /\ done = {}
    /\ turn = NoApp
    /\ count = [steps |-> [t \in Tasks |-> 0], undos |-> 0, n |-> 0]
    /\ intended = [i \in Ids |-> None]
    /\ bad = {}
    /\ clock = 0
    /\ used = [i \in Ids |-> [v \in 1..MaxV |-> 0]]

(* Time passes.                                                            *)
Tick ==
    /\ clock < MaxClock
    /\ clock' = clock + 1
    /\ UNCHANGED <<store, mine, op, done, turn, count, intended, bad, used>>

\* `Store::moved`, its notes: the version left and the one moved to are in
\* use now.
Noted(us, from, to) ==
    IF "fromSaved" \in Old THEN us
    ELSE [v \in 1..MaxV |-> IF v = to \/ v = from THEN clock ELSE us[v]]

\* ... and what it removes of the versions S, with `to` in use and the
\* notes us: not among the newest, not in use, long unused.
Gone(S, to, us) ==
    {v \in S : /\ Cardinality({w \in S : w > v}) >= Newest
               /\ v # to
               /\ clock - us[v] > Keep}

Latest(i) == IF store[i] = {} THEN 0
             ELSE CHOOSE v \in store[i] : \A w \in store[i] : v >= w

\* A turn for t, held from the plan to the end of the step.
CanStart(t) == ~Turns \/ turn = NoApp
TakeTurn(t) == IF Turns THEN turn' = t ELSE UNCHANGED turn
GiveTurn == IF Turns THEN turn' = NoApp ELSE UNCHANGED turn

\* `plan_save` for a remix of Mail: an id of theirs no longer chosen (an
\* undone remix) goes on; else the first id no app has.
RemixId ==
    LET undone == {i \in Ids : store[i] # {} /\ mine[i] = None}
        free == {i \in Ids : store[i] = {}}
        first(S) == CHOOSE i \in S : \A j \in S : Order(i) <= Order(j)
    IN IF undone # {} THEN first(undone)
       ELSE IF free # {} THEN first(free) ELSE NoApp

PlanRemix(t) ==
    /\ op[t] = Idle
    /\ count.steps[t] < MaxSteps
    /\ CanStart(t)
    /\ LET i == RemixId IN
       /\ i # NoApp
       /\ op' = [op EXCEPT ![t] = [kind |-> "save", app |-> i, floor |-> 1,
                    remix |-> TRUE, chosenAtPlan |-> mine[i] # None,
                    before |-> mine]]
    /\ count' = [count EXCEPT !.steps[t] = @ + 1]
    /\ TakeTurn(t)
    /\ UNCHANGED <<store, mine, done, intended, bad, clock, used>>

\* A new version of an app of theirs.
PlanNewVersion(t, i) ==
    /\ op[t] = Idle
    /\ count.steps[t] < MaxSteps
    /\ CanStart(t)
    /\ store[i] # {}
    /\ op' = [op EXCEPT ![t] = [kind |-> "save", app |-> i,
                 floor |-> Latest(i) + 1, remix |-> FALSE,
                 chosenAtPlan |-> TRUE, before |-> mine]]
    /\ count' = [count EXCEPT !.steps[t] = @ + 1]
    /\ TakeTurn(t)
    /\ UNCHANGED <<store, mine, done, intended, bad, clock, used>>

\* `save`, whole under `saving`: numbered again, the store written,
\* mine.json read and written, the versions past keeping removed.
Save(t) ==
    /\ op[t].kind = "save"
    /\ LET i == op[t].app
           v == IF op[t].floor > Latest(i) + 1 THEN op[t].floor ELSE Latest(i) + 1
           m2 == [mine EXCEPT ![i] = v]
           \* A version saved counts from now, noted or not.
           us == Noted([used[i] EXCEPT ![v] = clock], mine[i], v)
           all == store[i] \union {v}
       IN /\ store' = [store EXCEPT ![i] = all \ Gone(all, v, us)]
          /\ used' = [used EXCEPT ![i] = us]
          /\ mine' = m2
          /\ intended' = [intended EXCEPT ![i] = v]
          /\ bad' = bad
                \union (IF v \in store[i] THEN {"a version written twice"} ELSE {})
                \union (IF op[t].remix /\ ~op[t].chosenAtPlan /\ mine[i] # None
                        THEN {"a remix saved as a version of another remix"}
                        ELSE {})
          /\ done' = done \union {[id |-> count.n, task |-> t, kind |-> "save",
                         app |-> i, before |-> op[t].before, prev |-> mine,
                         left |-> m2, at |-> clock]}
    /\ op' = [op EXCEPT ![t] = Idle]
    /\ count' = [count EXCEPT !.n = @ + 1]
    /\ GiveTurn
    /\ UNCHANGED clock

\* `revert`: back to an existing version, mine.json read then written.
RevertRead(t, i, v) ==
    /\ op[t] = Idle
    /\ count.steps[t] < MaxSteps
    /\ CanStart(t)
    /\ v \in store[i]
    /\ op' = [op EXCEPT ![t] = [kind |-> "revert", app |-> i, version |-> v,
                 read |-> mine, before |-> mine]]
    /\ count' = [count EXCEPT !.steps[t] = @ + 1]
    /\ TakeTurn(t)
    /\ UNCHANGED <<store, mine, done, intended, bad, clock, used>>

RevertWrite(t) ==
    /\ op[t].kind = "revert"
    /\ LET i == op[t].app
           m2 == [op[t].read EXCEPT ![i] = op[t].version]
           us == Noted(used[i], op[t].read[i], op[t].version)
       IN
       /\ mine' = m2
       /\ store' = [store EXCEPT ![i] = @ \ Gone(@, op[t].version, us)]
       /\ used' = [used EXCEPT ![i] = us]
       /\ intended' = [intended EXCEPT ![i] = op[t].version]
       /\ done' = done \union {[id |-> count.n, task |-> t, kind |-> "revert",
                      app |-> i, before |-> op[t].before, prev |-> mine,
                      left |-> m2, at |-> clock]}
       /\ bad' = IF mine # op[t].read
                 THEN bad \union {"a step's choice written over"}
                 ELSE bad
    /\ op' = [op EXCEPT ![t] = Idle]
    /\ count' = [count EXCEPT !.n = @ + 1]
    /\ GiveTurn
    /\ UNCHANGED clock

\* Undo of a finished step, while it can still be undone: mine.json back
\* from before it, if it is as the step left it; else left as it is and
\* listed.
Undo(d) ==
    /\ count.undos < MaxUndos
    /\ ~Turns \/ turn = NoApp
    /\ clock - d.at <= Reach
    /\ IF mine = d.left
       THEN /\ mine' = d.before
            /\ intended' = d.before
            /\ bad' = IF d.before # d.prev
                      THEN bad \union {"an undo took back another step's choice"}
                      ELSE bad
       ELSE UNCHANGED <<mine, intended, bad>>
    /\ done' = done \ {d}
    /\ count' = [count EXCEPT !.undos = @ + 1]
    /\ UNCHANGED <<store, op, turn, clock, used>>

Finished ==
    /\ \A t \in Tasks : op[t] = Idle
    /\ UNCHANGED vars

Next ==
    \/ Finished
    \/ Tick
    \/ \E t \in Tasks : PlanRemix(t) \/ Save(t) \/ RevertWrite(t)
    \/ \E t \in Tasks, i \in Ids : PlanNewVersion(t, i)
    \/ \E t \in Tasks, i \in Ids, v \in 1..2 : RevertRead(t, i, v)
    \/ \E d \in done : Undo(d)

Spec == Init /\ [][Next]_vars

\* GA-3: a version, once saved, is never written again.
GA3_VersionsAreNeverOverwritten == "a version written twice" \notin bad

\* GA-3: what mine.json chooses is a version that exists, after any save,
\* going back or undo, and whatever was removed meanwhile.
GA3_TheChoiceIsAVersionThatExists ==
    \A i \in Ids : mine[i] = None \/ mine[i] \in store[i]

\* Not a promise: a check on the model, which TLC must find false (run it
\* by hand as an invariant). Versions are removed in it.
NothingIsEverRemoved == \A i \in Ids : store[i] = 1..Latest(i)

\* GA-3: every step finished is in mine.json as it meant, until a later step
\* or an undo changes it: none is lost to another written over it.
GA3_NoChoiceIsLost ==
    /\ (\A t \in Tasks : op[t] = Idle) => mine = intended
    /\ "a step's choice written over" \notin bad
    /\ "an undo took back another step's choice" \notin bad

\* ADR 0032: two remixes of an app are two apps (My Mail, My Mail 2), not
\* one's versions.
ADR0032_TwoRemixesAreTwoApps ==
    "a remix saved as a version of another remix" \notin bad
=============================================================================
