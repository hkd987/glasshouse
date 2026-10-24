--------------------------- MODULE CalendarInvite ---------------------------
(***************************************************************************)
(* Invitations to an event on the person's calendar (NV-3, CF-21, DA-11):  *)
(* an agent asks to invite people, and while the person decides, the event *)
(* is moved, made a Glasshouse meeting or not, and changes are undone.     *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/apps/calendar/src/lib.rs: preview of calendar.invite (what   *)
(*    the ask shows: the event as it is then), invoke (it loads the event *)
(*    again, makes the meeting's conversation if it's a meeting not made  *)
(*    yet, and writes the invitation from what it loaded), changed (an    *)
(*    update can't take the meeting off once its invitations went out),   *)
(*    revert (undo's hook).                                               *)
(*  - crates/svc/runtime/src/execute.rs: preview, ask, answer, invoke; a  *)
(*    reversible step (calendar.update, and undo) holds the person's      *)
(*    writing turn.                                                       *)
(*  - crates/svc/runtime/src/undo.rs: the event's file restored from its  *)
(*    snapshot, then the provider's revert, under the turn.               *)
(*                                                                         *)
(* Before ADR 0052 TLC found, against the code as it was:                  *)
(*  - SameAsAsked = FALSE: the invitation sent was the event as it was    *)
(*    when the person said yes, not as the ask showed it: moved, or made  *)
(*    a meeting, while they decided. What left wasn't what was approved.  *)
(*  - KeepMeeting = FALSE: undoing the change that made the event a       *)
(*    meeting, after its invitations went out, took the meeting off the   *)
(*    organizer's event while everyone invited still had it.             *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS
  SameAsAsked,  \* the fix: after the answer, the call is previewed again
                \* under the turn, and refused if it changed
  KeepMeeting,  \* the fix: revert puts a meeting whose invitations went
                \* out back on the event undo restored
  MaxInvites,   \* calendar.invite calls
  MaxUpdates    \* calendar.update calls (moves, meeting on or off)

Events == [when : {1, 2}, meeting : BOOLEAN]

VARIABLES
  ev,       \* the event's file
  made,     \* its meeting's conversation was made: invitations went out
  inv,      \* each invite call: "idle", "asking", "approved", "done"
  seen,     \* the event as each invite's ask showed it
  sent,     \* invitations that left: what was approved, and what went
  hist,     \* the event before each update not undone, for undo
  updates   \* updates so far

vars == <<ev, made, inv, seen, sent, hist, updates>>
Calls == 1..MaxInvites
Start == [when |-> 1, meeting |-> FALSE]

Init ==
  /\ ev = Start
  /\ made = FALSE
  /\ inv = [i \in Calls |-> "idle"]
  /\ seen = [i \in Calls |-> Start]
  /\ sent = {}
  /\ hist = <<>>
  /\ updates = 0

(* preview: the ask shows the event as it is now.                        *)
Ask(i) ==
  /\ inv[i] = "idle"
  /\ inv' = [inv EXCEPT ![i] = "asking"]
  /\ seen' = [seen EXCEPT ![i] = ev]
  /\ UNCHANGED <<ev, made, sent, hist, updates>>

(* The person approves what the ask showed.                              *)
Approve(i) ==
  /\ inv[i] = "asking"
  /\ inv' = [inv EXCEPT ![i] = "approved"]
  /\ UNCHANGED <<ev, made, seen, sent, hist, updates>>

(* invoke: the event loaded again; a meeting's conversation made first;  *)
(* the invitation written from what was loaded. With the fix, under the  *)
(* turn, a call whose preview changed is refused and nothing leaves.     *)
Invoke(i) ==
  /\ inv[i] = "approved"
  /\ inv' = [inv EXCEPT ![i] = "done"]
  /\ IF SameAsAsked /\ ev /= seen[i]
       THEN UNCHANGED <<made, sent>>
       ELSE /\ made' = (made \/ ev.meeting)
            /\ sent' = sent \cup {[approved |-> seen[i], went |-> ev]}
  /\ UNCHANGED <<ev, seen, hist, updates>>

(* calendar.update, under the turn: moved, made a meeting, or the meeting *)
(* taken off, which `changed` refuses once its invitations went out.     *)
Update(new) ==
  /\ updates < MaxUpdates
  /\ new /= ev
  /\ ev.meeting /\ ~new.meeting => ~made
  /\ updates' = updates + 1
  /\ hist' = Append(hist, ev)
  /\ ev' = new
  /\ UNCHANGED <<made, inv, seen, sent>>

(* Undo of the latest update not undone: its file back from the snapshot, *)
(* then the provider's revert, under the turn.                           *)
Undo ==
  /\ hist /= <<>>
  /\ LET back == hist[Len(hist)]
     IN ev' = IF KeepMeeting /\ made /\ ~back.meeting
                THEN [back EXCEPT !.meeting = TRUE]
                ELSE back
  /\ hist' = SubSeq(hist, 1, Len(hist) - 1)
  /\ UNCHANGED <<made, inv, seen, sent, updates>>

Next ==
  \/ \E i \in Calls : Ask(i) \/ Approve(i) \/ Invoke(i)
  \/ \E new \in Events : Update(new)
  \/ Undo

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ ev \in Events
  /\ inv \in [Calls -> {"idle", "asking", "approved", "done"}]

(* What leaves is what the person approved (DA-11, NV-3: invitations     *)
(* ask first because they can't be recalled).                            *)
DA11_WhatLeavesIsWhatWasApproved == \A s \in sent : s.went = s.approved

(* A meeting whose invitations went out stays on its organizer's event,   *)
(* where they join it (CF-21).                                           *)
CF21_InvitedMeetingStaysOnTheEvent == made => ev.meeting
=============================================================================
