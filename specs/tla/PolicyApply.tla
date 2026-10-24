----------------------------- MODULE PolicyApply -----------------------------
(***************************************************************************)
(* A release from IT reaching a computer while calls are under way (AD-3, *)
(* ADR 0021, ADR 0023 as amended for app sessions).                       *)
(*                                                                         *)
(* An open app's session follows the policy in force from its next call;  *)
(* an agent's task keeps the policy it started with, the scope the person *)
(* agreed to on the handoff card. One action X: version 1 of the policy   *)
(* asks for it, version 2 blocks it, version 3 lets it run. Two calls of  *)
(* the open app and one of an agent's task each decide, ask when policy   *)
(* says, wait for the person's writing turn (a reversible step, or one    *)
(* the person approved, holds it from its second look to its record) and *)
(* run. Another task's long write holds the turn at times.                *)
(*                                                                         *)
(* The code, each action one step:                                         *)
(*  - crates/svc/runtime/src/runtime.rs apply_policy: the runtime's        *)
(*    settings swapped in one pointer store (Release), then each open      *)
(*    app's follow_policy, under its task's policy lock (Follow).          *)
(*  - crates/svc/runtime/src/execute.rs pipeline: the policy read for the  *)
(*    people-only check and for decide (Read, Decide); ask (Answer); the   *)
(*    turn (TakeTurn); the second look, and since ADR 0053 the decision    *)
(*    again under the policy the session has now (Check); the provider     *)
(*    (Run).                                                               *)
(*                                                                         *)
(* Old holds the code before ADR 0053, to show TLC needs each fix:        *)
(*  - "decideOnce": a call was decided once, before its ask and its turn:  *)
(*    an answer given, or a turn taken, after a release that blocks it     *)
(*    still ran it;                                                        *)
(*  - "twoReads": the people-only check and decide read the session's     *)
(*    policy separately, so one call could be judged by two.              *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Old,          \* fixes switched off, as above
  MaxVersion    \* the last version IT releases

Versions == 1..MaxVersion
\* What each version says about X.
Tier(v) == CASE v = 1 -> "ask" [] v = 2 -> "never" [] OTHER -> "auto"

AppCalls == {"c1", "c2"}
AgentCalls == {"g1"}
Calls == AppCalls \cup AgentCalls
\* The agent's task started under version 1 and keeps it.
AgentPolicy == 1

NoCall == [phase |-> "idle", first |-> 0, decided |-> 0, approved |-> FALSE,
           readySeq |-> 0, checked |-> 0, checkedSeq |-> 0]

VARIABLES
  inForce,   \* the runtime's settings: the version applied
  session,   \* the open app session's policy (follow_policy)
  seq,       \* how many times the session has followed a release
  turn,      \* who holds the person's writing turn
  call       \* each call's progress

vars == <<inForce, session, seq, turn, call>>

TypeOK ==
  /\ inForce \in Versions /\ session \in Versions /\ seq \in Nat
  /\ turn \in {"none", "other"} \cup Calls
  /\ \A c \in Calls : call[c].phase \in
       {"idle", "read", "asking", "waiting", "turn", "checked", "ran", "refused"}

Init ==
  /\ inForce = 1 /\ session = 1 /\ seq = 0
  /\ turn = "none"
  /\ call = [c \in Calls |-> NoCall]

PolicyOf(c) == IF c \in AppCalls THEN session ELSE AgentPolicy

-----------------------------------------------------------------------------
(* IT's release, applied: the settings, then each open app.               *)

Release ==
  /\ inForce < MaxVersion /\ session = inForce
  /\ inForce' = inForce + 1
  /\ UNCHANGED <<session, seq, turn, call>>

Follow ==
  /\ session < inForce
  /\ session' = inForce /\ seq' = seq + 1
  /\ UNCHANGED <<inForce, turn, call>>

-----------------------------------------------------------------------------
(* A call.                                                                 *)

\* The people-only check's read of the session's policy. Fixed, the same
\* read is the one decide uses.
Read(c) ==
  /\ call[c].phase = "idle"
  /\ call' = [call EXCEPT ![c].phase = "read", ![c].first = PolicyOf(c)]
  /\ UNCHANGED <<inForce, session, seq, turn>>

Decide(c) ==
  /\ call[c].phase = "read"
  /\ LET v == IF "twoReads" \in Old THEN PolicyOf(c) ELSE call[c].first IN
     call' = [call EXCEPT ![c].decided = v, ![c].checked = v, ![c].checkedSeq = seq,
                          ![c].phase = CASE Tier(v) = "never" -> "refused"
                                         [] Tier(v) = "ask" -> "asking"
                                         [] OTHER -> "waiting"]
  /\ UNCHANGED <<inForce, session, seq, turn>>

\* The person answers on the desktop.
Answer(c) ==
  /\ call[c].phase = "asking"
  /\ \/ call' = [call EXCEPT ![c].phase = "waiting", ![c].approved = TRUE]
     \/ call' = [call EXCEPT ![c].phase = "refused"]
  /\ UNCHANGED <<inForce, session, seq, turn>>

\* The person's writing turn: from here nothing else waits.
TakeTurn(c) ==
  /\ call[c].phase = "waiting" /\ turn = "none"
  /\ turn' = c
  /\ call' = [call EXCEPT ![c].phase = IF "decideOnce" \in Old THEN "checked" ELSE "turn",
                          ![c].readySeq = seq]
  /\ UNCHANGED <<inForce, session, seq>>

\* Decided again by what the session has now. What now asks wasn't asked:
\* refused, to be called again.
Check(c) ==
  /\ call[c].phase = "turn"
  /\ LET v == PolicyOf(c) IN
     IF Tier(v) = "never" \/ (Tier(v) = "ask" /\ ~call[c].approved)
     THEN /\ call' = [call EXCEPT ![c].phase = "refused", ![c].checked = v,
                                  ![c].checkedSeq = seq]
          /\ turn' = "none"
     ELSE /\ call' = [call EXCEPT ![c].phase = "checked", ![c].checked = v,
                                  ![c].checkedSeq = seq]
          /\ UNCHANGED turn
  /\ UNCHANGED <<inForce, session, seq>>

Run(c) ==
  /\ call[c].phase = "checked"
  /\ call' = [call EXCEPT ![c].phase = "ran"]
  /\ turn' = "none"
  /\ UNCHANGED <<inForce, session, seq>>

\* Another task's long write (an approved send) holds the turn.
OtherTakes ==
  /\ turn = "none" /\ turn' = "other"
  /\ UNCHANGED <<inForce, session, seq, call>>
OtherDone ==
  /\ turn = "other" /\ turn' = "none"
  /\ UNCHANGED <<inForce, session, seq, call>>

Next ==
  \/ Release \/ Follow \/ OtherTakes \/ OtherDone
  \/ \E c \in Calls : Read(c) \/ Decide(c) \/ Answer(c) \/ TakeTurn(c) \/ Check(c) \/ Run(c)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* What IT and the person are promised.                                    *)

\* AD-3: a call that waited, for an answer or the person's turn, runs only
\* as the policy its session had once it stopped waiting decides: a release
\* that blocks it, applied meanwhile, stops it.
AD3_ACallThatWaitedIsDecidedAgain ==
  \A c \in AppCalls : call[c].phase = "ran" => call[c].checkedSeq >= call[c].readySeq

\* No call runs on a decision that blocks it, and what asks was answered.
AD3_NothingRunsBlockedOrUnasked ==
  \A c \in Calls : call[c].phase = "ran" =>
    /\ Tier(call[c].checked) # "never"
    /\ (Tier(call[c].checked) = "ask" => call[c].approved)

\* An agent's task keeps the policy it started with (ADR 0023).
ADR0023_AnAgentTaskKeepsItsPolicy ==
  \A c \in AgentCalls : call[c].phase \notin {"idle", "read"} => call[c].checked = AgentPolicy

\* Each decision is one whole policy's.
AD3_OneWholePolicy ==
  \A c \in Calls : call[c].phase \notin {"idle", "read"} => call[c].decided = call[c].first
=============================================================================
