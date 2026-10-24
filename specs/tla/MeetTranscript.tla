---------------------------- MODULE MeetTranscript ----------------------------
(***************************************************************************)
(* What a person says in a transcribed meeting reaching its transcript     *)
(* (ADR 0045 T2): each line once, in order, none lost, and a "Hey Glass"   *)
(* request starting the person's agent once, while the organization's      *)
(* server goes out of reach and answers get lost.                          *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   apps/meet/src/transcribe.ts  Batcher: add, take, drain (and putBack  *)
(*                                before the fix)                          *)
(*   apps/meet/src/main.ts        hear, send (one call at a time; `unsent` *)
(*                                goes again as it was), stopHearing       *)
(*                                (leaving sends what is left)             *)
(*   crates/apps/meet/src/lib.rs  MeetProvider::transcribe: the lines      *)
(*                                posted as one sealed message, then each  *)
(*                                "Hey Glass" starts the agent             *)
(*   crates/bin/glass-app-bridge  relay: a call whose connection closed is *)
(*                                sent again once                          *)
(*                                                                         *)
(* Lines are numbered in the order heard; AskLines say "Hey Glass, ...".   *)
(* The transcript is the meeting's conversation at the console. A call's   *)
(* answer can be lost after glassd did the work (glassd restarting, the    *)
(* bridge sending it again): the page then sees the system out of reach.   *)
(* Posting to the console and its answer are one step here (chat's own     *)
(* post, ADR 0030).                                                        *)
(*                                                                         *)
(* Fixed = FALSE is the code before ADR 0052; TRUE is the code now: a      *)
(* batch keeps its id until glassd has it, glassd answers a batch it has   *)
(* had with what it did then, and the server out of reach is "not added    *)
(* yet", which the page tries again, not a refusal that stops hearing.     *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    N,           \* lines the person says
    AskLines,    \* those that ask their agent for something
    MaxOutages,  \* times the server goes out of reach
    MaxLost,     \* answers lost after glassd did the work
    Fixed

Idle == [st |-> "idle", lines |-> <<>>, id |-> 0, answer |-> "none"]

VARIABLES
    heard,     \* lines heard so far
    hearing,   \* the page is hearing (stopped when it left, or refused)
    leaving,   \* the person pressed Leave: what is left goes, then nothing
    queue,     \* Batcher: lines waiting
    unsent,    \* Fixed: the batch glassd hasn't taken, and its id
    unsentId,
    nextId,
    call,      \* meet.transcribe out: "idle", "glassd", "back"
    seen,      \* Fixed: batch ids glassd has posted
    posted,    \* the transcript: lines in the order posted
    started,   \* started[l]: agents the line started
    up, outages, lost

vars == <<heard, hearing, leaving, queue, unsent, unsentId, nextId, call, seen,
          posted, started, up, outages, lost>>

Lines == 1..N
Range(s) == {s[i] : i \in DOMAIN s}

Init ==
    /\ heard = 0 /\ hearing = TRUE /\ leaving = FALSE
    /\ queue = <<>> /\ unsent = <<>> /\ unsentId = 0 /\ nextId = 0
    /\ call = Idle /\ seen = {} /\ posted = <<>>
    /\ started = [l \in Lines |-> 0]
    /\ up = TRUE /\ outages = 0 /\ lost = 0

-----------------------------------------------------------------------------
\* The speech worker hears a line: Batcher.add.
Hear ==
    /\ hearing /\ ~leaving /\ heard < N
    /\ heard' = heard + 1
    /\ queue' = Append(queue, heard + 1)
    /\ UNCHANGED <<hearing, leaving, unsent, unsentId, nextId, call, seen,
                   posted, started, up, outages, lost>>

\* Leave: hearing stops (stopHearing), and what is left goes first.
Leave ==
    /\ ~leaving
    /\ leaving' = TRUE
    /\ UNCHANGED <<heard, hearing, queue, unsent, unsentId, nextId, call, seen,
                   posted, started, up, outages, lost>>

\* send(): one call at a time, every twelve seconds, at once for a
\* request, and while leaving until nothing is left. Fixed: a batch glassd
\* hasn't taken goes again as it was, with its id, before anything newer.
Send ==
    /\ call.st = "idle" /\ hearing
    /\ IF Fixed /\ unsent # <<>>
       THEN /\ call' = [st |-> "glassd", lines |-> unsent, id |-> unsentId,
                        answer |-> "none"]
            /\ UNCHANGED <<queue, nextId>>
       ELSE /\ queue # <<>>
            /\ call' = [st |-> "glassd", lines |-> queue, id |-> nextId + 1,
                        answer |-> "none"]
            /\ nextId' = nextId + 1
            /\ queue' = <<>>
    /\ UNCHANGED <<heard, hearing, leaving, unsent, unsentId, seen, posted,
                   started, up, outages, lost>>

\* MeetProvider::transcribe: the lines posted as one message, then each
\* request starts the agent. Fixed: a batch it has had is answered as
\* then; the server out of reach is "not added yet".
Glassd ==
    /\ call.st = "glassd"
    /\ IF Fixed /\ call.id \in seen
       THEN /\ call' = [call EXCEPT !.st = "back", !.answer = "ok"]
            /\ UNCHANGED <<posted, started, seen>>
       ELSE IF up
       THEN /\ posted' = posted \o call.lines
            /\ started' = [l \in Lines |->
                             IF l \in Range(call.lines) /\ l \in AskLines
                             THEN started[l] + 1 ELSE started[l]]
            /\ seen' = IF Fixed THEN seen \cup {call.id} ELSE seen
            /\ call' = [call EXCEPT !.st = "back", !.answer = "ok"]
       ELSE /\ call' = [call EXCEPT !.st = "back",
                                    !.answer = IF Fixed THEN "waiting" ELSE "refused"]
            /\ UNCHANGED <<posted, started, seen>>
    /\ UNCHANGED <<heard, hearing, leaving, queue, unsent, unsentId, nextId,
                   up, outages, lost>>

\* The answer is lost after glassd did the work: the page sees Offline.
LoseAnswer ==
    /\ call.st = "back" /\ call.answer = "ok" /\ lost < MaxLost
    /\ call' = [call EXCEPT !.answer = "lost"]
    /\ lost' = lost + 1
    /\ UNCHANGED <<heard, hearing, leaving, queue, unsent, unsentId, nextId,
                   seen, posted, started, up, outages>>

\* The page takes the answer.
Answer ==
    /\ call.st = "back"
    /\ call' = Idle
    /\ CASE call.answer = "ok" ->
              /\ unsent' = IF Fixed THEN <<>> ELSE unsent
              /\ UNCHANGED <<hearing, queue, unsentId>>
         [] call.answer \in {"lost", "waiting"} ->
              \* Fixed: kept as it was, to go again with its id.
              IF Fixed
              THEN /\ unsent' = call.lines /\ unsentId' = call.id
                   /\ UNCHANGED <<hearing, queue>>
              \* Batcher.putBack: first in the queue, with what came since.
              ELSE /\ queue' = call.lines \o queue
                   /\ UNCHANGED <<hearing, unsent, unsentId>>
         [] call.answer = "refused" ->
              \* "IT turned transcripts off": stopHearing(false).
              /\ hearing' = FALSE /\ queue' = <<>>
              /\ UNCHANGED <<unsent, unsentId>>
    /\ UNCHANGED <<heard, leaving, nextId, seen, posted, started, up, outages,
                   lost>>

Outage ==
    /\ up /\ outages < MaxOutages
    /\ up' = FALSE /\ outages' = outages + 1
    /\ UNCHANGED <<heard, hearing, leaving, queue, unsent, unsentId, nextId,
                   call, seen, posted, started, lost>>

Back ==
    /\ ~up /\ up' = TRUE
    /\ UNCHANGED <<heard, hearing, leaving, queue, unsent, unsentId, nextId,
                   call, seen, posted, started, outages, lost>>

Next == Hear \/ Leave \/ Send \/ Glassd \/ LoseAnswer \/ Answer \/ Outage \/ Back

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
TypeOK ==
    /\ heard \in 0..N /\ call.st \in {"idle", "glassd", "back"}
    /\ Range(posted) \subseteq Lines

\* ADR 0045: each line is in the transcript once.
ADR0045_EachLineOnce ==
    \A i, j \in DOMAIN posted : i < j => posted[i] # posted[j]

\* ADR 0045: in the order said.
ADR0045_InOrder ==
    \A i, j \in DOMAIN posted : i < j => posted[i] < posted[j]

\* ADR 0045: a request starts the person's agent once.
ADR0045_ARequestStartsOneTask ==
    \A l \in Lines : started[l] <= 1

\* ADR 0045 T2 ("Lines glassd can't take yet wait"): once the person has
\* left and nothing is left to send, everything they said is in the
\* transcript.
ADR0045_NothingSaidIsLost ==
    (leaving /\ call.st = "idle" /\ queue = <<>> /\ unsent = <<>>)
        => Range(posted) = 1..heard
=============================================================================
