------------------------------- MODULE OsUpdate -------------------------------
(***************************************************************************)
(* A signed OS update installed as a boot environment of its own (ADR     *)
(* 0061, EP-1, BS-4): through power cuts and crashes at any step, glassd  *)
(* restarting, an image that doesn't start, releases replayed, forged or  *)
(* signed by a key the root has since revoked, and a kernel patch the     *)
(* computer keeps (ADR 0057), which the new image doesn't carry unless   *)
(* the release fixes the same thing or brings a rebuild of it.           *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/svc/runtime/src/osupdate.rs `apply`: the release checked      *)
(*    (the root's key list, the channel's key, the OS key over the image, *)
(*    a number above the last one taken), the record ("staging") first,   *)
(*    then the image received into a new boot environment (`bectl         *)
(*    import`), the kept kernel patches' modules written into it, the      *)
(*    release's number kept as the last taken, the next start set to it   *)
(*    once (`bectl activate -t`), then the record "staged". BeginStage,   *)
(*    Import, CarryPatches, Take, ActivateOnce, MarkStaged. A staging that *)
(*    fails before Take can be tried again.                                *)
(*  - The loader starts the environment set to start once and clears that  *)
(*    as it does; otherwise the permanent one. Start.                      *)
(*  - `at_start`, as glassd starts: on the new environment, the image is   *)
(*    the release's (its version file), and it's made the permanent one    *)
(*    (`bectl activate`), then the record says kept (Confirm, Kept); on   *)
(*    the one before, the update rolled back. GlassdStart.                *)
(*  - `undo`: back to the environment before, permanent, then the record. *)
(*  - `cancel`: a staged update taken back before the restart.            *)
(*                                                                         *)
(* glassd's state, the record and what it last took, is on a dataset of   *)
(* its own outside the environments (post-install.sh), as ADR 0057 has   *)
(* it.                                                                     *)
(*                                                                         *)
(* Old switches, each a version that would be wrong:                      *)
(*  - "activateFirst": the start-once set before the image is all in.     *)
(*  - "permanent": `bectl activate` at once: an image that doesn't start  *)
(*    is then what every start uses.                                       *)
(*  - "replay": a release taken whatever its number, so an old one, or    *)
(*    the same one again, can be played back to undo a fix.               *)
(*  - "trustAnyKey": the channel key taken without the root's current     *)
(*    list, so a revoked key still signs updates.                          *)
(*  - "patchesIgnored": the new image staged without the kept kernel     *)
(*    patches: the fix the person relies on is gone after the update.     *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    MaxVersion,   \* release numbers 1..MaxVersion are offered
    MaxRestarts,
    Old

Envs == {"a", "b"}  \* the environment the computer ran from, the update's
Records == {"none", "staging", "staged", "kept", "failed", "rolledback",
            "cancelled", "undone"}
Ended == {"none", "kept", "failed", "rolledback", "cancelled", "undone"}
\* The release offered: its number, whether the channel key that signed it
\* is one the root lists now, whether the image is what the OS key signed,
\* whether the image starts, and what it does about the kept patch.
Offers == [n: 1..MaxVersion, keyListed: BOOLEAN, imageSigned: BOOLEAN,
           starts: BOOLEAN, patch: {"none", "fixed", "rebuilt"}]

VARIABLES
    running,     \* "off", or the environment the computer runs from
    started,     \* glassd's start check ran since glassd started
    closing,
    env,         \* the update's environment: "none", "partial", "complete"
    patched,     \* the kept patch's fix is in the update's environment
    once, perm,
    record,
    step,        \* "idle", "begun", "imported", "carried", "taken", "activated",
                 \* "undoing"
    offer,       \* the release being staged
    taken,       \* the highest release number taken
    patchKept,   \* the computer keeps a kernel patch (ADR 0057)
    restarts,
    ranUnsigned, \* the computer ran an image no trusted key signed
    lostFix,     \* the computer runs an update without the kept patch's fix
    replayed     \* a release was taken whose number wasn't above the last

vars == <<running, started, closing, env, patched, once, perm, record, step,
          offer, taken, patchKept, restarts, ranUnsigned, lostFix, replayed>>

TypeOK ==
    /\ running \in {"off"} \cup Envs
    /\ started \in BOOLEAN /\ closing \in BOOLEAN
    /\ env \in {"none", "partial", "complete"}
    /\ patched \in BOOLEAN /\ once \in BOOLEAN /\ perm \in Envs
    /\ record \in Records
    /\ step \in {"idle", "begun", "imported", "carried", "taken", "activated", "undoing"}
    /\ offer \in Offers
    /\ taken \in 0..MaxVersion
    /\ patchKept \in BOOLEAN
    /\ restarts \in 0..MaxRestarts
    /\ ranUnsigned \in BOOLEAN /\ lostFix \in BOOLEAN /\ replayed \in BOOLEAN

Init ==
    /\ running = "a" /\ started = TRUE /\ closing = FALSE
    /\ env = "none" /\ patched = FALSE /\ once = FALSE /\ perm = "a"
    /\ record = "none" /\ step = "idle"
    /\ offer = [n |-> 1, keyListed |-> TRUE, imageSigned |-> TRUE, starts |-> TRUE,
                patch |-> "none"]
    /\ taken = 0
    /\ patchKept \in BOOLEAN
    /\ restarts = 0
    /\ ranUnsigned = FALSE /\ lostFix = FALSE /\ replayed = FALSE

\* What `apply` checks before it changes anything.
Trusted(o) ==
    /\ o.keyListed \/ "trustAnyKey" \in Old
    /\ o.imageSigned
    /\ o.n > taken \/ "replay" \in Old
\* The kept patch's fix survives the update: the release fixes the same
\* thing, or brings a rebuild that `apply` writes in.
CarriesFix(o) == ~patchKept \/ o.patch \in {"fixed", "rebuilt"} \/ "patchesIgnored" \in Old

Down ==
    /\ running' = "off" /\ started' = FALSE /\ closing' = FALSE /\ step' = "idle"

-----------------------------------------------------------------------------
\* `apply`, step by step on the running environment, with no update open.
\* A release it doesn't trust, or that would drop the kept patch's fix,
\* is refused and changes nothing.
BeginStage ==
    /\ running = "a" /\ started /\ ~closing /\ step = "idle"
    /\ record \in Ended /\ perm = "a" /\ env = "none"
    /\ \E o \in Offers :
        /\ Trusted(o) /\ CarriesFix(o)
        /\ offer' = o
        /\ replayed' = (replayed \/ o.n <= taken)
    /\ record' = "staging" /\ step' = "begun" /\ patched' = FALSE
    /\ UNCHANGED <<running, started, closing, env, once, perm, taken, patchKept, restarts,
                   ranUnsigned, lostFix>>

Import ==
    /\ running = "a" /\ step = "begun"
    /\ env' = "complete" /\ step' = "imported"
    /\ UNCHANGED <<running, started, closing, patched, once, perm, record, offer,
                   taken, patchKept, restarts, ranUnsigned, lostFix, replayed>>

\* A partial receive: a crash during `bectl import` leaves a dataset half
\* written (Stop then leaves env "partial").
ImportPartly ==
    /\ running = "a" /\ step = "begun" /\ env = "none"
    /\ env' = "partial"
    /\ UNCHANGED <<running, started, closing, patched, once, perm, record, step, offer,
                   taken, patchKept, restarts, ranUnsigned, lostFix, replayed>>

\* The kept patch's rebuild written into the new environment, or nothing
\* to write: the release fixes it, or there's none.
CarryPatches ==
    /\ running = "a" /\ step = "imported"
    /\ patched' = (patchKept /\ offer.patch \in {"fixed", "rebuilt"})
    /\ step' = "carried"
    /\ UNCHANGED <<running, started, closing, env, once, perm, record, offer, taken,
                   patchKept, restarts, ranUnsigned, lostFix, replayed>>

\* The release's number kept as the last one taken.
Take ==
    /\ running = "a" /\ step = "carried"
    /\ taken' = IF offer.n > taken THEN offer.n ELSE taken
    /\ step' = "taken"
    /\ UNCHANGED <<running, started, closing, env, patched, once, perm, record, offer,
                   patchKept, restarts, ranUnsigned, lostFix, replayed>>

ActivateOnce ==
    /\ running = "a"
    /\ IF "activateFirst" \in Old THEN step = "begun" ELSE step = "taken"
    /\ IF "permanent" \in Old
       THEN /\ perm' = "b" /\ UNCHANGED once
       ELSE /\ once' = TRUE /\ UNCHANGED perm
    /\ step' = "activated"
    /\ UNCHANGED <<running, started, closing, env, patched, record, offer, taken,
                   patchKept, restarts, ranUnsigned, lostFix, replayed>>

MarkStaged ==
    /\ running = "a" /\ step = "activated"
    /\ record' = "staged" /\ step' = "idle"
    /\ UNCHANGED <<running, started, closing, env, patched, once, perm, offer, taken,
                   patchKept, restarts, ranUnsigned, lostFix, replayed>>

\* A staged update taken back before the restart.
Cancel ==
    /\ running = "a" /\ started /\ ~closing /\ step = "idle" /\ record = "staged" /\ once
    /\ once' = FALSE /\ record' = "cancelled"
    /\ UNCHANGED <<running, started, closing, env, patched, perm, step, offer, taken,
                   patchKept, restarts, ranUnsigned, lostFix, replayed>>

-----------------------------------------------------------------------------
Stop ==
    /\ running # "off" /\ restarts < MaxRestarts
    /\ Down
    /\ restarts' = restarts + 1
    /\ UNCHANGED <<env, patched, once, perm, record, offer, taken, patchKept,
                   ranUnsigned, lostFix, replayed>>

\* The person restarts, when glassd says the update is ready, or at any
\* other time.
Restart ==
    /\ running # "off" /\ started /\ ~closing /\ step = "idle"
    /\ (record = "staged" \/ restarts < MaxRestarts)
    /\ closing' = TRUE
    /\ restarts' = IF record = "staged" THEN restarts ELSE restarts + 1
    /\ UNCHANGED <<running, started, env, patched, once, perm, record, step, offer,
                   taken, patchKept, ranUnsigned, lostFix, replayed>>

Off ==
    /\ running # "off" /\ closing
    /\ Down
    /\ UNCHANGED <<env, patched, once, perm, record, offer, taken, patchKept, restarts,
                   ranUnsigned, lostFix, replayed>>

GlassdRestart ==
    /\ running # "off" /\ started /\ ~closing /\ restarts < MaxRestarts
    /\ started' = FALSE /\ step' = "idle"
    /\ restarts' = restarts + 1
    /\ UNCHANGED <<running, closing, env, patched, once, perm, record, offer, taken,
                   patchKept, ranUnsigned, lostFix, replayed>>

\* The loader. An image that doesn't start never gets as far as glassd:
\* the person turns the computer off and on (Fail).
Start ==
    /\ running = "off"
    /\ IF once
       THEN /\ running' = "b" /\ once' = FALSE
       ELSE /\ running' = perm /\ UNCHANGED once
    /\ ranUnsigned' = (ranUnsigned \/ (running' = "b" /\ ~(offer.keyListed /\ offer.imageSigned)))
    /\ lostFix' = (lostFix \/ (running' = "b" /\ patchKept /\ ~patched))
    /\ UNCHANGED <<started, closing, env, patched, perm, record, step, offer, taken,
                   patchKept, restarts, replayed>>

\* An image that doesn't start (or a half-written one): it hangs or
\* panics, and the computer is turned off and on.
Fail ==
    /\ running = "b" /\ ~started
    /\ ~offer.starts \/ env # "complete"
    /\ Down
    /\ UNCHANGED <<env, patched, once, perm, record, offer, taken, patchKept, restarts,
                   ranUnsigned, lostFix, replayed>>

GlassdStart ==
    /\ running # "off" /\ ~started
    /\ ~(running = "b" /\ (~offer.starts \/ env # "complete"))
    /\ started' = TRUE
    /\ \/ \* On the update, set to start once: it's the release's image.
          \* The permanent one first, then the record (a crash between is
          \* finished at the next start).
          /\ running = "b" /\ record \in {"staging", "staged"} /\ perm = "a"
          /\ perm' = "b"
          /\ UNCHANGED record
       \/ /\ running = "b" /\ record \in {"staging", "staged"} /\ perm = "b"
          /\ record' = "kept"
          /\ UNCHANGED perm
       \/ \* Back on the environment before: it didn't start.
          /\ running = "a" /\ record \in {"staging", "staged"} /\ ~once
          /\ record' = "rolledback"
          /\ UNCHANGED perm
       \/ \* Back on the one before an update an undo was finishing.
          /\ running = "a" /\ record = "kept" /\ perm = "a"
          /\ record' = "undone"
          /\ UNCHANGED perm
       \/ /\ ~(running = "b" /\ record \in {"staging", "staged"})
          /\ ~(running = "a" /\ record \in {"staging", "staged"} /\ ~once)
          /\ ~(running = "a" /\ record = "kept" /\ perm = "a")
          /\ UNCHANGED <<record, perm>>
    /\ UNCHANGED <<running, closing, env, patched, once, step, offer, taken, patchKept,
                   restarts, ranUnsigned, lostFix, replayed>>

\* `at_start`'s keeping, as one more step glassd takes after the permanent
\* one is set: the record.
Kept ==
    /\ running = "b" /\ started /\ record \in {"staging", "staged"} /\ perm = "b"
    /\ record' = "kept"
    /\ UNCHANGED <<running, started, closing, env, patched, once, perm, step, offer,
                   taken, patchKept, restarts, ranUnsigned, lostFix, replayed>>

\* The person or IT takes the update back: the one before is permanent.
UndoActivate ==
    /\ running = "b" /\ started /\ ~closing /\ step = "idle" /\ record = "kept"
    /\ perm' = "a" /\ step' = "undoing"
    /\ UNCHANGED <<running, started, closing, env, patched, once, record, offer, taken,
                   patchKept, restarts, ranUnsigned, lostFix, replayed>>

UndoEnd ==
    /\ step = "undoing" /\ running # "off"
    /\ record' = "undone" /\ step' = "idle"
    /\ UNCHANGED <<running, started, closing, env, patched, once, perm, offer, taken,
                   patchKept, restarts, ranUnsigned, lostFix, replayed>>

\* An update that didn't stay is removed once nothing runs from it or will
\* start it.
Remove ==
    /\ env # "none" /\ running = "a" /\ started /\ step = "idle"
    /\ perm = "a" /\ ~once
    /\ record \in {"failed", "rolledback", "cancelled", "undone"}
    /\ env' = "none" /\ patched' = FALSE
    /\ UNCHANGED <<running, started, closing, once, perm, record, step, offer, taken,
                   patchKept, restarts, ranUnsigned, lostFix, replayed>>

Next ==
    \/ BeginStage \/ Import \/ ImportPartly \/ CarryPatches \/ Take \/ ActivateOnce
    \/ MarkStaged \/ Cancel
    \/ Stop \/ Restart \/ Off \/ GlassdRestart \/ Start \/ Fail \/ GlassdStart
    \/ Kept \/ UndoActivate \/ UndoEnd \/ Remove

Spec == Init /\ [][Next]_vars
        /\ WF_vars(Import) /\ WF_vars(CarryPatches) /\ WF_vars(Take)
        /\ WF_vars(ActivateOnce)
        /\ WF_vars(MarkStaged) /\ WF_vars(Off) /\ WF_vars(Start) /\ WF_vars(Fail)
        /\ WF_vars(GlassdStart) /\ WF_vars(Kept) /\ WF_vars(UndoEnd)
        /\ WF_vars(Remove)
        /\ WF_vars(Restart /\ record = "staged")

-----------------------------------------------------------------------------
\* The environment every start uses starts: the one before, or an update
\* that started and passed glassd's check.
EP1_TheComputerAlwaysStarts ==
    perm = "a" \/ (env = "complete" /\ offer.starts)

\* The computer never runs an image the channel's listed key and the OS
\* key didn't both sign.
EP1_OnlySignedImagesRun == ~ranUnsigned

\* An old release, or the same one again, is never taken.
EP12_NoReplay == ~replayed

\* After an update the computer still has the kept kernel patch's fix: the
\* release fixes the same thing, or brings a rebuild.
ADR0061_NoKeptFixLost == ~lostFix

\* The computer never starts an update that isn't all there.
ADR0061_NeverStartsAHalfWrittenImage == running = "b" => env = "complete"

\* A staged update ends: kept, rolled back or taken back.
ADR0061_AnUpdateEnds == (record = "staged") ~> (record \in Ended)

\* An image that doesn't start goes back to the one before by the next
\* start.
EP1_ABadImageRollsBack ==
    (running = "b" /\ ~offer.starts) ~> (running = "a" /\ record = "rolledback")

=============================================================================
