----------------------------- MODULE ChunkStore -----------------------------
(***************************************************************************)
(* Sleeping computers' chunks in the store (ADR 0065 section 11, ADR 0066; *)
(* audit 5.3): computers save versions that share chunks, old versions are *)
(* trimmed, and GC deletes the chunks no version names, while saves run.   *)
(* A version must never name a chunk that is gone.                         *)
(*                                                                         *)
(* The code:                                                               *)
(*   crates/svc/computers/src/store.rs                                     *)
(*     upload (SaveBegin: the chunks the computer's last version names     *)
(*     aren't looked at), Sending::send (SaveChunk: any other chunk is     *)
(*     relied on only if the store has it and it is young, REUSE_YOUNGER;  *)
(*     otherwise it is sent, which makes it a new object), commit          *)
(*     (SaveCommit: the version written last, within SAVE_WITHIN, or the   *)
(*     save gives up: SaveGivesUp), gc (GcBegin: the time is noted, then   *)
(*     the chunks any version names are read; GcList: the others that were *)
(*     older than GRACE at that time, each with what tells it from a later *)
(*     writing; GcDelete: each deleted only if it is still that object).   *)
(*   crates/svc/computers/src/store/blobs.rs  delete_listed (S3's          *)
(*     If-Match on the entity tag; a folder looks again).                  *)
(*   crates/svc/computers/src/sandbox.rs  sleep (Trim: versions past the   *)
(*     ones kept are removed after a save).                                *)
(*                                                                         *)
(* Time is one fact per chunk: young (written within REUSE_YOUNGER) or old *)
(* (older than GRACE, so GC may take it). GRACE is a day and a save has an *)
(* hour, so a chunk a save sent or found young can't become old while that *)
(* save is still going: Age waits for it to commit or give up.             *)
(*                                                                         *)
(* A computer keeps one version here (three in the code): trimming is what *)
(* leaves chunks no version names.                                         *)
(*                                                                         *)
(* Old holds the code before, and a way this could be wrong:               *)
(*   "reuseAny"     a save relied on any chunk the store had, however old: *)
(*                  one no version named, which GC then collected (why GC  *)
(*                  had no caller, and every sleep left chunks for good)   *)
(*   "blindDelete"  GC deleted what it had listed without checking it was  *)
(*                  still that object: a chunk a save sent again in        *)
(*                  between went with it                                   *)
(*   "ageAtListing" GC judged a chunk's age when it listed the chunks,     *)
(*                  after reading the versions: a pass slow enough found a *)
(*                  chunk old that a version written meanwhile named (as   *)
(*                  the code was; this model found it)                     *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Computers,      \* each saves versions of itself
    Chunks,
    MaxPuts,        \* times a chunk is written (a bound on the model)
    Old

VARIABLES
    chunk,      \* each chunk in the store: "none", "young" or "old"
    gen,        \* how many times each was written: tells one writing from the next
    version,    \* each computer's version in the store: the chunks it names, or {}
    pc,         \* each computer's save: "idle" or "saving"
    want,       \* the chunks the version being saved will name
    have,       \* those of them it has sent, or found young
    gc,         \* GC: "idle", "listing" or "deleting"
    refs,       \* the chunks versions named when GC looked
    aged,       \* the writings of chunks that were old when GC began
    listed      \* the chunks GC means to delete, each with the writing it listed

vars == <<chunk, gen, version, pc, want, have, gc, refs, aged, listed>>

Named == UNION {version[k] : k \in Computers}

Init ==
    /\ chunk = [c \in Chunks |-> "none"]
    /\ gen = [c \in Chunks |-> 0]
    /\ version = [k \in Computers |-> {}]
    /\ pc = [k \in Computers |-> "idle"]
    /\ want = [k \in Computers |-> {}]
    /\ have = [k \in Computers |-> {}]
    /\ gc = "idle" /\ refs = {} /\ aged = {} /\ listed = {}

(* A computer goes to sleep: its folder is these chunks now.               *)
SaveBegin(k, cs) ==
    /\ pc[k] = "idle" /\ cs # {}
    /\ pc' = [pc EXCEPT ![k] = "saving"]
    /\ want' = [want EXCEPT ![k] = cs]
    \* What its last version names is in the store, and stays named.
    /\ have' = [have EXCEPT ![k] = cs \cap version[k]]
    /\ UNCHANGED <<chunk, gen, version, gc, refs, aged, listed>>

(* One chunk of it: relied on if it's there and young, sent otherwise.     *)
SaveChunk(k, c) ==
    /\ pc[k] = "saving" /\ c \in want[k] \ have[k]
    /\ IF chunk[c] = "young" \/ ("reuseAny" \in Old /\ chunk[c] # "none")
          THEN UNCHANGED <<chunk, gen>>
          ELSE /\ gen[c] < MaxPuts
               /\ chunk' = [chunk EXCEPT ![c] = "young"]
               /\ gen' = [gen EXCEPT ![c] = @ + 1]
    /\ have' = [have EXCEPT ![k] = @ \cup {c}]
    /\ UNCHANGED <<version, pc, want, gc, refs, aged, listed>>

(* Its version, written last; the version before it is trimmed.            *)
SaveCommit(k) ==
    /\ pc[k] = "saving" /\ have[k] = want[k]
    /\ version' = [version EXCEPT ![k] = want[k]]
    /\ pc' = [pc EXCEPT ![k] = "idle"]
    /\ want' = [want EXCEPT ![k] = {}]
    /\ have' = [have EXCEPT ![k] = {}]
    /\ UNCHANGED <<chunk, gen, gc, refs, aged, listed>>

(* A save that fails, or runs past the time a save has: no version.        *)
SaveGivesUp(k) ==
    /\ pc[k] = "saving"
    /\ pc' = [pc EXCEPT ![k] = "idle"]
    /\ want' = [want EXCEPT ![k] = {}]
    /\ have' = [have EXCEPT ![k] = {}]
    /\ UNCHANGED <<chunk, gen, version, gc, refs, aged, listed>>

(* A day passes for a chunk. Not while a save that sent it, or found it    *)
(* young, is still going: a save has an hour.                              *)
Age(c) ==
    /\ chunk[c] = "young"
    /\ \A k \in Computers : pc[k] = "saving" => c \notin (have[k] \ version[k])
    /\ chunk' = [chunk EXCEPT ![c] = "old"]
    /\ UNCHANGED <<gen, version, pc, want, have, gc, refs, aged, listed>>

(* GC notes the time, and reads every version for the chunks they name.    *)
GcBegin ==
    /\ gc = "idle"
    /\ refs' = Named
    /\ aged' = {<<c, gen[c]>> : c \in {d \in Chunks : chunk[d] = "old"}}
    /\ gc' = "listing"
    /\ UNCHANGED <<chunk, gen, version, pc, want, have, listed>>

(* Then lists the chunks: those none named whose writing was a day old     *)
(* when it began (one written since is young to this pass, however long    *)
(* the pass takes), with which writing of each it saw.                     *)
GcList ==
    /\ gc = "listing"
    /\ listed' = {<<c, gen[c]>> :
                    c \in {d \in Chunks :
                            /\ chunk[d] # "none" /\ d \notin refs
                            /\ \/ <<d, gen[d]>> \in aged
                               \/ ("ageAtListing" \in Old /\ chunk[d] = "old")}}
    /\ gc' = "deleting"
    /\ UNCHANGED <<chunk, gen, version, pc, want, have, refs, aged>>

(* And deletes each, if it is still the object that was listed.            *)
GcDelete(c, g) ==
    /\ gc = "deleting" /\ <<c, g>> \in listed
    /\ listed' = listed \ {<<c, g>>}
    /\ IF gen[c] = g \/ "blindDelete" \in Old
          THEN chunk' = [chunk EXCEPT ![c] = "none"]
          ELSE UNCHANGED chunk
    /\ UNCHANGED <<gen, version, pc, want, have, gc, refs, aged>>

GcEnd ==
    /\ gc = "deleting" /\ listed = {}
    /\ gc' = "idle" /\ refs' = {} /\ aged' = {}
    /\ UNCHANGED <<chunk, gen, version, pc, want, have, listed>>

Next ==
    \/ \E k \in Computers :
        \/ \E cs \in SUBSET Chunks : SaveBegin(k, cs)
        \/ \E c \in Chunks : SaveChunk(k, c)
        \/ SaveCommit(k) \/ SaveGivesUp(k)
    \/ \E c \in Chunks : Age(c)
    \/ GcBegin \/ GcList \/ GcEnd
    \/ \E c \in Chunks : \E g \in 0..MaxPuts : GcDelete(c, g)

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ chunk \in [Chunks -> {"none", "young", "old"}]
    /\ gen \in [Chunks -> 0..MaxPuts]
    /\ version \in [Computers -> SUBSET Chunks]
    /\ pc \in [Computers -> {"idle", "saving"}]
    /\ want \in [Computers -> SUBSET Chunks]
    /\ have \in [Computers -> SUBSET Chunks]
    /\ gc \in {"idle", "listing", "deleting"}
    /\ refs \subseteq Chunks /\ aged \subseteq (Chunks \X (0..MaxPuts))
    /\ listed \subseteq (Chunks \X (0..MaxPuts))

(* What the OS promises.                                                   *)

\* A computer asleep can be woken: every chunk its version names is in the
\* store, whatever GC and other computers' saves did meanwhile.
EveryChunkAVersionNamesIsThere ==
    \A k \in Computers : \A c \in version[k] : chunk[c] # "none"

=============================================================================
