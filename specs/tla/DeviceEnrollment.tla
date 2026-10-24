--------------------------- MODULE DeviceEnrollment ---------------------------
(***************************************************************************)
(* A managed computer's key and enrollment (ADR 0030 slice P, ADR 0023,   *)
(* DV-1): glassd makes the computer's key, registers it with the org's    *)
(* enrollment token, then signs every request with it; the console        *)
(* answers a long-polling sync with what IT released; IT removes the      *)
(* computer, forgets it, and releases more; glassd stops at any point,    *)
(* and the computer can be set up again from scratch under the same id.   *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/lib/managed/src/device.rs `kept_secret`: the key on disk, or  *)
(*    a new one written there. KeyStart, KeyWrite.                         *)
(*  - crates/bin/glassd/src/managed.rs `Managed::run`: register first, and *)
(*    again whenever the console says it doesn't know this computer, then  *)
(*    sync; a refusal waits and tries again. Register, SyncAsk.            *)
(*  - crates/bin/glass-console/src/server.rs `device`, `signed_by`,        *)
(*    `register_device`, the `sync` long poll (checked, then answered      *)
(*    after the wait); state.rs `register_device`, `computer`,             *)
(*    `revoke_device`, `forget_device`, `sync` (which made an entry for a  *)
(*    device it didn't have). SyncAnswer, Remove, Forget, Release.         *)
(*                                                                         *)
(* Abstractions: one computer id; keys k1 (the first) and k2 (the one it   *)
(* makes when set up again); a release is a number; signatures can't be  *)
(* forged and each is used once (`seen`), so what a stolen or replayed    *)
(* request can do is what the computer itself does; the device report    *)
(* goes the way of the sync (checked, then applied) and is left out.      *)
(*                                                                         *)
(* Fixed = FALSE is the code before ADR 0052; TRUE is the code now:        *)
(*  1. The console remembers the keys of computers IT removed after it     *)
(*     forgets them: one asking to register again is refused as removed.   *)
(*     Before, glassd heard "isn't registered" after the forget and        *)
(*     registered its old key again with the enrollment token: the removed *)
(*     computer was back in the org by itself.                             *)
(*  2. The long polls' answers (sync, chat) and the report check again,   *)
(*     under the state's lock, that the key that signed is still the       *)
(*     computer's: a computer removed while it waited gets nothing         *)
(*     released after, and nothing makes an entry for one IT forgot.       *)
(*  3. The key is written whole under another name and linked into place: *)
(*     glassd stopping halfway left an empty key, "damaged", and the       *)
(*     computer never synced again until someone deleted it by hand. An    *)
(*     empty key file, which the old code could leave, is made again.      *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxReleases,    \* releases IT makes
    MaxRestarts,    \* glassd stopping and starting again
    Fixed           \* the code with ADR 0052's fixes

Keys == {"k1", "k2"}
Absent == [key |-> "absent", revoked |-> FALSE]

VARIABLES
    keyfile,    \* the device key file: "absent", "partial" or a whole key
    gpc,        \* glassd: "start", "made" (created, not written), "run", "stuck"
    gkey,       \* the key glassd signs with
    again,      \* the computer was set up again from scratch (its key wiped)
    registered, \* glassd's `registered`
    req,        \* the sync request in flight: "none" or "polling"
    have,       \* the newest release this computer took
    restarts,
    dev,        \* the console's entry for the id: Absent, or a key ("none": made
                \* by `sync` for an id it didn't have) and whether it's removed
    removedKeys,\* Fixed: keys of computers IT removed and forgot
    released,   \* the newest release IT made
    removedAt,  \* ghost: for each key IT removed, the newest release then
    rejoined,   \* ghost: a removed key's registration was taken again
    leak        \* ghost: a removed computer took a release made after

vars == <<keyfile, gpc, gkey, again, registered, req, have, restarts, dev,
          removedKeys, released, removedAt, rejoined, leak>>

NoRelease == 99

TypeOK ==
    /\ keyfile \in {"absent", "partial"} \cup Keys
    /\ gpc \in {"start", "made", "run", "stuck"}
    /\ gkey \in Keys \cup {"none"}
    /\ registered \in BOOLEAN /\ again \in BOOLEAN
    /\ req \in {"none", "polling"}
    /\ dev.key \in Keys \cup {"none", "absent"} /\ dev.revoked \in BOOLEAN
    /\ removedKeys \subseteq Keys
    /\ removedAt \in [Keys -> 0..MaxReleases \cup {NoRelease}]

Init ==
    /\ keyfile = "absent" /\ gpc = "start" /\ gkey = "none" /\ again = FALSE
    /\ registered = FALSE /\ req = "none" /\ have = 0 /\ restarts = 0
    /\ dev = Absent /\ removedKeys = {}
    /\ released = 0
    /\ removedAt = [k \in Keys |-> NoRelease]
    /\ rejoined = FALSE /\ leak = FALSE

NewKey == IF again THEN "k2" ELSE "k1"
Removed(k) == removedAt[k] # NoRelease

\* `computer(id)` is Key(k) and k signed: the signature check passes.
Signs(k) == dev.key = k /\ ~dev.revoked

----------------------------------------------------------------------------
\* glassd

\* kept_secret: the key there, a new one, or "damaged".
KeyStart ==
    /\ gpc = "start"
    /\ CASE keyfile \in Keys ->
              gkey' = keyfile /\ gpc' = "run" /\ UNCHANGED keyfile
         [] keyfile = "partial" ->
              \* "the device key ... is damaged": this device won't sync.
              gpc' = "stuck" /\ UNCHANGED <<keyfile, gkey>>
         [] keyfile = "absent" /\ Fixed ->
              \* Written whole under another name, then linked into place.
              keyfile' = NewKey /\ gkey' = NewKey /\ gpc' = "run"
         [] keyfile = "absent" ->
              \* create_new, then write_all and sync_all.
              keyfile' = "partial" /\ gpc' = "made" /\ UNCHANGED gkey
    /\ UNCHANGED <<again, registered, req, have, restarts, dev, removedKeys,
                   released, removedAt, rejoined, leak>>

KeyWrite ==
    /\ gpc = "made"
    /\ keyfile' = NewKey /\ gkey' = NewKey /\ gpc' = "run"
    /\ UNCHANGED <<again, registered, req, have, restarts, dev, removedKeys,
                   released, removedAt, rejoined, leak>>

\* glassd stops (a crash, a power cut, an update) and starts again: what it
\* had in memory is gone, and a long poll's answer goes nowhere.
Restart ==
    /\ restarts < MaxRestarts /\ gpc # "start"
    /\ restarts' = restarts + 1
    /\ gpc' = "start" /\ registered' = FALSE /\ req' = "none"
    /\ UNCHANGED <<keyfile, gkey, again, have, dev, removedKeys, released,
                   removedAt, rejoined, leak>>

\* The computer set up again from scratch, under the same id: a new key.
SetUpAgain ==
    /\ ~again
    /\ again' = TRUE /\ keyfile' = "absent" /\ gpc' = "start"
    /\ registered' = FALSE /\ req' = "none" /\ have' = 0
    /\ UNCHANGED <<gkey, restarts, dev, removedKeys, released, removedAt,
                   rejoined, leak>>

\* POST /device/v1/register with the enrollment token: one request, answered
\* under the console's state lock (`register_device`).
Register ==
    /\ gpc = "run" /\ ~registered /\ req = "none"
    /\ LET refusedAsRemoved == Fixed /\ gkey \in removedKeys
           taken ==
             CASE refusedAsRemoved -> FALSE
               [] dev.key = "absent" -> TRUE
               [] dev.revoked -> FALSE
               [] dev.key = gkey -> TRUE
               [] dev.key = "none" -> TRUE
               [] OTHER -> FALSE          \* "enrolled with another key"
       IN /\ registered' = taken
          /\ dev' = IF taken THEN [key |-> gkey, revoked |-> FALSE] ELSE dev
          /\ rejoined' = (rejoined \/ (taken /\ Removed(gkey)))
    /\ UNCHANGED <<keyfile, gpc, gkey, again, req, have, restarts, removedKeys,
                   released, removedAt, leak>>

\* POST /device/v1/sync, signed: `signed_by`, under the state lock, then the
\* long poll waits. "isn't registered" makes glassd register again.
SyncAsk ==
    /\ gpc = "run" /\ registered /\ req = "none"
    /\ IF Signs(gkey)
       THEN req' = "polling" /\ UNCHANGED registered
       ELSE /\ UNCHANGED req
            \* 401 "isn't registered" (no entry, or none with this key);
            \* 403 "removed" keeps glassd as it is, trying again later.
            /\ registered' = (dev.key # "absent" /\ dev.revoked)
    /\ UNCHANGED <<keyfile, gpc, gkey, again, have, restarts, dev, removedKeys,
                   released, removedAt, rejoined, leak>>

\* The long poll's answer, after the wait: `state.sync`.
SyncAnswer ==
    /\ req = "polling"
    /\ req' = "none"
    /\ IF Fixed /\ ~Signs(gkey)
       \* Fixed: checked again under the lock: removed or forgotten meanwhile.
       THEN /\ registered' = (dev.key # "absent" /\ dev.revoked)
            /\ UNCHANGED <<have, dev, leak>>
       ELSE /\ have' = released
            \* `self.device(id)` made an entry for an id it didn't have.
            /\ dev' = IF dev.key = "absent" THEN [key |-> "none", revoked |-> FALSE]
                                            ELSE dev
            /\ leak' = (leak \/ (Removed(gkey) /\ released > removedAt[gkey]))
            /\ UNCHANGED registered
    /\ UNCHANGED <<keyfile, gpc, gkey, again, restarts, removedKeys, released,
                   removedAt, rejoined>>

----------------------------------------------------------------------------
\* IT, on the console's Devices page, and its releases.

Remove ==
    /\ dev.key # "absent" /\ ~dev.revoked
    /\ dev' = [dev EXCEPT !.revoked = TRUE]
    /\ removedAt' = IF dev.key \in Keys THEN [removedAt EXCEPT ![dev.key] = released]
                                        ELSE removedAt
    /\ UNCHANGED <<keyfile, gpc, gkey, again, registered, req, have, restarts,
                   removedKeys, released, rejoined, leak>>

\* "so a computer can enroll under its id again (one set up again)".
Forget ==
    /\ dev.key # "absent" /\ dev.revoked
    /\ dev' = Absent
    /\ removedKeys' = IF Fixed /\ dev.key \in Keys THEN removedKeys \cup {dev.key}
                                                   ELSE removedKeys
    /\ UNCHANGED <<keyfile, gpc, gkey, again, registered, req, have, restarts,
                   released, removedAt, rejoined, leak>>

Release ==
    /\ released < MaxReleases
    /\ released' = released + 1
    /\ UNCHANGED <<keyfile, gpc, gkey, again, registered, req, have, restarts,
                   dev, removedKeys, removedAt, rejoined, leak>>

Next ==
    \/ KeyStart \/ KeyWrite \/ Restart \/ SetUpAgain
    \/ Register \/ SyncAsk \/ SyncAnswer
    \/ Remove \/ Forget \/ Release

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* What the OS promises.

\* ADR 0030 slice P: a computer IT removed stays removed: its key is never
\* registered again, forgotten or not. A computer set up again makes a new
\* key, and that one can enroll.
ADR0030_ARemovedComputerStaysRemoved == ~rejoined

\* DV-1: nothing IT releases after removing a computer reaches it.
DV1_NothingReleasedAfterRemovalReachesIt == ~leak

\* AD-4: the Devices page lists computers that registered a key, never an
\* entry a request made for an id IT forgot.
AD4_TheFleetIsComputersThatRegistered == dev.key # "none"

\* Enrollment survives glassd stopping at any point: no half-written key
\* leaves the computer unable to sync.
ADR0030_EnrollmentSurvivesARestart == gpc # "stuck"
=============================================================================
