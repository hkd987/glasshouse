----------------------------- MODULE SkillMerge -----------------------------
(***************************************************************************)
(* A skill's versions and proposals across computers (ADR 0034 §6-7,      *)
(* SK-3, SK-4): its owner on two computers, and a colleague on a third,   *)
(* propose changes and merge them while each computer sees the skill's    *)
(* messages as far as it last synced.                                      *)
(*                                                                         *)
(* A skill is a shared item (ADR 0033): the console orders its messages   *)
(* (seq) and can't read them, so what a version is comes from how each    *)
(* computer reads the messages. A bundle is modelled as the set of        *)
(* proposals whose changes it holds; the first version holds none.        *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/apps/skills/src/lib.rs: Skill::from_item (Accepted: which    *)
(*    version messages count, read the same way on every computer),       *)
(*    SkillsProvider::propose (Propose), merge (Merge): each reads the     *)
(*    item from this computer's store, then posts one message.             *)
(*  - crates/apps/chat/src/lib.rs: post_sealed -> send_body keeps the      *)
(*    message posted, at its seq, before any sync; store.rs item_messages  *)
(*    reads an item's messages in seq order; sync_once brings the rest    *)
(*    (Sync).                                                              *)
(*                                                                         *)
(* Before this model, TLC found against the code: two merges on base vN   *)
(* at once (the owner's two computers, or two tasks) both posted vN+1;    *)
(* every computer kept the first and dropped the second, but the computer *)
(* that posted the second read it as vN+1 until it synced. A change       *)
(* proposed there on that vN+1 and merged became vN+2 everywhere: the     *)
(* first merge's change was lost, and the dropped one's came back unmerged *)
(* (ADR0034_MergeKeepsEveryChange). A version now counts only when it is  *)
(* the next number and the proposal it merges was made on the version     *)
(* before it (its base's SHA-256), not merged already; merge checks the   *)
(* same.                                                                   *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS
  Owners,     \* the owner's computers
  Others,     \* colleagues' computers
  Changes,    \* the proposals that may be made, each once
  MaxLog      \* messages in the item, at most

NONE == "none"
Computers == Owners \cup Others

V1 == [t |-> "v", n |-> 1, content |-> {}, merged |-> NONE]

VARIABLES
  log,      \* the item's messages, in the console's order
  synced,   \* computer -> how far it synced
  own,      \* computer -> the seqs it posted past what it synced
  reads,    \* computer -> the versions it counts, as it reads them now
  proposed  \* the changes proposed so far

vars == <<log, synced, own, reads, proposed>>

\* What computer c's store holds, in seq order.
View(c) ==
  LET ix == SelectSeq([i \in 1..Len(log) |-> i],
                      LAMBDA i : i <= synced[c] \/ i \in own[c])
  IN [k \in 1..Len(ix) |-> log[ix[k]]]

ProposalIn(ms, id) == {ms[i] : i \in {j \in 1..Len(ms) : ms[j].t = "p" /\ ms[j].id = id}}

MergedIn(acc, id) == \E i \in 1..Len(acc) : acc[i].merged = id

(* Skill::from_item: a version counts when it is the next number and,     *)
(* when it merges a proposal, that proposal changes the version before it *)
(* (number and SHA-256) and no version before merged it.                  *)
CountsAfter(ms, acc, m) ==
  /\ m.n = Len(acc) + 1
  /\ m.merged # NONE =>
       \A p \in ProposalIn(ms, m.merged) :
         /\ Len(acc) > 0
         /\ p.base = acc[Len(acc)].n
         /\ p.before = acc[Len(acc)].content
         /\ ~MergedIn(acc, m.merged)

RECURSIVE Walk(_, _, _)
Walk(ms, i, acc) ==
  IF i > Len(ms) THEN acc
  ELSE IF ms[i].t = "v" /\ CountsAfter(ms, acc, ms[i])
       THEN Walk(ms, i + 1, Append(acc, ms[i]))
       ELSE Walk(ms, i + 1, acc)

Accepted(ms) == Walk(ms, 1, <<>>)

Init ==
  /\ log = <<V1>>
  /\ synced = [c \in Computers |-> 1]
  /\ own = [c \in Computers |-> {}]
  /\ reads = [c \in Computers |-> <<V1>>]
  /\ proposed = {}

(* post_sealed: the console orders it last; this computer keeps it at     *)
(* once, and reads it after everything else it has (its seq is highest).  *)
Post(c, m) ==
  /\ log' = Append(log, m)
  /\ own' = [own EXCEPT ![c] = own[c] \cup {Len(log) + 1}]
  /\ reads' = [reads EXCEPT ![c] =
       IF m.t = "v" /\ CountsAfter(Append(View(c), m), reads[c], m)
       THEN Append(reads[c], m) ELSE reads[c]]

(* skill.propose: on the latest version this computer has, with the       *)
(* base's scores (their SHA-256 is the base's) and the draft's.           *)
Propose(c, x) ==
  /\ Len(log) < MaxLog
  /\ x \notin proposed
  /\ LET base == reads[c][Len(reads[c])] IN
     Post(c, [t |-> "p", id |-> x, base |-> base.n, before |-> base.content,
              content |-> base.content \cup {x}])
  /\ proposed' = proposed \cup {x}
  /\ UNCHANGED synced

(* skill.merge, by the owner: a proposal not merged yet, on the latest    *)
(* version this computer has, posted as the next one.                     *)
Merge(c, x) ==
  /\ Len(log) < MaxLog
  /\ LET acc == reads[c]
         latest == acc[Len(acc)]
     IN \E p \in ProposalIn(View(c), x) :
          /\ ~MergedIn(acc, x)
          /\ p.base = latest.n
          /\ p.before = latest.content
          /\ Post(c, [t |-> "v", n |-> latest.n + 1, content |-> p.content, merged |-> x])
  /\ UNCHANGED <<synced, proposed>>

Sync(c) ==
  /\ synced[c] < Len(log)
  /\ synced' = [synced EXCEPT ![c] = Len(log)]
  /\ own' = [own EXCEPT ![c] = {}]
  /\ reads' = [reads EXCEPT ![c] = Accepted(log)]
  /\ UNCHANGED <<log, proposed>>

Next ==
  \/ \E c \in Computers, x \in Changes : Propose(c, x)
  \/ \E c \in Owners, x \in Changes : Merge(c, x)
  \/ \E c \in Computers : Sync(c)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* ADR 0034 §7, SK-3: each version everyone reads holds the change of     *)
(* every proposal merged up to it and no other: a merge never loses a     *)
(* change, nor brings in one that wasn't merged.                          *)
ADR0034_MergeKeepsEveryChange ==
  LET acc == Accepted(log) IN
  \A i \in 1..Len(acc) :
    acc[i].content = {acc[j].merged : j \in {k \in 2..i : acc[k].merged # NONE}}

(* A proposal is merged into one version at most. *)
ADR0034_MergedOnce ==
  LET acc == Accepted(log) IN
  \A i, j \in 2..Len(acc) : i # j => acc[i].merged # acc[j].merged

(* Versions are numbered 1, 2, 3 ... with none missing, on every computer. *)
ADR0034_VersionsInOrder ==
  \A c \in Computers \cup {"all"} :
    LET acc == IF c = "all" THEN Accepted(log) ELSE reads[c] IN
    \A i \in 1..Len(acc) : acc[i].n = i

(* Once a computer has synced, it reads the skill as everyone does. *)
SameEverywhere ==
  \A c \in Computers : synced[c] = Len(log) => reads[c] = Accepted(log)

(* What a computer reads as it goes is what reading its whole store gives. *)
ReadsAsAWhole ==
  \A c \in Computers : reads[c] = Accepted(View(c))

Symmetry == Permutations(Changes) \cup Permutations(Owners)
=============================================================================
