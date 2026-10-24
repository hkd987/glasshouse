-------------------------------- MODULE Boot --------------------------------
(***************************************************************************)
(* From power on to a person's desktop (ADR 0019, ADR 0021, ADR 0031,     *)
(* ADR 0038, UX-2), on a computer no organization manages, through power  *)
(* cuts and glassd crashing at any step.                                   *)
(*                                                                         *)
(* The code:                                                               *)
(*  - os/freebsd/overlay/usr/local/etc/rc.d: `glassd` is started under     *)
(*    daemon(8) and its start returns at once; `glasshouse_greeter`       *)
(*    REQUIREs it, so the greeter starts once glassd's process has been    *)
(*    launched, not once it is ready. GlassdLaunch, GreeterStart.          *)
(*  - crates/bin/glassd/src/main.rs `prepare`: the policy from local disk  *)
(*    (crates/lib/policy/src/store.rs `load`: the file if it validates,    *)
(*    else the last good one, else it refuses to start and says why on the *)
(*    console) and the computer's owner (device.rs `owner`: setup.toml's   *)
(*    `owner`, else the first person). Prepare.                            *)
(*  - main.rs `run`: `sandbox.cleanup_stale()` (jails, mounts and limits   *)
(*    an earlier glassd left), then the runtime, `keep_seat_in` (the seat  *)
(*    kept in /var/run/glasshouse/seat comes back if its `su` still runs), *)
(*    then the sockets: ready. Cleanup, Ready. Then, off the start path,   *)
(*    `store.remember` records the policy in force as the last good one.   *)
(*    Remember.                                                            *)
(*  - shell/crates/glass-greeter/src/main.rs: first-run setup when nobody  *)
(*    can sign in, setup.toml isn't there and nobody manages the computer  *)
(*    (`first_run`); setup's account (crates/lib/platform/src/users.rs     *)
(*    `create_person`: the home's dataset, `pw useradd`, `chown`), then    *)
(*    `record_setup` writes setup.toml, and the person is signed in.       *)
(*    SetupSubmit, MakeDataset, AddAccount, Chown, Record. Otherwise the   *)
(*    sign-in screen; a sign-in starts `su -l` and tells glassd who is at  *)
(*    the screen (`seat.begin`); signing out tells it again (`seat.end`).  *)
(*    SignIn, TellBegin, SignOut, TellEnd. glassd takes the same session  *)
(*    told twice as one sign-in and refuses one whose `su` has ended       *)
(*    (crates/svc/runtime/src/seat.rs `seat_begin`), so the greeter's      *)
(*    tries, stopped when the session ends, never bring back a seat.       *)
(*                                                                         *)
(* Abstractions: one person, p; the policy file holds v1, v2 or something  *)
(* that doesn't validate (an edit by hand, IT's next version); a power cut *)
(* empties memory, the kernel's jails and /var/run, and keeps the disk;    *)
(* glassd crashing keeps jails and /var/run, and someone or something      *)
(* starts it again. Each step of `create_person` is one command.           *)
(*                                                                         *)
(* Old is the set of fixes switched off, the code before ADR 0053:         *)
(*  - "ownerAtStart": glassd read the owner only as it started. At first   *)
(*    boot that is before setup made them, so for their whole first        *)
(*    session the computer treated them as IT-managed.                     *)
(*  - "tellOnce": the greeter told glassd who signed in once, and gave up  *)
(*    if glassd didn't answer (starting, restarting, slow).                *)
(*  - "noJournal": setup made the account step by step with no record of  *)
(*    what it was making: a power cut between the steps left a home with   *)
(*    no account (that name could never be chosen again) or an account     *)
(*    whose home wasn't theirs (a desktop that couldn't save anything).    *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    MaxCuts,     \* power cuts
    MaxCrashes,  \* glassd crashing
    MaxEdits,    \* the policy file changed
    Old          \* fixes switched off

Policies == {"v1", "v2"}
Files == Policies \cup {"bad"}

VARIABLES
    \* On disk: survive power cuts and crashes.
    policyFile,  \* what policy.toml holds
    lastGood,    \* state/policy/last-good.toml, or "none"
    dataset,     \* p's home dataset exists
    account,     \* p's account exists (and can sign in)
    chowned,     \* p's home is theirs
    record,      \* setup.toml (owner = p)
    intent,      \* setup.toml.new: setup is making p (ADR 0053)
    \* Kept by the kernel or /var/run: survive glassd crashing, not a power cut.
    seatFile,    \* glassd's kept seat (p)
    jailsLeft,   \* jails, mounts and limits of an earlier glassd's tasks
    session,     \* p's desktop (`su -l`) runs
    rc,          \* the boot's rc scripts: "glassd", "greeter", "done"
    \* glassd's memory.
    gd,          \* "down", "starting", "prepared", "cleaned", "ready", "failed"
    inForce,     \* the policy it enforces, or "none"
    origin,      \* "file" or "lastgood"
    remembered,  \* this run recorded its policy
    owner,       \* who it takes for the owner, or "none"
    seat,        \* who it takes to be at the screen, or "none"
    tasks,       \* a task of this run's is running
    \* The greeter's memory.
    gr,          \* "down", "setup", "dataset", "useradd", "chown", "record",
                 \* "screen", "session", "ending"
    told,        \* glassd heard who signed in, this session
    gaveUp,      \* the greeter stopped telling (Old "tellOnce")
    \* History and bounds.
    everInForce, \* every policy any run enforced
    cuts, crashes, edits

vars == <<policyFile, lastGood, dataset, account, chowned, record, intent,
          seatFile, jailsLeft, session, rc, gd, inForce, origin, remembered,
          owner, seat, tasks, gr, told, gaveUp, everInForce, cuts, crashes,
          edits>>

disk == <<policyFile, lastGood, dataset, account, chowned, record, intent>>

Fixed(f) == f \notin Old

\* device.rs `owner`: setup.toml's owner, else the first person's account.
OwnerOnDisk == IF record \/ account THEN "p" ELSE "none"

\* The greeter's `first_run`: nobody can sign in and setup.toml isn't there.
FirstRun == ~account /\ ~record

Init ==
    /\ policyFile = "v1" /\ lastGood = "none"
    /\ dataset = FALSE /\ account = FALSE /\ chowned = FALSE
    /\ record = FALSE /\ intent = FALSE
    /\ seatFile = FALSE /\ jailsLeft = FALSE /\ session = FALSE
    /\ rc = "glassd"
    /\ gd = "down" /\ inForce = "none" /\ origin = "file"
    /\ remembered = FALSE /\ owner = "none" /\ seat = "none" /\ tasks = FALSE
    /\ gr = "down" /\ told = FALSE /\ gaveUp = FALSE
    /\ everInForce = {}
    /\ cuts = 0 /\ crashes = 0 /\ edits = 0

-----------------------------------------------------------------------------
(* rc: glassd's start launches it and returns; the greeter follows.        *)

GlassdLaunch ==
    /\ rc = "glassd" /\ gd = "down"
    /\ rc' = "greeter" /\ gd' = "starting"
    /\ UNCHANGED <<disk, seatFile, jailsLeft, session, inForce, origin,
                   remembered, owner, seat, tasks, gr, told, gaveUp,
                   everInForce, cuts, crashes, edits>>

\* The greeter starts: a setup a power cut interrupted is finished or undone
\* first (ADR 0053), then setup or the sign-in screen.
GreeterStart ==
    /\ rc = "greeter" /\ gr = "down"
    /\ rc' = "done"
    /\ IF Fixed("noJournal") /\ intent
       THEN IF account
            THEN \* useradd done: the home is made theirs, the record kept.
                 /\ chowned' = TRUE /\ record' = TRUE /\ intent' = FALSE
                 /\ UNCHANGED <<dataset, account>>
                 /\ gr' = "screen"
            ELSE \* No account: the home it made goes; setup asks again.
                 /\ dataset' = FALSE /\ intent' = FALSE
                 /\ UNCHANGED <<account, chowned, record>>
                 /\ gr' = IF ~record THEN "setup" ELSE "screen"
       ELSE /\ UNCHANGED <<dataset, account, chowned, record, intent>>
            /\ gr' = IF FirstRun THEN "setup" ELSE "screen"
    /\ UNCHANGED <<policyFile, lastGood, seatFile, jailsLeft, session, gd,
                   inForce, origin, remembered, owner, seat, tasks, told,
                   gaveUp, everInForce, cuts, crashes, edits>>

-----------------------------------------------------------------------------
(* glassd starting.                                                        *)

\* `prepare`: local disk only. A file that validates, else the last good
\* policy, else it refuses to start (and says why on the console).
Prepare ==
    /\ gd = "starting"
    /\ IF policyFile \in Policies
       THEN /\ inForce' = policyFile /\ origin' = "file" /\ gd' = "prepared"
       ELSE IF lastGood \in Policies
            THEN /\ inForce' = lastGood /\ origin' = "lastgood" /\ gd' = "prepared"
            ELSE /\ gd' = "failed" /\ UNCHANGED <<inForce, origin>>
    /\ owner' = OwnerOnDisk
    /\ UNCHANGED <<disk, seatFile, jailsLeft, session, rc, remembered, seat,
                   tasks, gr, told, gaveUp, everInForce, cuts, crashes, edits>>

\* `cleanup_stale`, before anything that can start a task.
Cleanup ==
    /\ gd = "prepared"
    /\ gd' = "cleaned" /\ jailsLeft' = FALSE
    /\ UNCHANGED <<disk, seatFile, session, rc, inForce, origin, remembered,
                   owner, seat, tasks, gr, told, gaveUp, everInForce, cuts,
                   crashes, edits>>

\* `keep_seat_in`, then the sockets: ready.
Ready ==
    /\ gd = "cleaned"
    /\ gd' = "ready"
    /\ IF seatFile /\ session
       THEN seat' = "p" /\ UNCHANGED seatFile
       ELSE seat' = "none" /\ seatFile' = FALSE
    /\ everInForce' = everInForce \cup {inForce}
    /\ UNCHANGED <<disk, jailsLeft, session, rc, inForce, origin, remembered,
                   owner, tasks, gr, told, gaveUp, cuts, crashes, edits>>

\* `store.remember`, off the start path: what's in force, unless it is the
\* last good one already standing in for a file that didn't validate.
Remember ==
    /\ gd = "ready" /\ ~remembered
    /\ remembered' = TRUE
    /\ lastGood' = IF origin = "file" THEN inForce ELSE lastGood
    /\ UNCHANGED <<policyFile, dataset, account, chowned, record, intent,
                   seatFile, jailsLeft, session, rc, gd, inForce, origin,
                   owner, seat, tasks, gr, told, gaveUp, everInForce, cuts,
                   crashes, edits>>

TaskStart ==
    /\ gd = "ready" /\ ~tasks
    /\ tasks' = TRUE
    /\ UNCHANGED <<disk, seatFile, jailsLeft, session, rc, gd, inForce,
                   origin, remembered, owner, seat, gr, told, gaveUp,
                   everInForce, cuts, crashes, edits>>

GlassdCrash ==
    /\ gd \in {"starting", "prepared", "cleaned", "ready"}
    /\ crashes < MaxCrashes
    /\ crashes' = crashes + 1
    /\ gd' = "down"
    /\ jailsLeft' = (jailsLeft \/ tasks) /\ tasks' = FALSE
    /\ inForce' = "none" /\ remembered' = FALSE /\ owner' = "none"
    /\ seat' = "none"
    /\ UNCHANGED <<disk, seatFile, session, rc, origin, gr, told, gaveUp,
                   everInForce, cuts, edits>>

\* Started again (by hand, or by its supervisor): once the boot's rc ran.
GlassdRestart ==
    /\ gd \in {"down", "failed"} /\ rc = "done"
    /\ gd' = "starting"
    /\ UNCHANGED <<disk, seatFile, jailsLeft, session, rc, inForce, origin,
                   remembered, owner, seat, tasks, gr, told, gaveUp,
                   everInForce, cuts, crashes, edits>>

\* Someone sees the computer's owner first appear (ADR 0053): read again
\* when the seat changes, while glassd knows of none.
OwnerAfterSeat(newSeat) ==
    IF Fixed("ownerAtStart") /\ owner = "none" THEN OwnerOnDisk ELSE owner

-----------------------------------------------------------------------------
(* First-run setup: `create_person`'s steps, then setup.toml.              *)

SetupSubmit ==
    /\ gr = "setup"
    /\ gr' = "dataset"
    /\ intent' = IF Fixed("noJournal") THEN TRUE ELSE intent
    /\ UNCHANGED <<policyFile, lastGood, dataset, account, chowned, record,
                   seatFile, jailsLeft, session, rc, gd, inForce, origin,
                   remembered, owner, seat, tasks, told, gaveUp, everInForce,
                   cuts, crashes, edits>>

\* `zfs create`: refused when the dataset is there already, and setup says
\* the account couldn't be made.
MakeDataset ==
    /\ gr = "dataset"
    /\ IF dataset
       THEN gr' = "setup" /\ UNCHANGED <<dataset, intent>>
       ELSE gr' = "useradd" /\ dataset' = TRUE /\ UNCHANGED intent
    /\ UNCHANGED <<policyFile, lastGood, account, chowned, record, seatFile,
                   jailsLeft, session, rc, gd, inForce, origin, remembered,
                   owner, seat, tasks, told, gaveUp, everInForce, cuts,
                   crashes, edits>>

AddAccount ==
    /\ gr = "useradd"
    /\ gr' = "chown" /\ account' = TRUE
    /\ UNCHANGED <<policyFile, lastGood, dataset, chowned, record, intent,
                   seatFile, jailsLeft, session, rc, gd, inForce, origin,
                   remembered, owner, seat, tasks, told, gaveUp, everInForce,
                   cuts, crashes, edits>>

Chown ==
    /\ gr = "chown"
    /\ gr' = "record" /\ chowned' = TRUE
    /\ UNCHANGED <<policyFile, lastGood, dataset, account, record, intent,
                   seatFile, jailsLeft, session, rc, gd, inForce, origin,
                   remembered, owner, seat, tasks, told, gaveUp, everInForce,
                   cuts, crashes, edits>>

\* setup.toml (by rename from the intent, ADR 0053), then p is signed in.
Record ==
    /\ gr = "record"
    /\ record' = TRUE /\ intent' = FALSE
    /\ gr' = "session" /\ session' = TRUE /\ told' = FALSE /\ gaveUp' = FALSE
    /\ UNCHANGED <<policyFile, lastGood, dataset, account, chowned, seatFile,
                   jailsLeft, rc, gd, inForce, origin, remembered, owner,
                   seat, tasks, everInForce, cuts, crashes, edits>>

-----------------------------------------------------------------------------
(* Signing in and out.                                                     *)

SignIn ==
    /\ gr = "screen" /\ account
    /\ gr' = "session" /\ session' = TRUE /\ told' = FALSE /\ gaveUp' = FALSE
    /\ UNCHANGED <<disk, seatFile, jailsLeft, rc, gd, inForce, origin,
                   remembered, owner, seat, tasks, everInForce, cuts,
                   crashes, edits>>

\* `seat.begin`: glassd keeps the seat; if it isn't there to answer, the
\* old greeter gave up, and the greeter now asks again until it hears.
TellBegin ==
    /\ gr = "session" /\ ~told /\ ~gaveUp
    /\ IF gd = "ready"
       THEN /\ told' = TRUE /\ seat' = "p" /\ seatFile' = TRUE
            /\ owner' = OwnerAfterSeat("p")
            /\ UNCHANGED gaveUp
       ELSE /\ ~Fixed("tellOnce")
            /\ gaveUp' = TRUE
            /\ UNCHANGED <<told, seat, seatFile, owner>>
    /\ UNCHANGED <<disk, jailsLeft, session, rc, gd, inForce, origin,
                   remembered, tasks, gr, everInForce, cuts, crashes, edits>>

SignOut ==
    /\ gr = "session"
    /\ gr' = "ending" /\ session' = FALSE
    /\ UNCHANGED <<disk, seatFile, jailsLeft, rc, gd, inForce, origin,
                   remembered, owner, seat, tasks, told, gaveUp, everInForce,
                   cuts, crashes, edits>>

\* `seat.end`, best effort: a glassd that isn't there finds the kept seat's
\* `su` gone when it starts.
TellEnd ==
    /\ gr = "ending"
    /\ gr' = "screen"
    /\ IF gd = "ready"
       THEN seat' = "none" /\ seatFile' = FALSE /\ owner' = OwnerAfterSeat("none")
       ELSE UNCHANGED <<seat, seatFile, owner>>
    /\ UNCHANGED <<disk, jailsLeft, session, rc, gd, inForce, origin,
                   remembered, tasks, told, gaveUp, everInForce, cuts,
                   crashes, edits>>

-----------------------------------------------------------------------------
(* What happens to the computer.                                           *)

PowerCut ==
    /\ cuts < MaxCuts
    /\ cuts' = cuts + 1
    /\ rc' = "glassd"
    /\ gd' = "down" /\ gr' = "down"
    /\ session' = FALSE /\ seatFile' = FALSE /\ jailsLeft' = FALSE
    /\ tasks' = FALSE /\ inForce' = "none" /\ remembered' = FALSE
    /\ owner' = "none" /\ seat' = "none" /\ told' = FALSE /\ gaveUp' = FALSE
    /\ UNCHANGED <<disk, origin, everInForce, crashes, edits>>

\* IT's next version, or an edit by hand that doesn't validate.
EditPolicy ==
    /\ edits < MaxEdits
    /\ edits' = edits + 1
    /\ \E f \in Files \ {policyFile} : policyFile' = f
    /\ UNCHANGED <<lastGood, dataset, account, chowned, record, intent,
                   seatFile, jailsLeft, session, rc, gd, inForce, origin,
                   remembered, owner, seat, tasks, gr, told, gaveUp,
                   everInForce, cuts, crashes>>

Next ==
    \/ GlassdLaunch \/ GreeterStart
    \/ Prepare \/ Cleanup \/ Ready \/ Remember \/ TaskStart
    \/ GlassdCrash \/ GlassdRestart
    \/ SetupSubmit \/ MakeDataset \/ AddAccount \/ Chown \/ Record
    \/ SignIn \/ TellBegin \/ SignOut \/ TellEnd
    \/ PowerCut \/ EditPolicy

\* What the code does by itself happens; a power cut, a crash, an edit,
\* starting a task and signing out are the world's, and may never come.
Fairness ==
    /\ WF_vars(GlassdLaunch) /\ WF_vars(GreeterStart)
    /\ WF_vars(Prepare) /\ WF_vars(Cleanup) /\ WF_vars(Ready)
    /\ WF_vars(Remember) /\ WF_vars(GlassdRestart)
    /\ WF_vars(SetupSubmit) /\ WF_vars(MakeDataset) /\ WF_vars(AddAccount)
    /\ WF_vars(Chown) /\ WF_vars(Record)
    /\ WF_vars(SignIn) /\ WF_vars(TellBegin) /\ WF_vars(TellEnd)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
TypeOK ==
    /\ policyFile \in Files /\ lastGood \in Policies \cup {"none"}
    /\ gd \in {"down", "starting", "prepared", "cleaned", "ready", "failed"}
    /\ inForce \in Policies \cup {"none"}
    /\ owner \in {"p", "none"} /\ seat \in {"p", "none"}
    /\ gr \in {"down", "setup", "dataset", "useradd", "chown", "record",
               "screen", "session", "ending"}

\* ADR 0021: glassd enforces only a policy that validated, and the last good
\* one only ever holds a policy some run enforced.
ADR0021_OnlyAValidPolicyInForce ==
    gd \in {"prepared", "cleaned", "ready"} => inForce \in Policies
ADR0021_LastGoodWasInForce ==
    lastGood \in everInForce \cup {"none"}
\* ADR 0021: it refuses to start only with no good copy on disk.
ADR0021_RefusesOnlyWithNoGoodCopy ==
    gd = "failed" => lastGood = "none"

\* PA-2: a task starts only once what an earlier glassd left is gone.
PA2_NothingLeftWhenATaskStarts ==
    tasks => ~jailsLeft

\* UX-2: whoever signs in has an account whose home is theirs, and setup
\* leaves no home without its account once it's done or given up.
UX2_WhoSignsInHasTheirHome ==
    session => (account /\ chowned)
UX2_NoHomeWithoutItsAccount ==
    gr \in {"screen", "session", "setup"} => (dataset => account)

\* ADR 0038: once the person setup made is at the screen, glassd knows them
\* for the computer's owner.
ADR0038_TheOwnerIsWhoSetItUp ==
    (gd = "ready" /\ seat = "p") => owner = "p"

\* Liveness. UX-2: setup finishes, whatever power cuts came.
UX2_SetupFinishes == <>record
\* ADR 0031: while someone stays signed in and glassd runs, glassd comes to
\* know who is at the screen.
ADR0031_GlassdLearnsWhoIsAtTheScreen ==
    (session /\ gd = "ready") ~> (seat = "p" \/ ~session \/ gd # "ready")
\* ADR 0021: the sign-in screen (or setup) always comes.
ADR0021_TheScreenComes ==
    <>(gr \in {"setup", "screen", "session"})
=============================================================================
