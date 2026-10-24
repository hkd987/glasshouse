------------------------- MODULE HardwarePatchStack -------------------------
(***************************************************************************)
(* Kernel patches kept one on top of another (ADR 0057): what the computer *)
(* runs is what its record says it runs, and keeping one patch never      *)
(* quietly undoes another.                                                 *)
(*                                                                         *)
(* A patch rewrites files in /boot/kernel: modules, or the kernel itself.  *)
(* Its build may carry other patches' changes too, when the build service  *)
(* built it on top of them (`includes` in its manifest). Each patch is     *)
(* tried in an environment copied from the running one (HardwarePatch.tla *)
(* models the trial; here a patch is kept at once), and taking a patch out *)
(* starts the environment it was made from again.                         *)
(*                                                                         *)
(* The code: crates/svc/runtime/src/hwpatch.rs `apply` (`in_effect`,       *)
(* `stacking`), `undo`. Init chooses what each patch rewrites and what it  *)
(* was built on, so TLC checks every combination.                         *)
(*                                                                         *)
(* Old switches:                                                           *)
(*  - "stackBlind": no check of the patches already kept, as slice 1's     *)
(*    first code: a second kernel patch replaces the kernel and the first  *)
(*    patch's fix with it, while the record says both are kept.            *)
(*  - "includesUnchecked": a patch built on top of one this computer       *)
(*    doesn't keep brings that one's code in, off the record.              *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS Patches, Files, MaxEnvs, Old

EnvIds == 0..MaxEnvs

VARIABLES
    filesOf,    \* what each patch rewrites
    includes,   \* the patches each was built on top of
    made,       \* environments made so far: 0 is the computer's own
    parent,     \* the environment each was copied from
    patchOf,    \* the patch kept in each ("none" in 0)
    content,    \* each environment's files: the patch whose build each is, or "base"
    running

vars == <<filesOf, includes, made, parent, patchOf, content, running>>

TypeOK ==
    /\ filesOf \in [Patches -> SUBSET Files]
    /\ includes \in [Patches -> SUBSET Patches]
    /\ made \in 1..(MaxEnvs + 1)
    /\ parent \in [EnvIds -> EnvIds]
    /\ patchOf \in [EnvIds -> Patches \cup {"none"}]
    /\ content \in [EnvIds -> [Files -> Patches \cup {"base"}]]
    /\ running \in EnvIds

Init ==
    /\ filesOf \in [Patches -> (SUBSET Files) \ {{}}]
    /\ includes \in {i \in [Patches -> SUBSET Patches] : \A p \in Patches : p \notin i[p]}
    /\ made = 1
    /\ parent = [e \in EnvIds |-> 0]
    /\ patchOf = [e \in EnvIds |-> "none"]
    /\ content = [e \in EnvIds |-> [f \in Files |-> "base"]]
    /\ running = 0

\* The patches the record says the environment keeps: its own and those of
\* the environments it was copied from (glassd's history, `in_effect`).
RECURSIVE InEffect(_)
InEffect(e) == IF e = 0 THEN {} ELSE {patchOf[e]} \cup InEffect(parent[e])

\* r's build carries q's change.
Carries(r, q) == r = q \/ q \in includes[r]

\* `apply`'s check: a patch rewriting a file a kept patch rewrote must have
\* been built on top of it, and one built on top of others needs them kept
\* here. A newer revision of a kept patch replaces it.
Stacks(p) ==
    /\ \/ "stackBlind" \in Old
       \/ \A q \in InEffect(running) \ {p} :
             filesOf[q] \cap filesOf[p] = {} \/ q \in includes[p]
    /\ "includesUnchecked" \in Old \/ includes[p] \subseteq InEffect(running)

\* A patch kept, in an environment copied from the running one.
Keep(p) ==
    /\ made <= MaxEnvs /\ Stacks(p)
    /\ parent' = [parent EXCEPT ![made] = running]
    /\ patchOf' = [patchOf EXCEPT ![made] = p]
    /\ content' = [content EXCEPT ![made] =
                     [f \in Files |-> IF f \in filesOf[p] THEN p ELSE content[running][f]]]
    /\ running' = made
    /\ made' = made + 1
    /\ UNCHANGED <<filesOf, includes>>

\* The latest patch taken out: the environment it was made from again.
Undo ==
    /\ running # 0
    /\ running' = parent[running]
    /\ UNCHANGED <<filesOf, includes, made, parent, patchOf, content>>

Next == (\E p \in Patches : Keep(p)) \/ Undo

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* Every patch the record says is kept is in what runs: each file it
\* rewrote is its build, or the build of one built on top of it.
ADR0057_NoKeptFixLost ==
    \A q \in InEffect(running) : \A f \in filesOf[q] :
        content[running][f] # "base" /\ Carries(content[running][f], q)

\* Nothing runs that the record doesn't say is kept: every rewritten file
\* is a kept patch's build, and so is every patch that build carries.
ADR0057_NothingRunsOffTheRecord ==
    \A f \in Files :
        content[running][f] # "base" =>
            /\ content[running][f] \in InEffect(running)
            /\ includes[content[running][f]] \subseteq InEffect(running)

=============================================================================
