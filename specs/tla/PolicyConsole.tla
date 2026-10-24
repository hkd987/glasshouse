---------------------------- MODULE PolicyConsole ----------------------------
(***************************************************************************)
(* IT's policy from the console's draft to the org's computers (AD-2,     *)
(* AD-3, DV-12, ADR 0023).                                                 *)
(*                                                                         *)
(* Two admins work on one draft at once: each makes an edit of their own, *)
(* previews its impact, publishes to the pilot ring, promotes the pilot's *)
(* version to broad, rolls back to an earlier version or discards the     *)
(* draft. Two computers, one in each ring, sync what their ring runs; IT  *)
(* moves them between rings. A draft's content is the set of edits in it. *)
(*                                                                         *)
(* The code, each action one request under the console's state lock:     *)
(*  - crates/bin/glass-console/src/state.rs: edit_draft (Edit), publish,  *)
(*    promote, rollback, discard_draft, set_ring, sync (Sync);            *)
(*    server.rs POST /api/policy/preview (Preview: impact::preview of the *)
(*    draft) and GET /api/policy, what the page shows (Look).             *)
(*  - apps/console/src/policy.ts: what the page sends, from what it last  *)
(*    loaded and previewed.                                                *)
(*  - glassd's side of a release is ManagedSync.tla and PolicyApply.tla:  *)
(*    here a computer takes what its ring runs if it's newer.             *)
(*                                                                         *)
(* Old holds the code before ADR 0053, to show TLC needs each fix:        *)
(*  - "stalePublish": publish took whatever the draft was, previewed or   *)
(*    not, so the other admin's edit went out unseen;                     *)
(*  - "stalePromote": promote took whatever the pilot ring ran, so a      *)
(*    version published as IT clicked went to broad untried;              *)
(*  - "silentRollback": a rollback put the earlier version in the draft   *)
(*    and said nothing of the unpublished edits it threw away.            *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  Old,          \* fixes switched off, as above
  MaxVersions   \* versions the console signs, the first included

Admins == {"a", "b"}
EditOf == [x \in Admins |-> IF x = "a" THEN "ea" ELSE "eb"]
Rings == {"pilot", "broad"}
Devices == {"d1", "d2"}

NoPreview == [ok |-> FALSE, c |-> {}]

VARIABLES
  draft,      \* the draft's edits
  versions,   \* what the console signed, in order
  rings,      \* each ring's version
  previewed,  \* what each admin's page last previewed
  shown,      \* the pilot version each admin's page shows
  promotions, \* each promote: the version sent, and the one the page showed
  ringOf,     \* each computer's ring
  have,       \* the version each computer runs
  silent      \* an unpublished edit thrown away without a word

vars == <<draft, versions, rings, previewed, shown, promotions, ringOf, have, silent>>

Content(v) == versions[v].content

TypeOK ==
  /\ draft \subseteq {"ea", "eb"}
  /\ rings \in [Rings -> 1..Len(versions)]
  /\ have \in [Devices -> 1..Len(versions)]
  /\ ringOf \in [Devices -> Rings]
  /\ silent \in BOOLEAN

\* The console starts with the default policy signed as v1, everywhere.
Init ==
  /\ draft = {}
  /\ versions = << [content |-> {}, kind |-> "initial", previewed |-> NoPreview] >>
  /\ rings = [r \in Rings |-> 1]
  /\ previewed = [x \in Admins |-> NoPreview]
  /\ shown = [x \in Admins |-> 1]
  /\ promotions = << >>
  /\ ringOf = [d \in Devices |-> IF d = "d1" THEN "pilot" ELSE "broad"]
  /\ have = [d \in Devices |-> 1]
  /\ silent = FALSE

-----------------------------------------------------------------------------
(* An admin.                                                               *)

\* One control changed on the policy page: applied to the draft as it is
\* now, so two admins' edits both stay.
Edit(x) ==
  /\ EditOf[x] \notin draft
  /\ draft' = draft \cup {EditOf[x]}
  /\ UNCHANGED <<versions, rings, previewed, shown, promotions, ringOf, have, silent>>

Preview(x) ==
  /\ previewed' = [previewed EXCEPT ![x] = [ok |-> TRUE, c |-> draft]]
  /\ UNCHANGED <<draft, versions, rings, shown, promotions, ringOf, have, silent>>

\* The page loads the rings.
Look(x) ==
  /\ shown' = [shown EXCEPT ![x] = rings["pilot"]]
  /\ UNCHANGED <<draft, versions, rings, previewed, promotions, ringOf, have, silent>>

\* Publish to the pilot ring: refused when it's what the pilot runs, and,
\* fixed, unless the page previewed this draft (the page sends what it
\* previewed; the console compares).
Publish(x) ==
  /\ Len(versions) < MaxVersions
  /\ draft # Content(rings["pilot"])
  /\ "stalePublish" \in Old \/ (previewed[x].ok /\ previewed[x].c = draft)
  /\ versions' = Append(versions, [content |-> draft, kind |-> "publish",
                                   previewed |-> previewed[x]])
  /\ rings' = [rings EXCEPT !["pilot"] = Len(versions) + 1]
  /\ UNCHANGED <<draft, previewed, shown, promotions, ringOf, have, silent>>

\* Promote: broad gets the pilot's version, fixed only the one the page
\* showed.
Promote(x) ==
  /\ rings["broad"] # rings["pilot"]
  /\ "stalePromote" \in Old \/ shown[x] = rings["pilot"]
  /\ promotions' = Append(promotions, [sent |-> rings["pilot"], shown |-> shown[x]])
  /\ rings' = [rings EXCEPT !["broad"] = rings["pilot"]]
  /\ UNCHANGED <<draft, versions, previewed, shown, ringOf, have, silent>>

\* Roll back to an earlier version's rules, as a new version, everywhere;
\* the draft becomes them. Fixed, the answer says when that threw away
\* unpublished edits.
Rollback(x, to) ==
  /\ Len(versions) < MaxVersions
  /\ to \in 1..Len(versions) /\ to # rings["pilot"]
  /\ versions' = Append(versions, [content |-> Content(to), kind |-> "rollback",
                                   previewed |-> NoPreview])
  /\ rings' = [r \in Rings |-> Len(versions) + 1]
  /\ draft' = Content(to)
  /\ silent' = (silent \/ ("silentRollback" \in Old /\ draft # Content(rings["pilot"])))
  /\ UNCHANGED <<previewed, shown, promotions, ringOf, have>>

\* Back to what the pilot runs: the admin chose to throw the draft away.
Discard(x) ==
  /\ draft' = Content(rings["pilot"])
  /\ UNCHANGED <<versions, rings, previewed, shown, promotions, ringOf, have, silent>>

SetRing(d, r) ==
  /\ ringOf' = [ringOf EXCEPT ![d] = r]
  /\ UNCHANGED <<draft, versions, rings, previewed, shown, promotions, have, silent>>

-----------------------------------------------------------------------------
(* A computer: what its ring runs, if newer than what it has.             *)

Sync(d) ==
  /\ rings[ringOf[d]] > have[d]
  /\ have' = [have EXCEPT ![d] = rings[ringOf[d]]]
  /\ UNCHANGED <<draft, versions, rings, previewed, shown, promotions, ringOf, silent>>

Next ==
  \/ \E x \in Admins :
       \/ Edit(x) \/ Preview(x) \/ Look(x) \/ Publish(x) \/ Promote(x) \/ Discard(x)
       \/ \E to \in 1..MaxVersions : Rollback(x, to)
  \/ \E d \in Devices : Sync(d) \/ \E r \in Rings : SetRing(d, r)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* What IT is promised.                                                    *)

\* AD-3: what IT published is what IT previewed ("this change affects 214
\* users and 3 agents").
AD3_PublishedIsWhatWasPreviewed ==
  \A v \in 1..Len(versions) : versions[v].kind = "publish" =>
    versions[v].previewed.ok /\ versions[v].previewed.c = versions[v].content

\* DV-12: a ring is promoted to the version IT saw tried in the ring before.
DV12_PromotedIsWhatWasShown ==
  \A i \in 1..Len(promotions) : promotions[i].sent = promotions[i].shown

\* AD-2: two admins' edits are both kept, or thrown away by someone's
\* choice, never without a word.
AD2_NoEditLostSilently == ~silent

\* AD-3: a rollback is a new version, the newest, everywhere.
AD3_ARollbackIsTheNewestVersion ==
  \A v \in 1..Len(versions) : versions[v].kind = "rollback" =>
    \A w \in 1..Len(versions) : w > v => versions[w].kind # "initial"

\* AD-3: a computer's version only goes up, so a replay is refused.
AD3_AComputersVersionOnlyGoesUp == [][\A d \in Devices : have'[d] >= have[d]]_vars
=============================================================================
