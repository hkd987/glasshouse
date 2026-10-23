------------------------------ MODULE Routines ------------------------------
(***************************************************************************)
(* Routines (DA-4, NV-21, ADR 0044, ADR 0047, ADR 0051) as glassd runs     *)
(* them: the scheduler, starting a run, a run reading its routine, what a  *)
(* person changes meanwhile, and glassd restarting.                        *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/bin/glassd/src/main.rs: the scheduler loop, every 15 s, once  *)
(*    per local minute (Tick).                                             *)
(*  - crates/svc/runtime/src/routines.rs: run_due_routines (Tick),         *)
(*    start_routine (SchedStart, RunNow), remember_run (RunEnds),          *)
(*    set_routines -> Routines::recover (Restart), RoutinesHost::run_of.   *)
(*  - crates/apps/routines/src/store.rs: the person's choices file under   *)
(*    one lock (store::change, store::pause, store::locked), versions      *)
(*    (save), runs.json and .running.json (record_run, run_started,        *)
(*    recover), the schedule's minute (claim_minute).                      *)
(*  - crates/apps/routines/src/lib.rs: routines.save/schedule (Routines::  *)
(*    keep: Save), routines.use (Use), routines.delete (Delete),           *)
(*    routines.pause (Pause), routines.read (AgentRead).                   *)
(*                                                                         *)
(* Before ADR 0051 TLC found, against the code as it was: two runs of one  *)
(* routine at once (Run now twice, or its schedule while a run went); a    *)
(* minute's routines started twice when glassd restarted in it or the      *)
(* clock came back to it; a routine paused after the scheduler read it     *)
(* started anyway; a run recording version 1 ran version 2, saved before   *)
(* its agent read the routine; a pause writing back a stale copy of the    *)
(* choices file, taking a routine back to its old version; and a run a    *)
(* restart cut off never kept.                                             *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
  Rs,          \* the person's routines
  MaxVersion,  \* versions a routine may have
  MaxClock,    \* the last local minute modeled
  MaxRuns,     \* runs started in one behavior
  Kept,        \* runs.json keeps this many (KEPT_RUNS)
  DueAt        \* the local minute every routine's schedule is due in

None == 0
Failed == 99   \* what a run reads of a routine removed since it started

Due(r) == {DueAt}

VARIABLES
  clock,     \* the local minute now: local_minute's number
  wentBack,  \* the clock went back once (set back, or daylight saving ending)
  restarted, \* glassd restarted once
  claimed,   \* the last minute the schedule claimed, on disk (.scheduled)
  versions,  \* versions saved, by routine
  chosen,    \* the person's choices file: the version in use, None if removed
  paused,    \* ... and whether its schedule is paused
  pending,   \* routines the scheduler found due this minute, not yet started
  live,      \* runs going: [id, r, v (recorded), read (what its agent read), sched]
  nextRun,
  kept,      \* runs.json, newest first, by routine
  \* what the invariants look back on
  started, occ, twice, pausedStart, lastWrite, lastPause

vars == <<clock, wentBack, restarted, claimed, versions, chosen, paused, pending,
          live, nextRun, kept, started, occ, twice, pausedStart, lastWrite, lastPause>>
choices == <<chosen, paused, lastWrite, lastPause>>
runVars == <<live, nextRun, started, occ, twice, pausedStart>>

Live(r) == {x \in live : x.r = r}

Init ==
  /\ clock = 0 /\ wentBack = FALSE /\ restarted = FALSE /\ claimed = -1
  /\ versions = [r \in Rs |-> 1] /\ chosen = [r \in Rs |-> 1]
  /\ paused = [r \in Rs |-> FALSE]
  /\ pending = {}
  /\ live = {} /\ nextRun = 1 /\ kept = [r \in Rs |-> <<>>]
  /\ started = {} /\ occ = {} /\ twice = FALSE /\ pausedStart = FALSE
  /\ lastWrite = [r \in Rs |-> 1] /\ lastPause = [r \in Rs |-> FALSE]

(* The computer's clock: forward a minute, and once back one.             *)
Advance ==
  /\ clock < MaxClock /\ clock' = clock + 1
  /\ UNCHANGED <<wentBack, restarted, claimed, versions, choices, pending, runVars, kept>>
StepBack ==
  /\ ~wentBack /\ clock > 0 /\ clock' = clock - 1 /\ wentBack' = TRUE
  /\ UNCHANGED <<restarted, claimed, versions, choices, pending, runVars, kept>>

(* A run starts: its task, recorded with the version in use, noted as      *)
(* going in .running.json.                                                 *)
StartRun(r, sched, at) ==
  LET run == [id |-> nextRun, r |-> r, v |-> chosen[r], read |-> None, sched |-> sched]
  IN /\ live' = live \cup {run}
     /\ nextRun' = nextRun + 1
     /\ started' = started \cup {nextRun}
     /\ occ' = IF sched THEN occ \cup {<<r, at>>} ELSE occ
     /\ twice' = (twice \/ (sched /\ <<r, at>> \in occ))
     /\ pausedStart' = (pausedStart \/ (sched /\ paused[r]))

(* run_due_routines: claims the minute on disk, then finds the routines    *)
(* due in it, in use and not paused, as read then.                         *)
Tick ==
  /\ pending = {}
  /\ clock > claimed
  /\ claimed' = clock
  /\ pending' = {r \in Rs : chosen[r] # None /\ ~paused[r] /\ clock \in Due(r)}
  /\ UNCHANGED <<clock, wentBack, restarted, versions, choices, runVars, kept>>

(* start_routine for the schedule, under the choices lock: in use, not     *)
(* paused and not running now, or it doesn't start.                        *)
SchedStart(r) ==
  /\ r \in pending
  /\ pending' = pending \ {r}
  /\ IF chosen[r] # None /\ ~paused[r] /\ Live(r) = {} /\ nextRun <= MaxRuns
       THEN StartRun(r, TRUE, claimed)
       ELSE UNCHANGED runVars
  /\ UNCHANGED <<clock, wentBack, restarted, claimed, versions, choices, kept>>

(* routines.run, Run now, glassctl routine run: start_routine, under the   *)
(* choices lock, refused while it runs.                                    *)
RunNow(r) ==
  /\ chosen[r] # None /\ Live(r) = {} /\ nextRun <= MaxRuns
  /\ StartRun(r, FALSE, -1)
  /\ UNCHANGED <<clock, wentBack, restarted, claimed, versions, choices, pending, kept>>

(* The run's agent reads its routine (routines.read with no version): the  *)
(* version its run started with (run_of), if the person still has it.     *)
AgentRead(x) ==
  /\ x \in live /\ x.read = None
  /\ live' = (live \ {x}) \cup {[x EXCEPT !.read = IF chosen[x.r] = None THEN Failed ELSE x.v]}
  /\ UNCHANGED <<clock, wentBack, restarted, claimed, versions, choices, pending,
                 nextRun, kept, started, occ, twice, pausedStart>>

(* record_run: first in runs.json, KEPT_RUNS at most, and no longer going. *)
Keep(r, id) ==
  IF Len(<<id>> \o kept[r]) > Kept THEN SubSeq(<<id>> \o kept[r], 1, Kept)
  ELSE <<id>> \o kept[r]
RunEnds(x) ==
  /\ x \in live
  /\ live' = live \ {x}
  /\ kept' = [kept EXCEPT ![x.r] = Keep(x.r, x.id)]
  /\ UNCHANGED <<clock, wentBack, restarted, claimed, versions, choices, pending,
                 nextRun, started, occ, twice, pausedStart>>

(* Calls changing the person's choices file: each reads and writes it      *)
(* under the choices lock, so one step.                                    *)
SetChosen(r, v) ==
  /\ chosen' = [chosen EXCEPT ![r] = v]
  \* The routines they no longer have aren't paused either.
  /\ paused' = [paused EXCEPT ![r] = paused[r] /\ v # None]
  /\ lastWrite' = [lastWrite EXCEPT ![r] = v]
  /\ UNCHANGED lastPause

(* routines.save or routines.schedule of a next version: Routines::keep    *)
(* numbers it and chooses it, refused if the routine was removed.         *)
Save(r) ==
  /\ chosen[r] # None /\ versions[r] < MaxVersion
  /\ versions' = [versions EXCEPT ![r] = versions[r] + 1]
  /\ SetChosen(r, versions[r] + 1)
  /\ UNCHANGED <<clock, wentBack, restarted, claimed, pending, runVars, kept>>
Use(r, v) ==
  /\ chosen[r] # None /\ v \in 1..versions[r] /\ v # chosen[r]
  /\ SetChosen(r, v)
  /\ UNCHANGED <<clock, wentBack, restarted, claimed, versions, pending, runVars, kept>>
Delete(r) ==
  /\ chosen[r] # None
  /\ SetChosen(r, None)
  /\ UNCHANGED <<clock, wentBack, restarted, claimed, versions, pending, runVars, kept>>
Pause(r, on) ==
  /\ chosen[r] # None /\ paused[r] # on
  /\ paused' = [paused EXCEPT ![r] = on]
  /\ lastPause' = [lastPause EXCEPT ![r] = on]
  /\ UNCHANGED <<chosen, lastWrite, clock, wentBack, restarted, claimed, versions, pending, runVars, kept>>

(* glassd restarts: its runs end with their tasks, and the next glassd    *)
(* keeps each as cut off (recover); the claimed minute is on disk.         *)
Restart ==
  /\ ~restarted /\ restarted' = TRUE
  /\ live' = {} /\ pending' = {}
  /\ kept' = [r \in Rs |-> IF Live(r) = {} THEN kept[r]
                           ELSE Keep(r, (CHOOSE x \in Live(r) : TRUE).id)]
  /\ UNCHANGED <<clock, wentBack, claimed, versions, choices, nextRun, started, occ, twice, pausedStart>>

Next ==
  \/ Advance \/ StepBack \/ Tick \/ Restart
  \/ \E r \in Rs : \/ SchedStart(r) \/ RunNow(r) \/ Save(r) \/ Delete(r)
                   \/ \E v \in 1..MaxVersion : Use(r, v)
                   \/ \E b \in BOOLEAN : Pause(r, b)
  \/ \E x \in live : AgentRead(x) \/ RunEnds(x)

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* What the OS promises.                                                   *)
(***************************************************************************)
\* "routines.read says which routine is running now, and its run": one.
DA4_OneRunAtATime == \A r \in Rs : Cardinality(Live(r)) <= 1
\* A minute on the schedule starts a routine once, whatever glassd or the
\* clock do; a minute slept through doesn't run late.
DA4_EachMinuteOnce == ~twice
\* ADR 0047: "A paused routine runs only when started."
ADR0047_PausedSkipsItsSchedule == ~pausedStart
\* A run runs the version its record says.
ADR0047_RunsTheVersionItRecords == \A x \in live : x.read \in {None, Failed, x.v}
\* No call's change to the person's choices is lost to another's.
DA4_NoChoiceLost ==
  \A r \in Rs : chosen[r] = lastWrite[r] /\ (chosen[r] # None => paused[r] = lastPause[r])
\* ADR 0047: the last 20 runs are kept...
ADR0047_RunsKeptAtMost == \A r \in Rs : Len(kept[r]) <= Kept
\* ... every one of them, a run a restart cut off too (with Kept >= MaxRuns).
KeptIds == UNION {{kept[r][i] : i \in 1..Len(kept[r])} : r \in Rs}
ADR0047_EveryRunKept == \A id \in started : id \in {x.id : x \in live} \/ id \in KeptIds

TypeOK ==
  /\ clock \in 0..MaxClock /\ claimed \in -1..MaxClock /\ nextRun \in 1..MaxRuns + 1
  /\ \A r \in Rs : chosen[r] \in 0..versions[r] /\ versions[r] \in 1..MaxVersion
=============================================================================
