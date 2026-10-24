--------------------------- MODULE SettingsLayers ---------------------------
(***************************************************************************)
(* A person's device settings, as IT's groups, their own choices and the  *)
(* table's defaults combine (UX-1, ADR 0029, ADR 0031, ADR 0038), while   *)
(* the console sends a release, the person changes settings from their    *)
(* desktop and from the Settings app at once, and people sign in and out. *)
(*                                                                         *)
(* One person, P, in group G or not. Two of their settings, A and B; G    *)
(* can lock A. One setting of the computer's that follows whoever is      *)
(* signed in, USB drives: G locks them off, everyone else has them on.    *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/apps/settings/src/lib.rs: SettingsHandle::set_policy         *)
(*    (Release, then Notify), choose (Read: the lock check and the         *)
(*    choices read; Write: the file written whole, then notify(uid)),      *)
(*    person (what the desktop reads).                                     *)
(*  - crates/svc/runtime/src/runtime.rs: choose_setting (the desktop's     *)
(*    settings.set, outside any turn), apply_policy; the Settings app's    *)
(*    settings.set takes the person's writing turn (execute.rs), which    *)
(*    the desktop's call doesn't.                                          *)
(*  - crates/bin/glassd/src/device.rs: follow (Apply, one run at a time,   *)
(*    on notify(None) and on each sign-in and sign-out).                   *)
(*  - The desktop reads settings.get again on SettingsChanged (Refresh).   *)
(*                                                                         *)
(* Before ADR 0052 TLC found, against the code as it was:                  *)
(*  - LockChoices = FALSE: the desktop and the Settings app changing two   *)
(*    settings at once each read the choices, and the second write put    *)
(*    back the first one's setting as it was: a change lost.               *)
(*  - NotifyOnMembers = FALSE: a release that moved the person into or out *)
(*    of a group, its settings the same, told nobody. USB drives stayed   *)
(*    as the old group had them for whoever was signed in, and the        *)
(*    desktop showed a lock that wasn't there, or missed one that was.    *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS
  LockChoices,     \* the fix: choices change under the handle's lock
  NotifyOnMembers, \* the fix: set_policy compares groups' members too
  MaxCalls,        \* settings.set calls per writer
  MaxReleases,     \* releases from the console
  MaxSeat          \* sign-ins and sign-outs

Ids == {"A", "B"}
Values == {1, 2}       \* a choice; 0 is "none: the default"
Writers == {"desktop", "app"}
Policies == [member : BOOLEAN, lockA : BOOLEAN]
Locked(p, id) == id = "A" /\ p.member /\ p.lockA
Effective(p, f, id) == IF Locked(p, id) THEN 1 ELSE f[id]
View(p, f) == [id \in Ids |-> Effective(p, f, id)]
\* USB drives for whoever is signed in: off for G's members.
Usb(p, seat) == IF seat /\ p.member THEN "off" ELSE "on"
\* What set_policy compares to decide whether anyone's settings changed.
Compared(p) == IF NotifyOnMembers THEN <<p.member, p.lockA>> ELSE <<p.lockA>>

VARIABLES
  policy,     \* the handle's policy in force
  file,       \* P's choices file, by id (0: no choice)
  pc,         \* each writer: "idle" or "write"
  snap,       \* the choices each writer read
  want,       \* the id and value each writer is setting
  calls,      \* settings.set calls so far, per writer
  holder,     \* who holds the handle's lock ("none")
  notifying,  \* a release whose notify(None) hasn't run yet
  releases,   \* releases so far
  seat,       \* P is signed in at the screen
  seatRun,    \* a sign-in or sign-out whose follow run hasn't run yet
  seats,      \* sign-ins and sign-outs so far
  applied,    \* USB drives as glassd applied them
  events,     \* SettingsChanged events the desktop hasn't read yet
  shown,      \* the settings the desktop shows
  chosen      \* the last value each completed settings.set chose, by id

vars == <<policy, file, pc, snap, want, calls, holder, notifying, releases,
          seat, seatRun, seats, applied, events, shown, chosen>>

None == [id \in Ids |-> 0]

Init ==
  /\ policy = [member |-> FALSE, lockA |-> FALSE]
  /\ file = None
  /\ pc = [w \in Writers |-> "idle"]
  /\ snap = [w \in Writers |-> None]
  /\ want = [w \in Writers |-> <<"A", 1>>]
  /\ calls = [w \in Writers |-> 0]
  /\ holder = "none"
  /\ notifying = FALSE
  /\ releases = 0
  /\ seat = TRUE
  /\ seatRun = FALSE
  /\ seats = 0
  /\ applied = "on"
  /\ events = 0
  /\ shown = View([member |-> FALSE, lockA |-> FALSE], None)
  /\ chosen = None

(* choose, to the write: the lock check against the policy in force and  *)
(* the choices read. A locked setting is refused, and nothing changes.    *)
Read(w, id, v) ==
  /\ pc[w] = "idle"
  /\ calls[w] < MaxCalls
  /\ ~LockChoices \/ holder = "none"
  /\ calls' = [calls EXCEPT ![w] = @ + 1]
  /\ IF Locked(policy, id)
       THEN UNCHANGED <<pc, snap, want, holder>>
       ELSE /\ pc' = [pc EXCEPT ![w] = "write"]
            /\ snap' = [snap EXCEPT ![w] = file]
            /\ want' = [want EXCEPT ![w] = <<id, v>>]
            /\ holder' = IF LockChoices THEN w ELSE holder
  /\ UNCHANGED <<policy, file, notifying, releases, seat, seatRun, seats,
                 applied, events, shown, chosen>>

(* write_private of the choices read, with this one changed; then         *)
(* notify(Some(uid)): the desktop hears.                                  *)
Write(w) ==
  /\ pc[w] = "write"
  /\ file' = [snap[w] EXCEPT ![want[w][1]] = want[w][2]]
  /\ chosen' = [chosen EXCEPT ![want[w][1]] = want[w][2]]
  /\ pc' = [pc EXCEPT ![w] = "idle"]
  /\ snap' = [snap EXCEPT ![w] = None]
  /\ want' = [want EXCEPT ![w] = <<"A", 1>>]
  /\ holder' = IF holder = w THEN "none" ELSE holder
  /\ events' = events + 1
  /\ UNCHANGED <<policy, calls, notifying, releases, seat,
                 seatRun, seats, applied, shown>>

(* apply_policy -> set_policy: the slot written, and whether anyone's     *)
(* settings changed, as it compares them.                                 *)
Release(p) ==
  /\ releases < MaxReleases
  /\ ~notifying
  /\ p /= policy
  /\ releases' = releases + 1
  /\ policy' = p
  /\ notifying' = (Compared(p) /= Compared(policy))
  /\ UNCHANGED <<file, pc, snap, want, calls, holder, seat, seatRun, seats,
                 applied, events, shown, chosen>>

(* notify(None): the desktops' event, and follow's run, which applies    *)
(* what is in force for whoever is signed in now.                        *)
Notify ==
  /\ notifying
  /\ notifying' = FALSE
  /\ events' = events + 1
  /\ applied' = Usb(policy, seat)
  /\ UNCHANGED <<policy, file, pc, snap, want, calls, holder, releases,
                 seat, seatRun, seats, shown, chosen>>

(* seat.begin or seat.end: the seat's watch wakes follow.                *)
SignInOrOut ==
  /\ seats < MaxSeat
  /\ seats' = seats + 1
  /\ seat' = ~seat
  /\ seatRun' = TRUE
  /\ UNCHANGED <<policy, file, pc, snap, want, calls, holder, notifying,
                 releases, applied, events, shown, chosen>>

(* follow's run after the seat changed (one at a time; it reads the seat *)
(* and the policy as they are when it runs).                             *)
SeatApply ==
  /\ seatRun
  /\ seatRun' = FALSE
  /\ applied' = Usb(policy, seat)
  /\ UNCHANGED <<policy, file, pc, snap, want, calls, holder, notifying,
                 releases, seat, seats, events, shown, chosen>>

(* The desktop reads settings.get again on SettingsChanged.              *)
Refresh ==
  /\ events > 0
  /\ events' = events - 1
  /\ shown' = View(policy, file)
  /\ UNCHANGED <<policy, file, pc, snap, want, calls, holder, notifying,
                 releases, seat, seatRun, seats, applied, chosen>>

Next ==
  \/ \E w \in Writers, id \in Ids, v \in Values : Read(w, id, v)
  \/ \E w \in Writers : Write(w)
  \/ \E p \in Policies : Release(p)
  \/ Notify
  \/ SignInOrOut
  \/ SeatApply
  \/ Refresh

Spec == Init /\ [][Next]_vars

Idle == \A w \in Writers : pc[w] = "idle"

TypeOK ==
  /\ policy \in Policies
  /\ file \in [Ids -> {0} \cup Values]
  /\ pc \in [Writers -> {"idle", "write"}]
  /\ applied \in {"on", "off"}
  /\ events \in Nat

(* No settings.set that returned is undone by another: each setting holds *)
(* the last value chosen for it.                                          *)
UX1_NoChoiceLost == Idle => (\A id \in Ids : file[id] = chosen[id])

(* Once glassd has caught up, USB drives are as IT set them for whoever  *)
(* is signed in (ADR 0031).                                              *)
ADR0031_AppliedFollowsWhoIsSignedIn ==
  (~notifying /\ ~seatRun) => applied = Usb(policy, seat)

(* Once the desktop has read what it heard of, it shows what is in       *)
(* force: IT's lock where IT locked, the person's own choice elsewhere.  *)
UX1_DesktopShowsWhatIsInForce ==
  (Idle /\ ~notifying /\ events = 0) => shown = View(policy, file)
=============================================================================
