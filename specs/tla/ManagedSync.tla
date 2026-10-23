---------------------------- MODULE ManagedSync ----------------------------
(***************************************************************************)
(* What the admin console sends a managed computer (ADR 0021, ADR 0023,   *)
(* DV-1, DV-10, AD-3, EP-12, ADR 0051): signed releases, fetched, checked *)
(* and applied, through restarts, a network that drops, replays and       *)
(* forges, a disk that fails, and the org key rotated.                    *)
(*                                                                         *)
(* One kind, the policy: the one whose release is kept, then written as   *)
(* the policy file, then swapped into the runtime (policy and registry    *)
(* together). The catalog, the directory and Chat's release follow the    *)
(* same path with nothing to fail after the release is kept.              *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/bin/glassd/src/managed.rs: Managed::start (Restart), run      *)
(*    (Sync, RetryKept, Wait), handle -> apply_policy (Receive, Swap),    *)
(*    refused, request.                                                    *)
(*  - crates/lib/managed/src/signing.rs: Signed::verify (trusted key,     *)
(*    version above the one held).                                         *)
(*  - crates/lib/managed/src/store.rs: ReleaseStore (kept: atomic save,   *)
(*    checked again on load).                                              *)
(*  - The console answers a sync at once with its latest release when it  *)
(*    is newer than the version the device names, and holds it otherwise. *)
(*                                                                         *)
(* Before ADR 0051 TLC found, against the code as it was: a release the   *)
(* device refused was fetched and refused again on every sync, at once,   *)
(* without end; and a device whose kept release failed to go back in     *)
(* force at start forgot its version, so an older release replayed to it *)
(* was applied: a rollback.                                                *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS
  MaxV,       \* versions IT publishes
  Bad,        \* versions signed properly whose content doesn't validate here
  RotateAt,   \* versions up to it are signed with the old key, later ones the new
  MaxFaults   \* network drops, disk failures, restarts and replays, together

K1 == "old key"
K2 == "new key"
Forged == "a key IT doesn't have"
KeyOf(v) == IF v <= RotateAt THEN K1 ELSE K2
Max(a, b) == IF a > b THEN a ELSE b

VARIABLES
  published,  \* the console's latest release
  trusted,    \* the org keys in the device's config
  rotated,    \* steps of the key rotation done: trust both, then the new alone
  net,        \* the console is reachable
  diskFull,   \* writes fail
  faults,     \* drops, disk failures and restarts so far
  version,    \* the version in force, as the device knows it
  runtime,    \* the policy in force in the runtime
  floor,      \* a kept release verified but not in force yet (0: none)
  skip,       \* the highest proper release whose content was refused
  backoff,    \* waiting before syncing again, after a refusal or a failure
  applying,   \* a release kept, its file not written yet
  kept,       \* the release kept on disk (ReleaseStore)
  file,       \* the policy file
  highest, untrusted, loop   \* what the invariants look back on

vars == <<published, trusted, rotated, net, diskFull, faults, version, runtime, floor, skip,
          backoff, applying, kept, file, highest, untrusted, loop>>
env == <<published, trusted, rotated, net, diskFull, faults>>

Init ==
  /\ published = 0 /\ trusted = {K1} /\ rotated = 0 /\ net = TRUE /\ diskFull = FALSE
  /\ faults = 0
  /\ version = 0 /\ runtime = 0 /\ floor = 0 /\ skip = 0 /\ backoff = FALSE /\ applying = 0
  /\ kept = 0 /\ file = 0
  /\ highest = 0 /\ untrusted = FALSE /\ loop = [v \in 1..MaxV |-> 0]

(* The world: IT publishes; the network and the disk fail and recover.    *)
Publish ==
  /\ published < MaxV /\ published' = published + 1
  /\ UNCHANGED <<trusted, rotated, net, diskFull, faults, version, runtime, floor, skip,
                 backoff, applying, kept, file, highest, untrusted, loop>>
NetFlip ==
  /\ faults < MaxFaults /\ faults' = faults + 1 /\ net' = ~net
  /\ UNCHANGED <<published, trusted, rotated, diskFull, version, runtime, floor, skip,
                 backoff, applying, kept, file, highest, untrusted, loop>>
DiskFlip ==
  /\ faults < MaxFaults /\ faults' = faults + 1 /\ diskFull' = ~diskFull
  /\ UNCHANGED <<published, trusted, rotated, net, version, runtime, floor, skip,
                 backoff, applying, kept, file, highest, untrusted, loop>>

(* What the device holds a release against: the version in force, or a   *)
(* kept one not in force yet, whichever is higher.                         *)
Have == Max(version, floor)

(* A release refused: audited, nothing changes. A proper release whose    *)
(* content doesn't validate is asked past (skip); anything else, the loop *)
(* backs off and asks again later: a forged or replayed release, or one   *)
(* signed with a key not trusted yet, never moves what it asks for.       *)
Refuse(v, fetched, proper) ==
  /\ loop' = IF fetched THEN [loop EXCEPT ![v] = loop[v] + 1] ELSE loop
  /\ skip' = IF proper THEN Max(skip, v) ELSE skip
  /\ backoff' = IF proper THEN backoff ELSE TRUE

(* handle -> apply_policy: verified (a trusted key, newer than what it   *)
(* holds), validated, then kept (saved first).                            *)
Receive(v, k, fetched) ==
  IF k \notin trusted \/ (Have # 0 /\ v <= Have)
  THEN /\ Refuse(v, fetched, FALSE) /\ UNCHANGED <<applying, kept, untrusted>>
  ELSE IF v \in Bad
  THEN /\ Refuse(v, fetched, TRUE) /\ UNCHANGED <<applying, kept, untrusted>>
  ELSE IF diskFull
  THEN /\ Refuse(v, fetched, FALSE) /\ UNCHANGED <<applying, kept, untrusted>>
  ELSE /\ applying' = v /\ kept' = v
       /\ untrusted' = (untrusted \/ k \notin trusted)
       /\ UNCHANGED <<skip, backoff, loop>>

(* run: a sync names the highest of what it has in force, what it keeps   *)
(* and what it refused; the console answers at once with its latest       *)
(* release when that is newer, and holds the request otherwise.          *)
Sync ==
  /\ net /\ applying = 0 /\ floor = 0 /\ ~backoff
  /\ published > Max(Have, skip)
  /\ Receive(published, KeyOf(published), TRUE)
  /\ UNCHANGED <<env, version, runtime, floor, file, highest>>

(* Someone on the path replays a release, or forges one. Bounded with    *)
(* the other faults: one who replays without end stalls the device, which *)
(* nothing on it can prevent.                                              *)
Replay(v, k) ==
  /\ faults < MaxFaults /\ faults' = faults + 1
  /\ applying = 0 /\ v \in 1..published
  /\ Receive(v, k, FALSE)
  /\ UNCHANGED <<published, trusted, rotated, net, diskFull, version, runtime, floor, file, highest>>

(* After a refusal or a failure, time passes before the next sync.        *)
Wait ==
  /\ backoff /\ backoff' = FALSE
  /\ loop' = [v \in 1..MaxV |-> 0]
  /\ UNCHANGED <<env, version, runtime, floor, skip, applying, kept, file, highest, untrusted>>

(* The policy file written, then policy and registry swapped in together. *)
(* A failure here leaves the kept release to go in force from disk.       *)
Swap ==
  /\ applying # 0
  /\ IF diskFull
     THEN /\ floor' = applying /\ backoff' = TRUE
          /\ UNCHANGED <<file, runtime, version, highest>>
     \* In force: nothing kept waits any more.
     ELSE /\ file' = applying /\ runtime' = applying /\ version' = applying
          /\ floor' = 0 /\ highest' = Max(highest, applying)
          /\ UNCHANGED backoff
  /\ applying' = 0
  /\ UNCHANGED <<env, skip, kept, untrusted, loop>>

(* run, before syncing: the release kept on disk now goes in force when   *)
(* it is newer than what is.                                               *)
RetryKept ==
  /\ floor # 0 /\ applying = 0 /\ ~backoff
  /\ IF kept <= version
     THEN /\ floor' = 0 /\ UNCHANGED <<file, runtime, version, highest, backoff>>
     ELSE IF diskFull
     THEN /\ backoff' = TRUE /\ UNCHANGED <<file, runtime, version, floor, highest>>
     ELSE /\ file' = kept /\ runtime' = kept /\ version' = kept /\ floor' = 0
          /\ highest' = Max(highest, kept)
          /\ UNCHANGED backoff
  /\ UNCHANGED <<env, skip, applying, kept, untrusted, loop>>

(* glassd stops, at any point, and starts again, perhaps with IT's key    *)
(* rotation a step further in its config. The policy file is what's in    *)
(* force; Managed::start checks the kept release again: its version is in *)
(* force if the file is its text, and held as the floor if not.           *)
Configs == {trusted} \cup (CASE rotated = 0 -> {{K1, K2}} [] rotated = 1 -> {{K2}} [] OTHER -> {})
Restart ==
  /\ faults < MaxFaults /\ faults' = faults + 1
  /\ \E t \in Configs :
       LET ok == kept # 0 /\ KeyOf(kept) \in t
       IN /\ trusted' = t
          /\ rotated' = IF t = trusted THEN rotated ELSE rotated + 1
          /\ version' = IF ok /\ file = kept THEN kept ELSE 0
          /\ floor' = IF ok /\ file # kept THEN kept ELSE 0
  /\ runtime' = file
  /\ applying' = 0 /\ skip' = 0 /\ backoff' = FALSE
  /\ loop' = [v \in 1..MaxV |-> 0]
  /\ UNCHANGED <<published, net, diskFull, kept, file, highest, untrusted>>

Next ==
  \/ Publish \/ NetFlip \/ DiskFlip \/ Restart
  \/ Sync \/ Wait \/ Swap \/ RetryKept
  \/ \E v \in 1..MaxV : Replay(v, KeyOf(v)) \/ Replay(v, Forged)

(* The device's own steps happen when they can; the world may stop.      *)
Spec == Init /\ [][Next]_vars
        /\ WF_vars(Sync) /\ WF_vars(Wait) /\ WF_vars(Swap) /\ WF_vars(RetryKept)

(***************************************************************************)
(* What the OS promises.                                                   *)
(***************************************************************************)
\* DV-10: an old release can't be replayed to undo a newer one.
DV10_NoRollback == runtime >= highest
\* DV-1: only a release signed with a key the device trusts is applied.
DV1_OnlyTrusted == ~untrusted
\* A release refused isn't fetched and refused again without a pause.
NoRefusalLoop == \A v \in 1..MaxV : loop[v] <= 1
\* What the device asks past is a proper release IT published, never one
\* a forger or a replay chose.
OnlyProperSkipped == skip = 0 \/ (skip \in Bad /\ skip <= published)
\* ADR 0023: when the console's latest release is proper and the device
\* trusts its key, it ends up in force, once the network and the disk work.
LatestUsable == published # 0 /\ published \notin Bad /\ KeyOf(published) \in trusted
ADR0023_Converges ==
  <>[](net /\ ~diskFull) => <>[](LatestUsable => runtime = published)
=============================================================================
