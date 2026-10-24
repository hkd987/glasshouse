------------------------------- MODULE ChatPost -------------------------------
(***************************************************************************)
(* Posting a chat message while the network loses answers and someone     *)
(* changes who is in the conversation (ADR 0030 C1, C2; CF-91): a message  *)
(* the person wrote once is posted once, and a post said to be done was.   *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/apps/chat/src/lib.rs     send_body: seal under the epoch this  *)
(*                                   computer has, post; an old epoch is   *)
(*                                   caught up (sync_once) and sealed      *)
(*                                   again, once; an answer lost is asked  *)
(*                                   again with the same message           *)
(*   crates/bin/glass-console/src/chat.rs  Chat::post: an old epoch is     *)
(*                                   refused before a message id it has    *)
(*                                   ("posted already"); Chat::rotate      *)
(*   crates/svc/gateway-providers/src/console.rs  a request that times     *)
(*                                   out or fails says Unreachable, though *)
(*                                   the console may have taken it, or     *)
(*                                   take it later                         *)
(*                                                                         *)
(* One computer posts what the person writes; requests in flight may be    *)
(* taken by the console late, or never. Another member's computer adds     *)
(* people (a new epoch), and this computer's syncs learn it.              *)
(*                                                                         *)
(* Old is the set of the code's behaviours before ADR 0052 that a          *)
(* configuration keeps; {} is the code now.                               *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Writes,        \* what the person writes, each once
    MaxLost,       \* answers lost
    MaxRotations,  \* new epochs another member makes
    Old            \* "tryAgain": a lost answer said "Nothing was queued;
                   \*   try again", and the person wrote it again;
                   \* "resealAfterLost": a first fix, asking again with
                   \*   the same message, that took an old-epoch refusal of
                   \*   the second request as "not posted" and sealed it
                   \*   again

VARIABLES
    epoch,      \* the conversation's epoch on the console
    known,      \* the epoch this computer has
    log,        \* messages the console took: [w, id]
    inflight,   \* requests sent and not yet handled: [w, id, e, n]
    pc,         \* pc[w]: "write", "wait", "done" or "maybe" (told it may
                \*   have been posted)
    msg,        \* msg[w]: the sealed message in hand: [id, e]
    att,        \* att[w]: the request w waits on
    answer,     \* answer[w]: its answer, "none" while it waits
    again,      \* again[w]: the one sealing again for an old epoch is left
    lostOnce,   \* lostOnce[w]: an answer for this message was lost
    nextId, lost, rotations

vars == <<epoch, known, log, inflight, pc, msg, att, answer, again, lostOnce,
          nextId, lost, rotations>>

Init ==
    /\ epoch = 1 /\ known = 1
    /\ log = {} /\ inflight = {}
    /\ pc = [w \in Writes |-> "write"]
    /\ msg = [w \in Writes |-> [id |-> 0, e |-> 0]]
    /\ att = [w \in Writes |-> 0]
    /\ answer = [w \in Writes |-> "none"]
    /\ again = [w \in Writes |-> TRUE]
    /\ lostOnce = [w \in Writes |-> FALSE]
    /\ nextId = 1 /\ lost = 0 /\ rotations = 0

\* The request for w's message in hand goes, as attempt att[w] + 1.
Send(w, m) ==
    /\ inflight' = inflight \cup {[w |-> w, id |-> m.id, e |-> m.e, n |-> att[w] + 1]}
    /\ att' = [att EXCEPT ![w] = @ + 1]
    /\ pc' = [pc EXCEPT ![w] = "wait"]
    /\ answer' = [answer EXCEPT ![w] = "none"]
    /\ msg' = [msg EXCEPT ![w] = m]

\* chat.post: sealed under the epoch this computer has, with a new id.
Write(w) ==
    /\ pc[w] = "write"
    /\ Send(w, [id |-> nextId, e |-> known])
    /\ nextId' = nextId + 1
    /\ again' = [again EXCEPT ![w] = TRUE]
    /\ lostOnce' = [lostOnce EXCEPT ![w] = FALSE]
    /\ UNCHANGED <<epoch, known, log, lost, rotations>>

-----------------------------------------------------------------------------
\* The console and the network.

\* Chat::post handles a request: an old epoch first, then an id it has.
Handle(r) ==
    LET result == IF r.e # epoch THEN "old"
                  ELSE IF \E m \in log : m.id = r.id THEN "already"
                  ELSE "ok"
    IN /\ inflight' = inflight \ {r}
       /\ log' = IF result = "ok" THEN log \cup {[w |-> r.w, id |-> r.id]}
                 ELSE log
       /\ answer' = IF pc[r.w] = "wait" /\ att[r.w] = r.n /\ answer[r.w] = "none"
                    THEN [answer EXCEPT ![r.w] = result] ELSE answer
       /\ UNCHANGED <<epoch, known, pc, msg, att, again, lostOnce, nextId,
                      lost, rotations>>

\* A request that never arrives.
Vanish(r) ==
    /\ inflight' = inflight \ {r}
    /\ UNCHANGED <<epoch, known, log, pc, msg, att, answer, again, lostOnce,
                   nextId, lost, rotations>>

\* The request times out or its answer is lost: Unreachable, whether or
\* not the console has taken it or will.
TimeOut(w) ==
    /\ pc[w] = "wait" /\ answer[w] = "none"
    /\ lost < MaxLost
    /\ answer' = [answer EXCEPT ![w] = "unreachable"]
    /\ lost' = lost + 1
    /\ UNCHANGED <<epoch, known, log, inflight, pc, msg, att, again, lostOnce,
                   nextId, rotations>>

\* Another member adds people: Chat::rotate takes the epoch after the
\* current one.
Rotate ==
    /\ rotations < MaxRotations
    /\ epoch' = epoch + 1 /\ rotations' = rotations + 1
    /\ UNCHANGED <<known, log, inflight, pc, msg, att, answer, again,
                   lostOnce, nextId, lost>>

\* glassd's long poll brings the new epoch.
Sync ==
    /\ known # epoch
    /\ known' = epoch
    /\ UNCHANGED <<epoch, log, inflight, pc, msg, att, answer, again,
                   lostOnce, nextId, lost, rotations>>

-----------------------------------------------------------------------------
\* send_body takes the answer.
Stay == UNCHANGED <<known, msg, att, answer, again, lostOnce, nextId, inflight>>

Answer(w) ==
    LET a == answer[w]
    IN /\ pc[w] = "wait" /\ a # "none"
       /\ CASE a \in {"ok", "already"} ->
                 /\ pc' = [pc EXCEPT ![w] = "done"]
                 /\ Stay
            [] a = "old" /\ lostOnce[w] /\ "resealAfterLost" \notin Old ->
                 \* The first request may have been taken before the epoch
                 \* changed: it may have been posted.
                 /\ pc' = [pc EXCEPT ![w] = "maybe"]
                 /\ Stay
            [] a = "old" /\ again[w] ->
                 \* Caught up (sync_once) and sealed again, once, as new.
                 /\ known' = epoch
                 /\ Send(w, [id |-> nextId, e |-> epoch])
                 /\ nextId' = nextId + 1
                 /\ again' = [again EXCEPT ![w] = FALSE]
                 /\ UNCHANGED lostOnce
            [] a = "old" ->
                 \* The refusal is shown; nothing was posted, and the
                 \* person writes it again.
                 /\ pc' = [pc EXCEPT ![w] = "write"]
                 /\ Stay
            [] a = "unreachable" /\ "tryAgain" \in Old ->
                 \* "Nothing was queued; try again when it's back."
                 /\ pc' = [pc EXCEPT ![w] = "write"]
                 /\ Stay
            [] a = "unreachable" /\ ~lostOnce[w] ->
                 \* The same message again: taken now, or "posted already".
                 /\ Send(w, msg[w])
                 /\ lostOnce' = [lostOnce EXCEPT ![w] = TRUE]
                 /\ UNCHANGED <<known, again, nextId>>
            [] OTHER ->
                 \* Unreachable twice: it may have been posted.
                 /\ pc' = [pc EXCEPT ![w] = "maybe"]
                 /\ Stay
       /\ UNCHANGED <<epoch, log, lost, rotations>>

Next ==
    \/ \E w \in Writes : Write(w) \/ TimeOut(w) \/ Answer(w)
    \/ \E r \in inflight : Handle(r) \/ Vanish(r)
    \/ Rotate \/ Sync

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* What Chat promises.

\* What the person wrote once is posted at most once.
CF91_PostedOnce ==
    \A w \in Writes : Cardinality({m \in log : m.w = w}) <= 1

\* A post said to be done was taken.
CF91_DoneIsPosted ==
    \A w \in Writes : pc[w] = "done" => \E m \in log : m.w = w

TypeOK ==
    /\ pc \in [Writes -> {"write", "wait", "done", "maybe"}]
    /\ lost <= MaxLost /\ rotations <= MaxRotations
=============================================================================
