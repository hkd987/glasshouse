---------------------------- MODULE AuditSegments ----------------------------
(***************************************************************************)
(* The audit log kept as several files (AU-1, AU-2, AU-3; ADR 0053, audit  *)
(* 4.7): the file being written is closed as a segment and a new one       *)
(* begun, old segments are removed under a retention rule with a record of *)
(* what went, glassd stops at any step and starts again, and `verify`      *)
(* reads beside the writer.                                                *)
(*                                                                         *)
(* AuditLog.tla is the writing of records into the one file: runs, a full  *)
(* disk, cutting back, the calls that wait. Here a record is written whole *)
(* or not at all, and what is modelled is which files hold which records.  *)
(*                                                                         *)
(* The code:                                                               *)
(*   crates/lib/audit/src/log.rs                                           *)
(*     write (Write), due and seal (SealRename: the file renamed to its    *)
(*     segment's name; SealCreate: an empty file in its place; both under  *)
(*     the writer's lock, between two runs), open_writer and chain_before  *)
(*     (Start: where the chain stands is read from the file, or from the   *)
(*     newest segment when the file is empty or missing),                  *)
(*     trim (TrimNote: the log_trimmed record first, saying through which  *)
(*     record; TrimDelete: the segments then, one at a time, oldest        *)
(*     first), open_files (VOpen: the file being written; VList: the       *)
(*     segments, listed and opened; VAgain: the file being written again), *)
(*     for_each_record and verify (VWalk: the chain through what was       *)
(*     opened, the oldest record kept checked against the log_trimmed      *)
(*     records; looked at again if the files changed under it).            *)
(*   crates/svc/runtime/src/runtime.rs  trim_audit (which segments are     *)
(*     past the org's rule: Age marks them, oldest first).                 *)
(*                                                                         *)
(* A record is its seq, and for a log_trimmed record how many records it   *)
(* says are gone (`covers`: through_seq + 1). A position in `recs` is a    *)
(* record ever written; a file is the positions it holds, and is known by  *)
(* its first, as a file on disk is known by what it is whatever it's       *)
(* called.                                                                 *)
(*                                                                         *)
(* Old holds ways it could be wrong:                                       *)
(*   "genesisAfterSeal"  a start on an empty file began the chain again    *)
(*                       from the first record, as it did when the log was *)
(*                       one file                                          *)
(*   "deleteFirst"       segments removed before the record of it is       *)
(*                       written                                           *)
(*   "exactAnchor"       verify asked that the oldest record kept be       *)
(*                       exactly the one after what a log_trimmed record   *)
(*                       names: wrong while segments are going one by one, *)
(*                       and for good after glassd stops part way          *)
(*   "noSecondLook"      verify reported a break found while the files     *)
(*                       changed under it                                  *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    MaxRecs,        \* records written
    MaxCrashes,     \* glassd stopping without warning
    MaxPasses,      \* times verify runs
    Old

VARIABLES
    recs,       \* every record ever written, in order: [seq, covers]
    segs,       \* the segments on disk, oldest first: [first, last] positions
    file,       \* the file being written: there or not, and its first position
    ever,       \* every segment ever closed (ghost): what an open file reads to
    aged,       \* firsts of the segments past the retention rule
    young,      \* a segment was removed that wasn't past the rule (ghost)
    up,         \* glassd runs
    wseq,       \* the writer's next seq
    sealing,    \* between the rename and the new file
    going,      \* a removal under way: the segments still to go
    vfy,        \* verify: its step, what it opened, the files as it began
    passes, crashes

vars == <<recs, segs, file, ever, aged, young, up, wseq, sealing, going, vfy,
          passes, crashes>>

Rec(seq, covers) == [seq |-> seq, covers |-> covers]
Seg(first, last) == [first |-> first, last |-> last]
NoVfy == [pc |-> "idle", opened |-> {}, shape |-> <<{}, 1>>, verdict |-> "none"]

Range(s) == {s[i] : i \in DOMAIN s}
\* The positions the file holds.
InFile == IF file.there THEN file.first..Len(recs) ELSE {}
\* The positions on disk.
OnDisk == InFile \cup UNION {s.first..s.last : s \in Range(segs)}
\* What the files are, to tell whether they changed: the segments' names,
\* and which file is being written.
Shape == <<{s.first : s \in Range(segs)}, IF file.there THEN file.first ELSE 0>>

Init ==
    /\ recs = <<>> /\ segs = <<>>
    /\ file = [there |-> TRUE, first |-> 1]
    /\ ever = {} /\ aged = {} /\ young = FALSE
    /\ up = TRUE /\ wseq = 0 /\ sealing = FALSE /\ going = <<>>
    /\ vfy = NoVfy /\ passes = 0 /\ crashes = 0

(* A record, onto the file.                                                *)
Write ==
    /\ up /\ ~sealing /\ file.there /\ Len(recs) < MaxRecs
    /\ recs' = Append(recs, Rec(wseq, 0))
    /\ wseq' = wseq + 1
    /\ UNCHANGED <<segs, file, ever, aged, young, up, sealing, going, vfy,
                   passes, crashes>>

(* The file, holding something, is closed as a segment: renamed.           *)
SealRename ==
    /\ up /\ ~sealing /\ file.there /\ file.first <= Len(recs)
    /\ segs' = Append(segs, Seg(file.first, Len(recs)))
    /\ ever' = ever \cup {Seg(file.first, Len(recs))}
    /\ file' = [file EXCEPT !.there = FALSE]
    /\ sealing' = TRUE
    /\ UNCHANGED <<recs, aged, young, up, wseq, going, vfy, passes, crashes>>

(* And an empty file takes its place.                                      *)
SealCreate ==
    /\ up /\ sealing
    /\ file' = [there |-> TRUE, first |-> Len(recs) + 1]
    /\ sealing' = FALSE
    /\ UNCHANGED <<recs, segs, ever, aged, young, up, wseq, going, vfy,
                   passes, crashes>>

(* Time: the oldest segment not yet past the retention rule now is.        *)
Age ==
    /\ \E i \in DOMAIN segs :
        /\ segs[i].first \notin aged
        /\ \A j \in 1..(i - 1) : segs[j].first \in aged
        /\ aged' = aged \cup {segs[i].first}
    /\ UNCHANGED <<recs, segs, file, ever, young, up, wseq, sealing, going,
                   vfy, passes, crashes>>

\* The segments past the rule, oldest first, with none before them kept.
PastRule ==
    LET n == Cardinality({i \in DOMAIN segs :
                            \A j \in 1..i : segs[j].first \in aged})
    IN SubSeq(segs, 1, n)

(* trim: the record of the removal first, on disk, then the segments.      *)
TrimNote ==
    /\ up /\ ~sealing /\ file.there /\ going = <<>>
    /\ PastRule # <<>>
    /\ Len(recs) < MaxRecs
    /\ recs' = Append(recs, Rec(wseq, PastRule[Len(PastRule)].last))
    /\ wseq' = wseq + 1
    /\ going' = PastRule
    /\ UNCHANGED <<segs, file, ever, aged, young, up, sealing, vfy, passes,
                   crashes>>

(* Old: the segments first, the record after.                              *)
TrimUnnoted ==
    /\ "deleteFirst" \in Old
    /\ up /\ ~sealing /\ file.there /\ going = <<>>
    /\ PastRule # <<>>
    /\ going' = PastRule
    /\ UNCHANGED <<recs, segs, file, ever, aged, young, up, wseq, sealing,
                   vfy, passes, crashes>>

TrimDelete ==
    /\ up /\ going # <<>>
    /\ segs' = SelectSeq(segs, LAMBDA s : s # Head(going))
    /\ young' = (young \/ Head(going).first \notin aged)
    /\ going' = Tail(going)
    /\ UNCHANGED <<recs, file, ever, aged, up, wseq, sealing, vfy, passes,
                   crashes>>

(* glassd stops: what it was in the middle of stays as it is on disk.      *)
Crash ==
    /\ up /\ crashes < MaxCrashes
    /\ up' = FALSE /\ sealing' = FALSE /\ going' = <<>>
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<recs, segs, file, ever, aged, young, wseq, vfy, passes>>

\* Where the chain stands, read from the disk: after the file's last
\* record, or the newest segment's when the file holds nothing.
ChainStands ==
    IF file.there /\ file.first <= Len(recs)
       THEN recs[Len(recs)].seq + 1
       ELSE IF segs # <<>> /\ "genesisAfterSeal" \notin Old
               THEN recs[segs[Len(segs)].last].seq + 1
               ELSE 0

(* open_writer: the file made if it isn't there, the chain taken up.       *)
Start ==
    /\ ~up
    /\ up' = TRUE
    /\ file' = IF file.there THEN file ELSE [there |-> TRUE, first |-> Len(recs) + 1]
    /\ wseq' = ChainStands
    /\ UNCHANGED <<recs, segs, ever, aged, young, sealing, going, vfy, passes,
                   crashes>>

(* verify, beside all that. It opens the file being written, lists and     *)
(* opens the segments, and opens the file being written again.             *)
VOpen ==
    /\ vfy.pc = "idle" /\ passes < MaxPasses
    /\ passes' = passes + 1
    /\ vfy' = [pc |-> "list",
               opened |-> IF file.there THEN {file.first} ELSE {},
               shape |-> Shape, verdict |-> "none"]
    /\ UNCHANGED <<recs, segs, file, ever, aged, young, up, wseq, sealing,
                   going, crashes>>

VList ==
    /\ vfy.pc = "list"
    /\ vfy' = [vfy EXCEPT !.pc = "again",
                          !.opened = @ \cup {s.first : s \in Range(segs)}]
    /\ UNCHANGED <<recs, segs, file, ever, aged, young, up, wseq, sealing,
                   going, passes, crashes>>

VAgain ==
    /\ vfy.pc = "again"
    /\ vfy' = [vfy EXCEPT !.pc = "walk",
                          !.opened = @ \cup (IF file.there THEN {file.first} ELSE {})]
    /\ UNCHANGED <<recs, segs, file, ever, aged, young, up, wseq, sealing,
                   going, passes, crashes>>

\* What an open file reads to, by its first position: a segment's last, or
\* everything so far for the file still being written.
ReadsTo(f) ==
    IF \E s \in ever : s.first = f
       THEN (CHOOSE s \in ever : s.first = f).last
       ELSE Len(recs)
Read(opened) == UNION {f..ReadsTo(f) : f \in opened}

\* A set of positions checks out: it is one stretch with nothing missing,
\* every record's seq follows the one before, and its oldest record is the
\* log's first or one a log_trimmed record in it covers.
Covered(ps, a) ==
    \E i \in ps :
        IF "exactAnchor" \in Old
           THEN recs[i].covers = a - 1
           ELSE recs[i].covers >= a - 1
ChecksOut(ps) ==
    \/ ps = {}
    \/ LET a == CHOOSE i \in ps : \A j \in ps : i <= j
           b == CHOOSE i \in ps : \A j \in ps : i >= j
       IN /\ ps = a..b
          /\ \A i \in ps : i = a \/ recs[i].seq = recs[i - 1].seq + 1
          /\ (recs[a].seq = 0 /\ a = 1) \/ (recs[a].seq > 0 /\ Covered(ps, a))

VWalk ==
    /\ vfy.pc = "walk"
    /\ LET ok == ChecksOut(Read(vfy.opened)) IN
        vfy' = IF ok THEN [vfy EXCEPT !.pc = "idle", !.verdict = "intact"]
               \* A break, and the files changed under the pass: it looks
               \* again, and says nothing yet.
               ELSE IF Shape # vfy.shape /\ "noSecondLook" \notin Old
                       THEN [vfy EXCEPT !.pc = "idle"]
                       ELSE [vfy EXCEPT !.pc = "idle", !.verdict = "corrupt"]
    /\ UNCHANGED <<recs, segs, file, ever, aged, young, up, wseq, sealing,
                   going, passes, crashes>>

Next ==
    \/ Write \/ SealRename \/ SealCreate \/ Age
    \/ TrimNote \/ TrimUnnoted \/ TrimDelete
    \/ Crash \/ Start
    \/ VOpen \/ VList \/ VAgain \/ VWalk

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ recs \in Seq([seq : 0..MaxRecs, covers : 0..MaxRecs])
    /\ Len(recs) <= MaxRecs
    /\ \A i \in DOMAIN segs : segs[i] \in [first : 1..MaxRecs, last : 1..MaxRecs]
    /\ file \in [there : BOOLEAN, first : 1..(MaxRecs + 1)]
    /\ young \in BOOLEAN /\ up \in BOOLEAN /\ sealing \in BOOLEAN
    /\ wseq \in 0..MaxRecs
    /\ vfy.pc \in {"idle", "list", "again", "walk"}
    /\ vfy.verdict \in {"none", "intact", "corrupt"}
    /\ passes \in 0..MaxPasses /\ crashes \in 0..MaxCrashes

(* What the OS promises.                                                   *)

\* AU-2: the log on disk checks out as it is, at every moment: mid-way
\* through closing a segment, through a removal, and however glassd
\* stopped. Every record's seq follows the one before it, across files, and
\* records are gone only where a record in the log says the rule removed
\* them.
AU2_TheLogOnDiskAlwaysVerifies == ChecksOut(OnDisk)

\* AU-3: only records past the retention rule are removed, and never from
\* the file being written.
AU3_OnlyRecordsPastTheRuleGo == ~young

\* AU-2: verify never reports a break in a log that has none.
AU2_VerifyReportsNoBreakThatIsntThere == vfy.verdict # "corrupt"

=============================================================================
