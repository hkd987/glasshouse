----------------------------- MODULE StoreShare -----------------------------
(***************************************************************************)
(* An app someone made, shared to the org's store, reviewed by IT and     *)
(* reaching computers (ADR 0032 slice R2; GA-8, GA-13, DV-11, DV-12).     *)
(*                                                                         *)
(* One catalog id. Its maker shares it from two computers, whose own      *)
(* versions of it differ, and someone else shares an app the console      *)
(* names the same. IT looks at the item and approves it for a group, or   *)
(* blocks it, at any moment. Two computers, in different groups, sync     *)
(* the signed catalog, fetch the bundle and serve the app; the network    *)
(* drops between a sync and its fetch.                                     *)
(*                                                                         *)
(* The code, each action one step under one lock:                          *)
(*  - crates/bin/glass-console/src/state.rs, under the console's state     *)
(*    lock: share_plan (Plan), take_share (Take), approve and block, each  *)
(*    with release_catalog (ITApprove, ITBlock), ConsoleItem::live (Live), *)
(*    org_bundle (inside Fetch). server.rs apps_submit checks and builds   *)
(*    between Plan and Take without the lock; GET catalog/<id> is what IT  *)
(*    reviews (ITLook).                                                     *)
(*  - crates/bin/glassd/src/managed.rs, glassd's one sync thread: handle   *)
(*    -> apply_catalog (Sync), fetch_org_apps after it (Fetch), and        *)
(*    retry_org_apps at a sync with no new catalog, at most once a minute, *)
(*    while a bundle the catalog lists is missing (Retry).                 *)
(*  - crates/bin/glassd/src/org_apps.rs: fetch and keep (inside Fetch),    *)
(*    OrgApps::apps and files; crates/svc/runtime/src/apps.rs org_apps;    *)
(*    crates/apps/catalog/src/store.rs reaches, catalog.install and        *)
(*    catalog.remove (Serves, Install, Uninstall).                          *)
(*                                                                         *)
(* Before this model, TLC found against the code:                          *)
(*  - IT's approve named the item, not the version IT reviewed: a new      *)
(*    version shared while IT read the old one was approved unseen and     *)
(*    signed to every computer (GA13_DevicesRunOnlyWhatITApproved).        *)
(*    approve now names the version and refuses another.                   *)
(*  - A bundle fetch that failed (the network dropped after the sync) was  *)
(*    tried again only with the next catalog or a restart: the approved    *)
(*    app never reached that computer (GA13_ApprovedReachesComputers).     *)
(*    A missing bundle is fetched again at a later sync, once a minute.    *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS
  MaxRel,      \* catalog releases IT signs
  MaxDrops     \* network drops per device

\* Dana shares her app from two computers that hold different versions of
\* it (2 and 1); Sam shares one the console names the same. One computer
\* receives the catalog: computers don't affect each other, so one in a
\* group IT may or may not approve it for stands for every computer.
Computers == {"dana1", "dana2", "sam1"}
MakerOf == [c \in Computers |-> IF c = "sam1" THEN "sam" ELSE "dana"]
OwnVersion == [c \in Computers |-> IF c = "dana1" THEN 2 ELSE 1]
Devices == {"d1"}
Groups == {"ap", "legal"}
GroupOf == [d \in Devices |-> "ap"]

NONE == "none"
\* Blocked for a group is left out: with one group per person it reaches
\* them as not approved for it does (CatalogItem::mode_for).
Modes == {"req", "avail"}
NoGroups == [g \in Groups |-> NONE]
\* No version: what a catalog without the item gives.
NoLive == [cv |-> 0, groups |-> NoGroups]

VARIABLES
  \* The console.
  item,        \* [cv, maker, from, mv, plan] of the item's current version; cv 0: none yet
  review,      \* "waiting", "approved" or "blocked"
  groups,      \* group -> mode, of the item's current version
  kept,        \* what computers keep while a newer version waits (ConsoleItem.approved)
  rel,         \* the latest signed catalog: [v, live]
  plans,       \* shares the console planned and is checking
  shared,      \* computers that shared already (each shares once)
  nplan,       \* plans so far, to order them
  seen,        \* the version on IT's screen (0: none)
  itApproved,  \* the versions IT approved, each having looked at it
  \* Each device.
  dRel,        \* the catalog in force: [v, live]
  bundles,     \* versions whose bundle is kept
  pending,     \* a fetch is due (after a sync, or the report's retry)
  online,
  drops,
  installed    \* the person installed it from the Store (catalog.install)

vars == <<item, review, groups, kept, rel, plans, shared, nplan, seen, itApproved,
          dRel, bundles, pending, online, drops, installed>>

\* What computers get: ConsoleItem::live.
Live ==
  IF item.cv = 0 THEN NoLive
  ELSE IF review = "approved"
       THEN [cv |-> item.cv, groups |-> groups]
       ELSE IF review = "waiting" THEN kept ELSE NoLive

\* CatalogItem::mode_for and catalog store.rs reaches, for one group.
Reaches(live, d) ==
  /\ live.cv # 0
  /\ \/ live.groups[GroupOf[d]] = "req"
     \/ live.groups[GroupOf[d]] = "avail" /\ installed[d]

\* What the device serves (OrgApps::files): the version its catalog lists,
\* from a bundle it kept, to a person the catalog lets it reach.
Serves(d) ==
  LET l == dRel[d].live IN
  IF l.cv # 0 /\ l.cv \in bundles[d] /\ Reaches(l, d) THEN l.cv ELSE 0

Init ==
  /\ item = [cv |-> 0, maker |-> NONE, from |-> NONE, mv |-> 0, plan |-> 0]
  /\ review = "waiting"
  /\ groups = NoGroups
  /\ kept = NoLive
  /\ rel = [v |-> 0, live |-> NoLive]
  /\ plans = {}
  /\ shared = {}
  /\ nplan = 0
  /\ seen = 0
  /\ itApproved = {}
  /\ dRel = [d \in Devices |-> [v |-> 0, live |-> NoLive]]
  /\ bundles = [d \in Devices |-> {}]
  /\ pending = [d \in Devices |-> FALSE]
  /\ online = [d \in Devices |-> TRUE]
  /\ drops = [d \in Devices |-> 0]
  /\ installed = [d \in Devices |-> FALSE]

DeviceVars == <<dRel, bundles, pending, online, drops, installed>>

(* share_plan: the next version of the item its maker shared before, or a *)
(* new item, under the lock. Another maker's app the console would name   *)
(* the same is planned as the same new id while none is taken yet; once   *)
(* one is, theirs gets another id (outside the model).                    *)
Plan(c) ==
  /\ c \notin shared
  /\ item.cv = 0 \/ item.maker = MakerOf[c]
  /\ plans' = plans \cup {[c |-> c, pv |-> item.cv + 1, n |-> nplan + 1]}
  /\ shared' = shared \cup {c}
  /\ nplan' = nplan + 1
  /\ UNCHANGED <<item, review, groups, kept, rel, seen, itApproved>>
  /\ UNCHANGED DeviceVars

(* take_share: kept for review only if nothing else came in while it was  *)
(* checked ("It changed while the console checked it; share it again."). *)
(* A new version of an approved app leaves the approved one with          *)
(* computers; who it reaches carries over.                                 *)
Take(p) ==
  /\ p \in plans
  /\ plans' = plans \ {p}
  /\ IF p.pv = item.cv + 1 /\ (item.cv = 0 \/ item.maker = MakerOf[p.c])
     THEN /\ kept' = IF review = "approved"
                     THEN [cv |-> item.cv, groups |-> groups] ELSE kept
          /\ item' = [cv |-> p.pv, maker |-> MakerOf[p.c], from |-> p.c,
                      mv |-> OwnVersion[p.c], plan |-> p.n]
          /\ review' = "waiting"
     ELSE UNCHANGED <<item, review, kept>>
  /\ UNCHANGED <<groups, rel, shared, nplan, seen, itApproved>>
  /\ UNCHANGED DeviceVars

(* IT opens the item: the version it reviews. *)
ITLook ==
  /\ item.cv > 0
  /\ seen # item.cv
  /\ seen' = item.cv
  /\ UNCHANGED <<item, review, groups, kept, rel, plans, shared, nplan, itApproved>>
  /\ UNCHANGED DeviceVars

Sign(live) == rel' = [v |-> rel.v + 1, live |-> live]

(* approve: the version IT reviewed, for a group, then release_catalog,   *)
(* one step under the lock. Another version than the one named is         *)
(* refused: "Version n came in while you were reviewing".                 *)
ITApprove(g, m) ==
  /\ seen > 0
  /\ rel.v < MaxRel
  /\ seen = item.cv
  /\ itApproved' = itApproved \cup {seen}
  /\ review' = "approved"
  /\ kept' = NoLive
  /\ groups' = [groups EXCEPT ![g] = m]
  /\ Sign([cv |-> item.cv, groups |-> groups'])
  /\ UNCHANGED <<item, plans, shared, nplan, seen>>
  /\ UNCHANGED DeviceVars

(* block, then release_catalog: left out on every computer. *)
ITBlock ==
  /\ item.cv > 0
  /\ rel.v < MaxRel
  /\ review # "blocked"
  /\ review' = "blocked"
  /\ kept' = NoLive
  /\ Sign(NoLive)
  /\ UNCHANGED <<item, groups, plans, shared, nplan, seen, itApproved>>
  /\ UNCHANGED DeviceVars

ConsoleVars == <<item, review, groups, kept, rel, plans, shared, nplan, seen, itApproved>>

(* A sync answered with a newer catalog: verified, kept, in force; the    *)
(* bundles are fetched next, in the same thread (handle).                 *)
Sync(d) ==
  /\ online[d]
  /\ rel.v > dRel[d].v
  /\ dRel' = [dRel EXCEPT ![d] = rel]
  /\ pending' = [pending EXCEPT ![d] = TRUE]
  /\ UNCHANGED <<bundles, online, drops, installed>>
  /\ UNCHANGED ConsoleVars

(* fetch_org_apps: the listed version's bundle if this computer lacks it, *)
(* from org_bundle, which serves only the version computers get now and   *)
(* is kept only if it matches the signed digest; an id the catalog no     *)
(* longer lists is removed.                                                *)
Fetch(d) ==
  /\ pending[d]
  /\ pending' = [pending EXCEPT ![d] = FALSE]
  /\ LET l == dRel[d].live IN
     bundles' = [bundles EXCEPT ![d] =
       IF l.cv = 0 THEN {}
       ELSE IF l.cv \notin bundles[d] /\ online[d] /\ Live.cv = l.cv
            THEN bundles[d] \cup {l.cv}
            ELSE bundles[d]]
  /\ UNCHANGED <<dRel, online, drops, installed>>
  /\ UNCHANGED ConsoleVars

(* retry_org_apps: at a sync with no new catalog, once a minute, a listed *)
(* bundle still missing is fetched again (the minute is left out).        *)
Retry(d) ==
  /\ ~pending[d]
  /\ online[d]
  /\ dRel[d].live.cv # 0
  /\ dRel[d].live.cv \notin bundles[d]
  /\ pending' = [pending EXCEPT ![d] = TRUE]
  /\ UNCHANGED <<dRel, bundles, online, drops, installed>>
  /\ UNCHANGED ConsoleVars

Drop(d) ==
  /\ online[d]
  /\ drops[d] < MaxDrops
  /\ online' = [online EXCEPT ![d] = FALSE]
  /\ drops' = [drops EXCEPT ![d] = drops[d] + 1]
  /\ UNCHANGED <<dRel, bundles, pending, installed>>
  /\ UNCHANGED ConsoleVars

Reconnect(d) ==
  /\ ~online[d]
  /\ online' = [online EXCEPT ![d] = TRUE]
  /\ UNCHANGED <<dRel, bundles, pending, drops, installed>>
  /\ UNCHANGED ConsoleVars

(* catalog.install / catalog.remove: only what the catalog in force makes *)
(* available to their group.                                              *)
Install(d) ==
  /\ dRel[d].live.groups[GroupOf[d]] = "avail"
  /\ ~installed[d]
  /\ installed' = [installed EXCEPT ![d] = TRUE]
  /\ UNCHANGED <<dRel, bundles, pending, online, drops>>
  /\ UNCHANGED ConsoleVars

Uninstall(d) ==
  /\ dRel[d].live.groups[GroupOf[d]] = "avail"
  /\ installed[d]
  /\ installed' = [installed EXCEPT ![d] = FALSE]
  /\ UNCHANGED <<dRel, bundles, pending, online, drops>>
  /\ UNCHANGED ConsoleVars

Next ==
  \/ \E c \in Computers : Plan(c)
  \/ \E p \in plans : Take(p)
  \/ ITLook
  \/ \E g \in Groups, m \in Modes : ITApprove(g, m)
  \/ ITBlock
  \/ \E d \in Devices :
       Sync(d) \/ Fetch(d) \/ Retry(d) \/ Drop(d) \/ Reconnect(d)
       \/ Install(d) \/ Uninstall(d)

Fairness ==
  /\ WF_vars(\E p \in plans : Take(p))
  /\ \A d \in Devices :
       WF_vars(Sync(d)) /\ WF_vars(Fetch(d)) /\ WF_vars(Retry(d)) /\ WF_vars(Reconnect(d))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
TypeOK ==
  /\ review \in {"waiting", "approved", "blocked"}
  /\ groups \in [Groups -> Modes \cup {NONE}]
  /\ \A d \in Devices : Serves(d) \in 0..Cardinality(Computers)

(* GA-13, DV-1: a computer serves only a version IT approved, having      *)
(* looked at that version, and the console signs nothing else.            *)
GA13_DevicesRunOnlyWhatITApproved ==
  /\ \A d \in Devices : Serves(d) # 0 => Serves(d) \in itApproved
  /\ rel.live.cv # 0 => rel.live.cv \in itApproved

(* GA-8: a version waiting for review reaches no one. *)
GA8_WaitingReachesNoOne ==
  review = "waiting" /\ item.cv > 0 =>
    /\ rel.live = Live
    /\ rel.live.cv < item.cv

(* DV-11: what a computer serves, the catalog in force gives the person's *)
(* group as required, or available and installed; never blocked.          *)
DV11_OnlyApprovedGroups ==
  \A d \in Devices : Serves(d) # 0 =>
    dRel[d].live.groups[GroupOf[d]] \in {"req", "avail"}

(* GA-3, ADR 0032: a share never replaces one planned after it: the      *)
(* item's versions follow the order the console planned them in.          *)
GA3_NoLaterVersionOverwritten ==
  [][item'.cv # item.cv => item'.plan > item.plan /\ item'.cv = item.cv + 1]_item

(* GA-8: a block reaches every computer. *)
GA8_BlockReachesEveryComputer ==
  (<>[](Live.cv = 0)) => <>[](\A d \in Devices : Serves(d) = 0)

(* GA-13: what IT approved for a group reaches its computers. *)
GA13_ApprovedReachesComputers ==
  \A d \in Devices :
    (<>[](Reaches(Live, d))) => <>[](Serves(d) = Live.cv)
=============================================================================
