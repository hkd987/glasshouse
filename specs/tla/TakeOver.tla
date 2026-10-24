------------------------------ MODULE TakeOver ------------------------------
(***************************************************************************)
(* Take over and hand back (AW-20, AW-21, AW-22, ADR 0015, ADR 0017) as   *)
(* the runtime runs them, against the agent's own steps, the person's own *)
(* edits, pause, resume, stop and the agent exiting, with two take overs  *)
(* and two hand backs that can land at once (a double click, the tile and *)
(* `glassctl` together).                                                   *)
(*                                                                         *)
(* The code (crates/svc/runtime/src):                                      *)
(*  - runtime.rs `take_over`: `hold` (under the task's lock: the tile says *)
(*    You're driving, the gate holds), then a snapshot of the task's       *)
(*    writable folders (`take_snapshot`, awaited), then `takeover` set.    *)
(*    TakeOverHold, TakeOverTurn, TakeOverSnap, TakeOverSet.               *)
(*  - runtime.rs `hand_back`: checks the tile, takes `takeover`, then      *)
(*    undo.rs `record_edits` (clones `written`; `changes_since` the        *)
(*    snapshot, keeping what `written` doesn't explain as the person's     *)
(*    edits, "by you"), then `release`. HandBackTurn, HandBackClaim,       *)
(*    HandBackClone, HandBackRecord, HandBackRelease.                      *)
(*  - runtime.rs `pause`, `resume`, `end_task` (stop; the agent exiting).  *)
(*  - execute.rs `call_tool`: a step counts itself in flight while the     *)
(*    gate is Run, takes the person's writing turn (ADR 0051), writes,     *)
(*    then records how it left the file in `written`. AgentStart,          *)
(*    AgentTurn, AgentWrite, AgentRecord.                                  *)
(*                                                                         *)
(* Abstractions: one file in the task's writable folder; each write makes  *)
(* a version nobody else writes, so "changed since the snapshot" and       *)
(* "explained by `written`" compare versions; the person's editor writes   *)
(* at any time; asks are left to the Approvals model, sleep too (it skips  *)
(* a task the person drives).                                              *)
(*                                                                         *)
(* Fixed = FALSE is the code before ADR 0052; TRUE is the code now:        *)
(*  1. Pause refuses a task the person drives: it ended the drive without *)
(*     a hand back, and a resume then ran the agent with the person's      *)
(*     edits never recorded.                                               *)
(*  2. Take over's snapshot and hand back hold the person's writing turn   *)
(*     (ADR 0051), and a take over snapshots only for a drive still under  *)
(*     way that has none: a hand back waits for the snapshot, for a step   *)
(*     of the agent between its write and its record, and for another     *)
(*     hand back; a second take over takes no new snapshot.                *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    TakeOvers,      \* take over requests
    HandBacks,      \* hand back requests
    AgentSteps,     \* the agent's steps that write the file
    PersonEdits,    \* the person's own edits
    MaxPauses,      \* pause requests
    MaxResumes,     \* resume requests
    Fixed           \* the code with ADR 0052's fixes

None == 99
MaxWrites == AgentSteps + PersonEdits
Versions == 0..MaxWrites
Live == {"Working", "Driving", "Paused"}

VARIABLES
    tile,       \* the task's tile: Working, Driving (You're driving), Paused, Ended
    gate,       \* Run, Hold, Ended
    takeover,   \* `TaskState::takeover`: the snapshot's version of the file, or None
    fver,       \* the file's version now (0: as it was)
    author,     \* who wrote each version
    written,    \* `TaskState::written`: the version the agent's last step left
    byYou,      \* versions recorded as the person's edits ("by you")
    turn,       \* who has the person's writing turn: "free", "agent", or a request
    apc, steps, amine,          \* the agent's step
    pedits,                     \* the person's edits so far
    tpc, tsnap,                 \* take over requests
    hpc, hh, hwc, hsaw,         \* hand back requests
    pauses, resumes, ended,
    holdId,     \* ghost: holds so far (pause and take over)
    driveFrom,  \* ghost: the snapshot's version for the drive under way, or None
    lost,       \* ghost: a hand back let go of a hold made after its check
    missed      \* ghost: a hand back left out an edit the person made driving

vars == <<tile, gate, takeover, fver, author, written, byYou, turn, apc, steps,
          amine, pedits, tpc, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended,
          holdId, driveFrom, lost, missed>>

TypeOK ==
    /\ tile \in Live \cup {"Ended"}
    /\ gate \in {"Run", "Hold", "Ended"}
    /\ takeover \in Versions \cup {None}
    /\ fver \in Versions
    /\ author \in [1..MaxWrites -> {"agent", "person", "-"}]
    /\ written \in Versions
    /\ byYou \subseteq 1..MaxWrites
    /\ turn \in {"free", "agent"} \cup TakeOvers \cup HandBacks
    /\ apc \in {"idle", "counted", "turned", "wrote"}
    /\ tpc \in [TakeOvers -> {"idle", "held", "turned", "snapped", "done"}]
    /\ hpc \in [HandBacks -> {"idle", "turned", "claimed", "cloned", "recorded", "done"}]

Init ==
    /\ tile = "Working" /\ gate = "Run"
    /\ takeover = None
    /\ fver = 0 /\ author = [v \in 1..MaxWrites |-> "-"]
    /\ written = 0 /\ byYou = {}
    /\ turn = "free"
    /\ apc = "idle" /\ steps = 0 /\ amine = 0
    /\ pedits = 0
    /\ tpc = [t \in TakeOvers |-> "idle"] /\ tsnap = [t \in TakeOvers |-> 0]
    /\ hpc = [h \in HandBacks |-> "idle"]
    /\ hh = [h \in HandBacks |-> None] /\ hwc = [h \in HandBacks |-> 0]
    /\ hsaw = [h \in HandBacks |-> 0]
    /\ pauses = 0 /\ resumes = 0 /\ ended = FALSE
    /\ holdId = 0 /\ driveFrom = None /\ lost = FALSE /\ missed = FALSE

\* `hold`: a live task's tile and gate, under its lock.
Hold(newTile) ==
    /\ tile' = newTile /\ gate' = "Hold"
    /\ holdId' = IF tile = newTile THEN holdId ELSE holdId + 1

\* `release`: a live task carries on (no ask open; asks are the Approvals model's).
Release ==
    /\ tile' = "Working" /\ gate' = "Run" /\ driveFrom' = None

----------------------------------------------------------------------------
\* The agent's step (execute.rs).

AgentStart ==
    /\ apc = "idle" /\ steps < AgentSteps /\ gate = "Run"
    /\ apc' = "counted" /\ steps' = steps + 1
    /\ UNCHANGED <<tile, gate, takeover, fver, author, written, byYou, turn, amine,
                   pedits, tpc, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended,
                   holdId, driveFrom, lost, missed>>

AgentTurn ==
    /\ apc = "counted" /\ turn = "free"
    /\ turn' = "agent" /\ apc' = "turned"
    /\ UNCHANGED <<tile, gate, takeover, fver, author, written, byYou, steps, amine,
                   pedits, tpc, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended,
                   holdId, driveFrom, lost, missed>>

AgentWrite ==
    /\ apc = "turned"
    /\ fver' = fver + 1 /\ author' = [author EXCEPT ![fver + 1] = "agent"]
    /\ amine' = fver + 1 /\ apc' = "wrote"
    /\ UNCHANGED <<tile, gate, takeover, written, byYou, turn, steps, pedits, tpc,
                   tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended, holdId,
                   driveFrom, lost, missed>>

AgentRecord ==
    /\ apc = "wrote"
    /\ written' = amine /\ turn' = "free" /\ apc' = "idle"
    /\ UNCHANGED <<tile, gate, takeover, fver, author, byYou, steps, amine, pedits,
                   tpc, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended, holdId,
                   driveFrom, lost, missed>>

\* The person's editor, or any program of theirs: no turns.
PersonEdit ==
    /\ pedits < PersonEdits
    /\ fver' = fver + 1 /\ author' = [author EXCEPT ![fver + 1] = "person"]
    /\ pedits' = pedits + 1
    /\ UNCHANGED <<tile, gate, takeover, written, byYou, turn, apc, steps, amine,
                   tpc, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended, holdId,
                   driveFrom, lost, missed>>

----------------------------------------------------------------------------
\* take_over

TakeOverHold(t) ==
    /\ tpc[t] = "idle"
    /\ IF tile \in Live /\ ~(Fixed /\ tile = "Driving")
       THEN Hold("Driving") /\ tpc' = [tpc EXCEPT ![t] = "held"]
       ELSE UNCHANGED <<tile, gate, holdId>> /\ tpc' = [tpc EXCEPT ![t] = "done"]
    /\ UNCHANGED <<takeover, fver, author, written, byYou, turn, apc, steps, amine,
                   pedits, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended,
                   driveFrom, lost, missed>>

\* Fixed: the snapshot waits for the person's writing turn.
TakeOverTurn(t) ==
    /\ Fixed /\ tpc[t] = "held" /\ turn = "free"
    /\ turn' = t /\ tpc' = [tpc EXCEPT ![t] = "turned"]
    /\ UNCHANGED <<tile, gate, takeover, fver, author, written, byYou, apc, steps,
                   amine, pedits, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended,
                   holdId, driveFrom, lost, missed>>

TakeOverSnap(t) ==
    /\ tpc[t] = IF Fixed THEN "turned" ELSE "held"
    /\ IF Fixed /\ ~(tile = "Driving" /\ takeover = None)
       \* Fixed: handed back, stopped, or snapshotted already: nothing to take.
       THEN /\ tpc' = [tpc EXCEPT ![t] = "done"] /\ turn' = "free"
            /\ UNCHANGED tsnap
       ELSE /\ tsnap' = [tsnap EXCEPT ![t] = fver]
            /\ tpc' = [tpc EXCEPT ![t] = "snapped"] /\ UNCHANGED turn
    /\ UNCHANGED <<tile, gate, takeover, fver, author, written, byYou, apc, steps,
                   amine, pedits, hpc, hh, hwc, hsaw, pauses, resumes, ended,
                   holdId, driveFrom, lost, missed>>

TakeOverSet(t) ==
    /\ tpc[t] = "snapped"
    /\ takeover' = tsnap[t]
    /\ driveFrom' = IF driveFrom = None THEN tsnap[t] ELSE driveFrom
    /\ tpc' = [tpc EXCEPT ![t] = "done"]
    /\ turn' = IF Fixed THEN "free" ELSE turn
    /\ UNCHANGED <<tile, gate, fver, author, written, byYou, apc, steps, amine,
                   pedits, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, ended,
                   holdId, lost, missed>>

----------------------------------------------------------------------------
\* hand_back

\* Fixed: the person's writing turn first.
HandBackTurn(h) ==
    /\ Fixed /\ hpc[h] = "idle" /\ turn = "free"
    /\ turn' = h /\ hpc' = [hpc EXCEPT ![h] = "turned"]
    /\ UNCHANGED <<tile, gate, takeover, fver, author, written, byYou, apc, steps,
                   amine, pedits, tpc, tsnap, hh, hwc, hsaw, pauses, resumes, ended,
                   holdId, driveFrom, lost, missed>>

\* `live`, the tile You're driving, `takeover.take()`.
HandBackClaim(h) ==
    /\ hpc[h] = IF Fixed THEN "turned" ELSE "idle"
    /\ IF tile = "Driving"
       THEN /\ hsaw' = [hsaw EXCEPT ![h] = holdId]
            /\ hh' = [hh EXCEPT ![h] = takeover]
            /\ takeover' = None
            \* No snapshot: "You handed back." and the release.
            /\ hpc' = [hpc EXCEPT ![h] = IF takeover = None THEN "recorded" ELSE "claimed"]
            /\ UNCHANGED turn
       ELSE /\ hpc' = [hpc EXCEPT ![h] = "done"]
            /\ turn' = IF Fixed THEN "free" ELSE turn
            /\ UNCHANGED <<hsaw, hh, takeover>>
    /\ UNCHANGED <<tile, gate, fver, author, written, byYou, apc, steps, amine,
                   pedits, tpc, tsnap, hwc, pauses, resumes, ended, holdId,
                   driveFrom, lost, missed>>

\* record_edits: `task.lock().written.clone()`.
HandBackClone(h) ==
    /\ hpc[h] = "claimed"
    /\ hwc' = [hwc EXCEPT ![h] = written]
    /\ hpc' = [hpc EXCEPT ![h] = "cloned"]
    /\ UNCHANGED <<tile, gate, takeover, fver, author, written, byYou, turn, apc,
                   steps, amine, pedits, tpc, tsnap, hh, hsaw, pauses, resumes,
                   ended, holdId, driveFrom, lost, missed>>

\* record_edits: what changed since the snapshot that `written` doesn't
\* explain is the person's, recorded "by you".
HandBackRecord(h) ==
    /\ hpc[h] = "cloned"
    /\ LET edit == fver # hh[h] /\ fver # hwc[h]
           mine == IF edit THEN byYou \cup {fver} ELSE byYou
       IN /\ byYou' = mine
          /\ missed' = (missed \/ (/\ driveFrom # None /\ fver > driveFrom
                                   /\ author[fver] = "person"
                                   /\ fver \notin mine))
    /\ hpc' = [hpc EXCEPT ![h] = "recorded"]
    /\ UNCHANGED <<tile, gate, takeover, fver, author, written, turn, apc, steps,
                   amine, pedits, tpc, tsnap, hh, hwc, hsaw, pauses, resumes, ended,
                   holdId, driveFrom, lost>>

HandBackRelease(h) ==
    /\ hpc[h] = "recorded"
    /\ IF tile \in Live
       THEN Release /\ lost' = (lost \/ holdId # hsaw[h])
       ELSE UNCHANGED <<tile, gate, driveFrom, lost>>
    /\ hpc' = [hpc EXCEPT ![h] = "done"]
    /\ turn' = IF Fixed THEN "free" ELSE turn
    /\ UNCHANGED <<takeover, fver, author, written, byYou, apc, steps, amine,
                   pedits, tpc, tsnap, hh, hwc, hsaw, pauses, resumes, ended,
                   holdId, missed>>

----------------------------------------------------------------------------
\* pause, resume, stop and the agent exiting (end_task).

Pause ==
    /\ pauses < MaxPauses /\ tile \in Live
    /\ ~(Fixed /\ tile = "Driving")
    /\ Hold("Paused") /\ pauses' = pauses + 1
    /\ UNCHANGED <<takeover, fver, author, written, byYou, turn, apc, steps, amine,
                   pedits, tpc, tsnap, hpc, hh, hwc, hsaw, resumes, ended,
                   driveFrom, lost, missed>>

\* `resume` refuses a task the person drives: hand back instead.
Resume ==
    /\ resumes < MaxResumes /\ tile = "Paused"
    /\ Release /\ resumes' = resumes + 1
    /\ UNCHANGED <<takeover, fver, author, written, byYou, turn, apc, steps, amine,
                   pedits, tpc, tsnap, hpc, hh, hwc, hsaw, pauses, ended, holdId,
                   lost, missed>>

End ==
    /\ ~ended /\ tile \in Live
    /\ tile' = "Ended" /\ gate' = "Ended" /\ ended' = TRUE /\ driveFrom' = None
    /\ UNCHANGED <<takeover, fver, author, written, byYou, turn, apc, steps, amine,
                   pedits, tpc, tsnap, hpc, hh, hwc, hsaw, pauses, resumes, holdId,
                   lost, missed>>

\* Every request answered and no step under way: nothing more happens (the
\* rest stutters), so TLC's deadlock check still finds a request stuck.
Done ==
    /\ \A t \in TakeOvers : tpc[t] = "done"
    /\ \A h \in HandBacks : hpc[h] = "done"
    /\ apc = "idle"
    /\ UNCHANGED vars

Next ==
    \/ Done
    \/ AgentStart \/ AgentTurn \/ AgentWrite \/ AgentRecord \/ PersonEdit
    \/ \E t \in TakeOvers : TakeOverHold(t) \/ TakeOverTurn(t) \/ TakeOverSnap(t)
                            \/ TakeOverSet(t)
    \/ \E h \in HandBacks : HandBackTurn(h) \/ HandBackClaim(h) \/ HandBackClone(h)
                            \/ HandBackRecord(h) \/ HandBackRelease(h)
    \/ Pause \/ Resume \/ End

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* What the OS promises.

\* AU-10: what's recorded "by you" was written by the person, not the agent.
AU10_YourEditsAreYours == \A v \in byYou : author[v] = "person"

\* AW-20: a hold made for the person (their pause, their take over) is let
\* go of by them alone: a hand back under way never releases a newer one.
AW20_AHoldForYouStaysHeld == ~lost

\* AW-21, ADR 0017: what the person changed while driving joins the task's
\* changes at hand back, to be undone like the agent's.
AW21_YourEditsJoinTheTask == ~missed

\* AW-21: the agent runs again only once the drive ended with a hand back,
\* which took its snapshot: never with a take over's snapshot outstanding.
AW21_TheAgentRunsOnlyAfterAHandBack == gate = "Run" => takeover = None

\* One driver at a time: while the person drives, the agent is held.
OneDriver == tile = "Driving" => gate = "Hold"
=============================================================================
