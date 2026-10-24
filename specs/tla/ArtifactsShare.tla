---------------------------- MODULE ArtifactsShare ----------------------------
(***************************************************************************)
(* A shared artifact's versions, comments and resolves while its owner's   *)
(* app and an agent update it at once and a colleague comments (ADR 0033;  *)
(* AR-3, AR-4).                                                            *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/apps/artifacts/src/lib.rs  update (the newest version's number *)
(*        read, then the next one posted: add_version), comment (marked    *)
(*        with the newest version its author sees), resolve,               *)
(*        Artifact::from_item (every read folds the whole log: a number's  *)
(*        first version in the log is the one shown, later ones dropped;   *)
(*        a comment's last resolve stands)                                 *)
(*   crates/apps/chat/src/{lib,store}.rs  each computer's log: its own     *)
(*        posts kept at once, everyone's by sync                           *)
(*                                                                         *)
(* Computer a is the owner's, where the Artifacts app and an agent both    *)
(* update; computer p is a colleague's, who comments; either resolves or   *)
(* reopens. Version 1 is the share.                                        *)
(*                                                                         *)
(* Old is the set of the code's behaviours before ADR 0052 that a          *)
(* configuration keeps; {} is the code now.                                *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Callers,      \* who updates on computer a
    MaxUpdates,   \* updates, all told
    MaxComments,  \* comments, by p
    MaxResolves,  \* resolves and reopens, by anyone
    Old           \* "noTurn": two updates on one computer could both read
                  \*   the newest number before either posted

Computers == {"a", "p"}

VARIABLES
    log,      \* the console's messages, by sequence number
    have,     \* the sequence numbers each computer keeps
    upd,      \* each caller's update: [pc, num]
    turn,     \* the caller whose update holds a's turn, or "none"
    ups, coms, ress

vars == <<log, have, upd, turn, ups, coms, ress>>

Max(S) == CHOOSE x \in S : \A y \in S : y <= x
Min(S) == CHOOSE x \in S : \A y \in S : x <= y
All == 1..Len(log)
Rec(kind, by, num, val) == [kind |-> kind, by |-> by, num |-> num, val |-> val]

\* from_item over the messages P: who posted the version shown as n.
ShownAs(P, n) ==
    LET V == {s \in P : log[s].kind = "ver" /\ log[s].num = n}
    IN IF V = {} THEN "none" ELSE log[Min(V)].by
Latest(P) == Max({log[s].num : s \in {t \in P : log[t].kind = "ver"}})
\* A comment's resolved state: its last resolve, in log order.
Resolved(P, cid) ==
    LET R == {s \in P : log[s].kind = "res" /\ log[s].num = cid}
    IN R # {} /\ log[Max(R)].val

Init ==
    /\ log = << Rec("ver", "share", 1, FALSE) >>
    /\ have = [c \in Computers |-> {1}]
    /\ upd = [k \in Callers |-> [pc |-> "idle", num |-> 0]]
    /\ turn = "none"
    /\ ups = 0 /\ coms = 0 /\ ress = 0

\* send_body: the console numbers it, its computer keeps it.
Post(c, r) ==
    /\ log' = Append(log, r)
    /\ have' = [have EXCEPT ![c] = @ \cup {Len(log) + 1}]

Sync(c) ==
    /\ have[c] # All
    /\ have' = [have EXCEPT ![c] = All]
    /\ UNCHANGED <<log, upd, turn, ups, coms, ress>>

\* update: its turn (since ADR 0052), then the artifact read and the next
\* number taken.
Read(k) ==
    /\ upd[k].pc = "idle" /\ ups < MaxUpdates
    /\ "noTurn" \in Old \/ turn = "none"
    /\ turn' = IF "noTurn" \in Old THEN turn ELSE k
    /\ upd' = [upd EXCEPT ![k] = [pc |-> "read", num |-> Latest(have["a"]) + 1]]
    /\ ups' = ups + 1
    /\ UNCHANGED <<log, have, coms, ress>>

\* add_version: the version posted; the call says "is at version num".
Add(k) ==
    /\ upd[k].pc = "read"
    /\ Post("a", Rec("ver", k, upd[k].num, FALSE))
    /\ upd' = [upd EXCEPT ![k].pc = "done"]
    /\ turn' = IF turn = k THEN "none" ELSE turn
    /\ UNCHANGED <<ups, coms, ress>>

\* comment: on the newest version p's computer has.
Comment ==
    /\ coms < MaxComments
    /\ Post("p", Rec("com", "p", Latest(have["p"]), FALSE))
    /\ coms' = coms + 1
    /\ UNCHANGED <<upd, turn, ups, ress>>

\* resolve or reopen a comment this computer has.
Resolve(c) ==
    /\ ress < MaxResolves
    /\ \E s \in have[c] : log[s].kind = "com" /\ \E v \in BOOLEAN :
          Post(c, Rec("res", c, s, v))
    /\ ress' = ress + 1
    /\ UNCHANGED <<upd, turn, ups, coms>>

Next ==
    \/ \E k \in Callers : Read(k) \/ Add(k)
    \/ Comment
    \/ \E c \in Computers : Resolve(c) \/ Sync(c)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* AR-4: an update's call says the version it made, and that number shows
\* what it posted, on every computer once it has the log.
AR4_EachUpdateIsTheVersionItSays ==
    \A k \in Callers : upd[k].pc = "done" => ShownAs(All, upd[k].num) = k

\* AR-4: versions count up from the share, one at a time.
AR4_VersionsCountUp ==
    \A n \in 1..Latest(All) : ShownAs(All, n) # "none"

\* AR-3: a comment is marked with a version everyone is shown.
AR3_ACommentIsOnAVersionThatIsShown ==
    \A s \in All : log[s].kind = "com" => ShownAs(All, log[s].num) # "none"

\* AR-3: computers with the same messages show each comment resolved or
\* open alike: the last resolve in the log, whoever's computer posted it.
AR3_EveryoneSeesTheSameResolved ==
    \A s \in All : log[s].kind = "com" =>
        \A c, d \in Computers : have[c] = All /\ have[d] = All =>
            Resolved(have[c], s) = Resolved(have[d], s)

=============================================================================
