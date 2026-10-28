--------------------------- MODULE ScheduleClaims ---------------------------
(***************************************************************************)
(* A person's schedule (ADR 0069) as agent hosts run it: every host looks  *)
(* for what's due, each through an index that may lag the table, and the   *)
(* first to claim a slot starts its run, while the person turns the        *)
(* schedule off and on, hosts are replaced and glassd dies at any step.    *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/svc/runtime/src/schedules.rs: run_due_schedules (Look: the    *)
(*    leaving check and the read of what's due), take_turn (Claim: the     *)
(*    write that holds only over the version read; Start), note_run,       *)
(*    remember_schedule_run (End), schedule_enable (Toggle).               *)
(*  - crates/bin/glassd/src/schedules.rs: DynamoSchedules (`due` reads     *)
(*    the sparse index, eventually consistent: Look reads any version the  *)
(*    table has had; `replace` is PutItem with `version = :seen`).         *)
(*  - crates/svc/runtime/src/computers.rs, runtime.rs: a host being        *)
(*    replaced starts nothing (Leave; Start on a leaving host fails).      *)
(*                                                                         *)
(* One schedule due every minute; a run of it takes a minute or more.      *)
(* OldBlindClaim is the claim as a plain write: TLC finds a slot started   *)
(* twice, and a run started for a schedule the person had turned off.      *)
(* OldLateFirst is the code as first written: TLC found two runs of one   *)
(* schedule at once (ScheduleClaims_long), and the fix is in take_turn.   *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS
  Hosts,
  MaxNow,        \* the last minute modeled
  Grace,         \* LATE_GRACE_SECS, in minutes
  RunningMax,    \* RUNNING_MAX_SECS, in minutes: a run elsewhere is taken as going this long
  MaxToggles,    \* times the person turns it off or on
  MaxCrashes,
  OldBlindClaim,
  OldLateFirst

Outcomes == {"none", "running", "done", "missed", "refused"}
Rec(v, n, on, last, at, out) ==
  [version |-> v, next |-> n, on |-> on, last |-> last, at |-> at, outcome |-> out]

VARIABLES
  now,      \* the minute it is
  rec,      \* the schedule's item in the table
  prev,     \* the item before the last write: what a lagging index may still show
  pc,       \* each host's step: "idle", "seen" (read what's due), "claimed"
  seen,     \* what each host read
  slotOf,   \* the slot each host claimed, and when
  claimedAt,
  leaving,  \* hosts being replaced
  live,     \* runs going: <<slot, host, claimed at>>
  starts,   \* runs started, by slot
  offStart, \* a run started on a claim made while the schedule was off
  overlap,  \* a run started while another of the schedule's was going
  toggles, crashes

vars == <<now, rec, prev, pc, seen, slotOf, claimedAt, leaving, live, starts, offStart, overlap,
          toggles, crashes>>

Max(a, b) == IF a > b THEN a ELSE b

Init ==
  /\ now = 0
  /\ rec = Rec(1, 1, TRUE, 0, 0, "none")
  /\ prev = rec
  /\ pc = [h \in Hosts |-> "idle"]
  /\ seen = [h \in Hosts |-> rec]
  /\ slotOf = [h \in Hosts |-> 0]
  /\ claimedAt = [h \in Hosts |-> 0]
  /\ leaving = [h \in Hosts |-> FALSE]
  /\ live = {}
  /\ starts = [t \in 0..MaxNow |-> 0]
  /\ offStart = FALSE
  /\ overlap = FALSE
  /\ toggles = 0
  /\ crashes = 0

Write(r) == /\ rec' = r
            /\ prev' = rec

(* Time passes. A run ends within RunningMax minutes of its claim: what   *)
(* lets another host take "running" as over after that (see ADR 0069).    *)
Tick ==
  /\ now < MaxNow
  /\ \A r \in live : now + 1 - r[3] < RunningMax
  \* take_turn starts what it claimed straight away.
  /\ \A h \in Hosts : pc[h] = "claimed" => now + 1 - claimedAt[h] < RunningMax
  /\ now' = now + 1
  /\ UNCHANGED <<rec, prev, pc, seen, slotOf, claimedAt, leaving, live, starts, offStart, overlap,
                 toggles, crashes>>

(* run_due_schedules: not while leaving; `due` from the index, which may   *)
(* show the item as it was before its last write.                         *)
Look(h) ==
  /\ pc[h] = "idle"
  /\ ~leaving[h]
  /\ \E r \in {rec, prev} :
       /\ r.on /\ r.next <= now
       /\ seen' = [seen EXCEPT ![h] = r]
  /\ pc' = [pc EXCEPT ![h] = "seen"]
  /\ UNCHANGED <<now, rec, prev, slotOf, claimedAt, leaving, live, starts, offStart, overlap,
                 toggles, crashes>>

(* take_turn: the claim. The last run still going (on this host: its     *)
(* task is live; elsewhere: claimed under RunningMax ago): let pass. Else *)
(* late by more than Grace: missed. OldLateFirst marked a late slot       *)
(* missed over a run going, and the next slot started a second beside it. *)
Claim(h) ==
  LET s == seen[h]
      late == now - s.next > Grace
      here == \E r \in live : r[2] = h /\ r[1] = s.last
      going == s.outcome = "running" /\ (here \/ now - s.at < RunningMax)
      missed == late /\ (OldLateFirst \/ ~going)
      start == ~late /\ ~going
      next == Max(now, s.next) + 1
      new == Rec(rec.version + 1, next, s.on,
                 IF start THEN s.next ELSE s.last,
                 IF start \/ missed THEN now ELSE s.at,
                 IF missed THEN "missed" ELSE IF going THEN s.outcome ELSE "running")
  IN
  /\ pc[h] = "seen"
  /\ IF OldBlindClaim \/ rec.version = s.version
       THEN /\ Write(new)
            /\ pc' = [pc EXCEPT ![h] = IF start THEN "claimed" ELSE "idle"]
            /\ slotOf' = [slotOf EXCEPT ![h] = s.next]
            /\ claimedAt' = [claimedAt EXCEPT ![h] = now]
            /\ offStart' = (offStart \/ (start /\ ~rec.on))
       ELSE /\ pc' = [pc EXCEPT ![h] = "idle"]
            /\ UNCHANGED <<rec, prev, slotOf, claimedAt, offStart>>
  /\ UNCHANGED <<now, seen, leaving, live, starts, overlap, toggles, crashes>>

(* start_scheduled. A host that began leaving since it looked refuses the *)
(* task (start_task); the slot is recorded refused, not run elsewhere.    *)
Start(h) ==
  /\ pc[h] = "claimed"
  /\ pc' = [pc EXCEPT ![h] = "idle"]
  /\ IF leaving[h]
       THEN /\ IF rec.last = slotOf[h] /\ rec.outcome = "running"
                 THEN Write([rec EXCEPT !.version = @ + 1, !.outcome = "refused"])
                 ELSE UNCHANGED <<rec, prev>>
            /\ UNCHANGED <<live, starts, overlap>>
       ELSE /\ starts' = [starts EXCEPT ![slotOf[h]] = @ + 1]
            /\ overlap' = (overlap \/ live # {})
            /\ live' = live \cup {<<slotOf[h], h, claimedAt[h]>>}
            /\ UNCHANGED <<rec, prev>>
  /\ UNCHANGED <<now, seen, slotOf, claimedAt, leaving, offStart, toggles, crashes>>

(* remember_schedule_run: kept if it's still the schedule's last run.     *)
End(r) ==
  /\ r \in live
  /\ live' = live \ {r}
  /\ IF rec.last = r[1] /\ rec.outcome = "running"
       THEN Write([rec EXCEPT !.version = @ + 1, !.outcome = "done"])
       ELSE UNCHANGED <<rec, prev>>
  /\ UNCHANGED <<now, pc, seen, slotOf, claimedAt, leaving, starts, offStart, overlap,
                 toggles, crashes>>

(* schedule_enable: on again counts its next slot from now.               *)
Toggle ==
  /\ toggles < MaxToggles
  /\ toggles' = toggles + 1
  /\ Write([rec EXCEPT !.version = @ + 1, !.on = ~@,
                       !.next = IF rec.on THEN @ ELSE now + 1])
  /\ UNCHANGED <<now, pc, seen, slotOf, claimedAt, leaving, live, starts, offStart, overlap,
                 crashes>>

(* The host is told it's being replaced.                                  *)
Leave(h) ==
  /\ ~leaving[h]
  /\ leaving' = [leaving EXCEPT ![h] = TRUE]
  /\ UNCHANGED <<now, rec, prev, pc, seen, slotOf, claimedAt, live, starts, offStart, overlap,
                 toggles, crashes>>

(* glassd dies: what it was doing stops, its runs with it; its claim      *)
(* stays in the table.                                                    *)
Crash(h) ==
  /\ crashes < MaxCrashes
  /\ crashes' = crashes + 1
  /\ pc' = [pc EXCEPT ![h] = "idle"]
  /\ live' = {r \in live : r[2] # h}
  /\ UNCHANGED <<now, rec, prev, seen, slotOf, claimedAt, leaving, starts, offStart, overlap,
                 toggles>>

Next ==
  \/ Tick
  \/ Toggle
  \/ \E h \in Hosts : Look(h) \/ Claim(h) \/ Start(h) \/ Leave(h) \/ Crash(h)
  \/ \E r \in live : End(r)

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ now \in 0..MaxNow
  /\ rec.outcome \in Outcomes
  /\ pc \in [Hosts -> {"idle", "seen", "claimed"}]
  /\ starts \in [0..MaxNow -> Nat]

\* A slot starts one run, however many hosts look and however late the
\* index is.
ADR0069_EachSlotOnce == \A t \in 0..MaxNow : starts[t] <= 1

\* Never a run for a schedule that was off when it was claimed.
ADR0069_NeverWhileOff == ~offStart

\* A schedule runs one run at a time.
ADR0069_OneRunAtATime == ~overlap

\* No host being replaced starts one.
ADR0069_NotOnALeavingHost == [][\A r \in live' \ live : ~leaving[r[2]]]_vars

=============================================================================
