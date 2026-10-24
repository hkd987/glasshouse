---------------------------- MODULE ModelGateway ----------------------------
(***************************************************************************)
(* Where a task's data may go, and what a call costs (ADR 0018, ADR 0041,  *)
(* ADR 0043, ADR 0044, DP-2, DP-3, DP-10, DP-11, MP-8): tasks of two       *)
(* people read and write files and call models at once; the label of what  *)
(* a task has read routes each call; the gateway redacts it, fails over    *)
(* along the route and puts the values back; the desktop agent steps up    *)
(* to dearer models.                                                       *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/svc/runtime/src/execute.rs  pipeline: the preview and its      *)
(*                       read_label, route_model before any ask, the       *)
(*                       provider's invoke, then the task's label raised   *)
(*                       under its lock before the result goes back        *)
(*   crates/svc/runtime/src/model.rs    call_label, route_model,           *)
(*                       send_model ("stronger")                           *)
(*   crates/svc/runtime/src/labels.rs   label_of: folder rules and the     *)
(*                       label stored on the file                          *)
(*   crates/lib/platform/src/fs.rs      ConfinedDir::write_as: a staged    *)
(*                       file renamed over the old one                     *)
(*   crates/lib/policy/src/models.rs    route, route_for, ordered,         *)
(*                       can_step_up                                       *)
(*   crates/svc/gateway/src/gateway.rs  Gateway::complete: a vault per     *)
(*                       call, replays only to the provider that wrote     *)
(*                       them, failover, a refusal billed, restore         *)
(*   crates/svc/gateway/src/redact.rs   Vault::placeholder, restore        *)
(*   crates/bin/glass-desktop-agent/src/main.rs  Ladder (args,             *)
(*                       answered_by, step_up) and the loop: a cut-off     *)
(*                       step is asked again a step up; the model may ask  *)
(*                       for a stronger model                              *)
(*                                                                         *)
(* Labels are 0 public, 1 internal, 2 confidential; unlabeled data is      *)
(* public. Three models: haiku (Anthropic, approved for public data),      *)
(* sonnet (Anthropic, up to internal) and opus (Bedrock, up to             *)
(* confidential), dearer in that order; policy lets agents choose by       *)
(* cost. Person p1 has f1, labeled confidential on the file itself, and    *)
(* f2, unlabeled; p2 has f3, in an internal folder. A file holds values;   *)
(* each value's origin is the label of where it came from.                 *)
(*                                                                         *)
(* An agent's conversation only grows (the desktop agent's), and it sends  *)
(* the whole conversation with each call. A task has at most one read and *)
(* one model call under way at once.                                       *)
(*                                                                         *)
(* KeepLabel = FALSE is the code before ADR 0052: a rewrite dropped the    *)
(* label stored on the file. LabelWrites = FALSE is the code before it     *)
(* too: what a task wrote carried no label of what it had read (DP-11);    *)
(* and RecheckRead = FALSE, a read raised the task's label by its preview  *)
(* alone, older than a write landing before the read.                      *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Tasks,        \* tasks running at once
    Other,        \* the task that is p2's; the rest are p1's
    MaxCalls,     \* model calls a task of p1's makes (p2's makes one)
    MaxReads,     \* reads a task makes
    MaxWrites,    \* writes, all tasks together
    MaxFailures,  \* attempts that fail or are refused, all told
    KeepLabel,    \* ConfinedDir::write_as keeps the stored label (ADR 0052)
    LabelWrites,  \* files.write labels the file with the task's label (DP-11)
    RecheckRead   \* the pipeline reads a read's labels again after it (DP-11)

Max(x, y) == IF x > y THEN x ELSE y
None == "none"

Owner(t) == IF t = Other THEN "p2" ELSE "p1"
Files == {"f1", "f2", "f3"}
FileOwner(f) == IF f = "f3" THEN "p2" ELSE "p1"
Folder(f) == IF f = "f3" THEN 1 ELSE 0
Values == {"a", "b", "c"}
Origin(v) == CASE v = "a" -> 2 [] v = "b" -> 0 [] v = "c" -> 1
ValueOwner(v) == IF v = "c" THEN "p2" ELSE "p1"
Unlabeled == 0

Models == {"haiku", "sonnet", "opus"}
Provider(m) == IF m = "opus" THEN "bedrock" ELSE "anthropic"
Price(m) == CASE m = "haiku" -> 1 [] m = "sonnet" -> 2 [] m = "opus" -> 3
\* [models.routes]: the models approved for each label.
Routes(l) == CASE l = 0 -> {"haiku", "sonnet", "opus"}
               [] l = 1 -> {"sonnet", "opus"}
               [] l = 2 -> {"opus"}
\* The most sensitive label policy approves a model for.
Approved(m) == CASE m = "haiku" -> 0 [] m = "sonnet" -> 1 [] m = "opus" -> 2

Range(s) == {s[i] : i \in 1..Len(s)}
IsPrefix(p, s) == Len(p) <= Len(s) /\ \A i \in 1..Len(p) : p[i] = s[i]

\* Route::ordered for prefer = cheapest: the cheapest first, or with `above`
\* the dearer ones cheapest first, then its price, then the cheaper ones
\* nearest first.
Rank(above, m) ==
    IF above = None THEN Price(m)
    ELSE IF Price(m) > Price(above) THEN Price(m)
    ELSE IF Price(m) = Price(above) THEN 10
    ELSE 20 + (10 - Price(m))
RECURSIVE Ordered(_, _)
Ordered(route, above) ==
    IF route = {} THEN <<>>
    ELSE LET m == CHOOSE x \in route : \A y \in route : Rank(above, x) <= Rank(above, y)
         IN <<m>> \o Ordered(route \ {m}, above)

\* ModelRules::can_step_up: a model on the route is dearer.
CanStepUp(route, m) == \E x \in route : Price(x) > Price(m)

\* Values in a fixed order, as a read appends them to the conversation.
VRank(v) == CASE v = "a" -> 1 [] v = "b" -> 2 [] v = "c" -> 3
RECURSIVE AsSeq(_)
AsSeq(S) ==
    IF S = {} THEN <<>>
    ELSE LET v == CHOOSE x \in S : \A y \in S : VRank(x) <= VRank(y)
         IN <<v>> \o AsSeq(S \ {v})

NoCall == [phase |-> "idle", hist |-> <<>>, under |-> <<>>, label |-> 0,
           route |-> {}, order |-> <<>>, at |-> 0, above |-> None,
           replay |-> [prov |-> None, hist |-> <<>>], model |-> None,
           restored |-> {}, charged |-> 0, lost |-> 0]
NoRead == [at |-> "idle", f |-> "f1", label |-> 0, got |-> {}]

VARIABLES
    content,    \* what each file holds
    stored,     \* the label stored on each file (0: none)
    dataLabel,  \* each task's label: the most sensitive it has read
    held,       \* each agent's conversation: the values it was given, in order
    under,      \* the label each value the agent holds was read under
    rd,         \* each task's read under way
    reads, writes, calls, failures,
    mc,         \* each task's model call under way
    ladder,     \* the desktop agent's Ladder
    replay,     \* the sealed replay the agent holds, and what it said
    ended       \* the agent gave up (no model could help)

vars == <<content, stored, dataLabel, held, under, rd, reads, writes, calls,
          failures, mc, ladder, replay, ended>>

LabelOf(f) == Max(Folder(f), stored[f])

Init ==
    /\ content = [f \in Files |-> CASE f = "f1" -> {"a"} [] f = "f2" -> {"b"} [] f = "f3" -> {"c"}]
    /\ stored = [f \in Files |-> IF f = "f1" THEN 2 ELSE 0]
    /\ dataLabel = [t \in Tasks |-> 0]
    /\ held = [t \in Tasks |-> <<>>]
    /\ under = [t \in Tasks |-> [v \in Values |-> 0]]
    /\ rd = [t \in Tasks |-> NoRead]
    /\ reads = [t \in Tasks |-> 0] /\ writes = 0
    /\ calls = [t \in Tasks |-> 0] /\ failures = 0
    /\ mc = [t \in Tasks |-> NoCall]
    /\ ladder = [t \in Tasks |-> [above |-> None, answered |-> None, stronger |-> FALSE]]
    /\ replay = [t \in Tasks |-> [prov |-> None, hist |-> <<>>]]
    /\ ended = [t \in Tasks |-> FALSE]

-----------------------------------------------------------------------------
(* Reading a file: the preview names it and its label is read then; the    *)
(* provider reads it; the label is raised under the task's lock before the *)
(* agent gets the result.                                                  *)

ReadPreview(t, f) ==
    /\ ~ended[t] /\ rd[t].at = "idle" /\ reads[t] < MaxReads
    /\ FileOwner(f) = Owner(t)
    /\ rd' = [rd EXCEPT ![t] = [at |-> "previewed", f |-> f, label |-> LabelOf(f), got |-> {}]]
    /\ reads' = [reads EXCEPT ![t] = @ + 1]
    /\ UNCHANGED <<content, stored, dataLabel, held, under, writes, calls,
                   failures, mc, ladder, replay, ended>>

ReadInvoke(t) ==
    /\ rd[t].at = "previewed"
    /\ rd' = [rd EXCEPT ![t].at = "invoked", ![t].got = content[rd[t].f]]
    /\ UNCHANGED <<content, stored, dataLabel, held, under, reads, writes,
                   calls, failures, mc, ladder, replay, ended>>

ReadReturn(t) ==
    /\ rd[t].at = "invoked"
    /\ LET r == rd[t] IN
       \* execute.rs: the labels read again once the provider has read; a
       \* write after that changes the file, not what was read.
       /\ LET label == IF RecheckRead THEN Max(r.label, LabelOf(r.f)) ELSE r.label IN
          /\ dataLabel' = [dataLabel EXCEPT ![t] = Max(@, label)]
          /\ held' = [held EXCEPT ![t] = @ \o AsSeq(r.got \ Range(@))]
          /\ under' = [under EXCEPT ![t] =
                         [v \in Values |-> IF v \in r.got THEN Max(@[v], label) ELSE @[v]]]
    /\ rd' = [rd EXCEPT ![t] = NoRead]
    /\ UNCHANGED <<content, stored, reads, writes, calls, failures, mc,
                   ladder, replay, ended>>

\* files.write: what the agent holds, written over a file of its person's.
\* One step: the person's writes take turns, and the rename is atomic. The
\* new file gets the old one's label and the task's, the higher, before the
\* rename (ConfinedDir::write_labeled).
Write(t, f) ==
    /\ ~ended[t] /\ writes < MaxWrites /\ held[t] # <<>>
    /\ FileOwner(f) = Owner(t)
    /\ content' = [content EXCEPT ![f] = Range(held[t])]
    /\ stored' = [stored EXCEPT ![f] = Max(IF KeepLabel THEN @ ELSE 0,
                                           IF LabelWrites THEN dataLabel[t] ELSE 0)]
    /\ writes' = writes + 1
    /\ UNCHANGED <<dataLabel, held, under, rd, reads, calls, failures, mc,
                   ladder, replay, ended>>

\* The person saves a file in one of their apps, through the same
\* ConfinedDir: the same content, revised.
PersonWrites(f) ==
    /\ writes < MaxWrites
    /\ stored' = [stored EXCEPT ![f] = IF KeepLabel THEN @ ELSE 0]
    /\ writes' = writes + 1
    /\ UNCHANGED <<content, dataLabel, held, under, rd, reads, calls, failures,
                   mc, ladder, replay, ended>>

-----------------------------------------------------------------------------
(* A model call: the agent sends its whole conversation, with the replay   *)
(* it holds and the ladder's routing arguments.                            *)

Issue(t) ==
    /\ ~ended[t] /\ mc[t].phase = "idle"
    /\ calls[t] < (IF t = Other THEN 1 ELSE MaxCalls)
    /\ mc' = [mc EXCEPT ![t] = [NoCall EXCEPT !.phase = "issued", !.hist = held[t],
                                  !.under = [i \in 1..Len(held[t]) |-> under[t][held[t][i]]],
                                  !.above = ladder[t].above, !.replay = replay[t]]]
    /\ UNCHANGED <<content, stored, dataLabel, held, under, rd, reads, writes,
                   calls, failures, ladder, replay, ended>>

\* route_model, before any ask: call_label under the task's lock, then
\* route_for. No route is a refusal the agent reads, and the desktop agent
\* stops there.
Route(t) ==
    /\ mc[t].phase = "issued"
    /\ LET label == Max(dataLabel[t], Unlabeled)
           route == Routes(label)
       IN IF route = {}
          THEN /\ mc' = [mc EXCEPT ![t] = NoCall]
               /\ ended' = [ended EXCEPT ![t] = TRUE]
          ELSE /\ mc' = [mc EXCEPT ![t].phase = "sending", ![t].label = label,
                           ![t].route = route, ![t].order = Ordered(route, mc[t].above),
                           ![t].at = 1]
               /\ UNCHANGED ended
    /\ UNCHANGED <<content, stored, dataLabel, held, under, rd, reads, writes,
                   calls, failures, ladder, replay>>

\* The agent gives up: an error from model_complete ends the desktop agent.
GiveUp(t) ==
    /\ mc' = [mc EXCEPT ![t] = NoCall]
    /\ ended' = [ended EXCEPT ![t] = TRUE]

\* One attempt on the route, to the model at `at`. What it answers:
\*   ok, cutoff  billed, answered (a cutoff stops at max_tokens)
\*   refusal     billed, and the call ends (ADR 0043)
\*   rejected    not billed, the call ends (another would refuse it too)
\*   down        not taken: the next model takes over (MP-8)
\*   lost        taken and billed, but its reply never came (timed out,
\*               the connection dropped): the next model takes over too
Attempt(t, o) ==
    /\ mc[t].phase = "sending"
    /\ o \in {"refusal", "rejected", "down", "lost"} => failures < MaxFailures
    /\ LET c == mc[t]
           m == c.order[c.at]
           billed == IF o \in {"ok", "cutoff", "refusal", "lost"} THEN 1 ELSE 0
           next == [c EXCEPT !.charged = @ + billed, !.lost = @ + (IF o = "lost" THEN 1 ELSE 0)]
       IN CASE o \in {"ok", "cutoff"} ->
                 \* Vault::restore: only this call's placeholders come back.
                 /\ mc' = [mc EXCEPT ![t] = [next EXCEPT !.phase = o, !.model = m,
                                               !.restored = Range(c.hist)]]
                 /\ UNCHANGED <<failures, ended>>
            [] o \in {"refusal", "rejected"} ->
                 /\ failures' = failures + 1
                 /\ GiveUp(t)
            [] o \in {"down", "lost"} ->
                 /\ failures' = failures + 1
                 /\ IF c.at < Len(c.order)
                    THEN /\ mc' = [mc EXCEPT ![t] = [next EXCEPT !.at = @ + 1]]
                         /\ UNCHANGED ended
                    ELSE GiveUp(t)
    /\ UNCHANGED <<content, stored, dataLabel, held, under, rd, reads, writes,
                   calls, ladder, replay>>

\* Ladder::step_up: to the cheapest dearer than the model that last answered.
StepUp(l) ==
    IF l.answered # None /\ l.stronger /\ l.above # l.answered
    THEN [l EXCEPT !.above = l.answered]
    ELSE l

\* answered_by: the reply's model, and whether a dearer approved model is on
\* the route (send_model's "stronger").
AnsweredBy(l, c) ==
    [l EXCEPT !.answered = c.model, !.stronger = CanStepUp(c.route, c.model)]

\* The reply reaches the agent. A step cut off at max_tokens is asked again
\* a step up, nothing of it kept, or the agent stops at the top. Otherwise
\* it keeps the reply, sealed, and the model may ask for a stronger model.
Answer(t) ==
    /\ mc[t].phase \in {"ok", "cutoff"}
    /\ LET c == mc[t]
           l == AnsweredBy(ladder[t], c)
       IN IF c.phase = "cutoff"
          THEN /\ IF StepUp(l) # l
                  THEN /\ ladder' = [ladder EXCEPT ![t] = StepUp(l)]
                       /\ UNCHANGED ended
                  ELSE /\ ladder' = [ladder EXCEPT ![t] = l]
                       /\ ended' = [ended EXCEPT ![t] = TRUE]
               /\ UNCHANGED replay
          ELSE /\ \/ ladder' = [ladder EXCEPT ![t] = l]
                  \/ ladder' = [ladder EXCEPT ![t] = StepUp(l)]
               /\ replay' = [replay EXCEPT ![t] = [prov |-> Provider(c.model), hist |-> c.hist]]
               /\ UNCHANGED ended
    /\ mc' = [mc EXCEPT ![t] = NoCall]
    /\ calls' = [calls EXCEPT ![t] = @ + 1]
    /\ UNCHANGED <<content, stored, dataLabel, held, under, rd, reads, writes,
                   failures>>

Outcomes == {"ok", "cutoff", "refusal", "rejected", "down", "lost"}

Next ==
    \/ \E f \in Files : PersonWrites(f)
    \/ \E t \in Tasks :
         \/ \E f \in Files : ReadPreview(t, f) \/ Write(t, f)
         \/ ReadInvoke(t) \/ ReadReturn(t)
         \/ Issue(t) \/ Route(t) \/ Answer(t)
         \/ \E o \in Outcomes : Attempt(t, o)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* What the OS promises.                                                   *)

TypeOK ==
    /\ \A f \in Files : content[f] \subseteq Values /\ stored[f] \in 0..2
    /\ \A t \in Tasks : dataLabel[t] \in 0..2 /\ Range(held[t]) \subseteq Values
    /\ \A t \in Tasks : mc[t].phase \in {"idle", "issued", "sending", "ok", "cutoff"}

\* DP-2: every value a call carries goes only to a model approved for the
\* label it was read under. A replay goes back only to the provider that
\* wrote it, and holds only what the conversation held then.
DP2_OnlyToModelsApprovedForWhatWasRead ==
    \A t \in Tasks : mc[t].phase = "sending" =>
        \A i \in 1..Len(mc[t].hist) : mc[t].under[i] <= Approved(mc[t].order[mc[t].at])

\* DP-2 and DP-11 by where each value came from: what a task writes
\* carries the label of what it had read, so a summary of a confidential
\* file, written into an unlabeled one, goes only where confidential data
\* may (ADR 0052).
DP11_OnlyToModelsApprovedForItsOrigin ==
    \A t \in Tasks : mc[t].phase = "sending" =>
        \A v \in Range(mc[t].hist) : Origin(v) <= Approved(mc[t].order[mc[t].at])

\* DP-10: a label stored with a file stays with it, whoever writes it.
DP10_ALabelStaysWithItsFile ==
    [][\A f \in Files : LabelOf(f)' >= LabelOf(f)]_vars

\* DP-3: a placeholder comes back only as a value its own call sent, so
\* nothing crosses to another task or person; a replay's placeholders mean
\* in the call that carries it what they meant when it was written.
DP3_PlaceholdersComeBackOnlyToTheirCall ==
    \A t \in Tasks :
        /\ mc[t].restored \subseteq Range(mc[t].hist)
        /\ \A v \in mc[t].restored : ValueOwner(v) = Owner(t)
DP3_AReplayMeansWhatItSaid ==
    \A t \in Tasks : mc[t].phase # "idle" => IsPrefix(mc[t].replay.hist, mc[t].hist)

\* MP-8, ADR 0044: a call only reaches models policy approves for its label,
\* however the agent orders them.
MP8_OnlyApprovedModels ==
    \A t \in Tasks : mc[t].phase = "sending" =>
        /\ mc[t].order[mc[t].at] \in Routes(mc[t].label)
        /\ Approved(mc[t].order[mc[t].at]) >= mc[t].label

\* ADR 0044: one step up, never down while a step up is there: a model no
\* dearer than `above` is tried only once every dearer one has failed.
ADR0044_NeverDownWhileAStepUpIsThere ==
    \A t \in Tasks : mc[t].phase = "sending" /\ mc[t].above # None =>
        LET c == mc[t]
            m == c.order[c.at]
        IN Price(m) <= Price(c.above) =>
             \A x \in c.route : Price(x) > Price(c.above) =>
                 \E i \in 1..(c.at - 1) : c.order[i] = x

\* Not promised, and not in the .cfg: the ladder only climbs. ADR 0044 steps
\* up to the cheapest dearer than the model that last answered, so after
\* the models above a task's step fail and a cheaper one answers, the next
\* step up starts from that one (opus and sonnet down, haiku cut off: from
\* above sonnet to above haiku). The task loses at most the climb it had.
ADR0044_TheLadderOnlyClimbs ==
    [][\A t \in Tasks :
         (ladder[t].above # None /\ ladder'[t].above # ladder[t].above)
            => Price(ladder'[t].above) > Price(ladder[t].above)]_vars

\* Billing: an attempt is billed only when the provider took it, so a call
\* is billed twice only when a reply was lost on the way (MP-8 fails over
\* then, as for any outage).
BilledOnceUnlessAReplyWasLost ==
    \A t \in Tasks : mc[t].charged <= 1 + mc[t].lost
=============================================================================
