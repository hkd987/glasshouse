---------------------------- MODULE HardwarePatch ----------------------------
(***************************************************************************)
(* A kernel patch tried in a boot environment of its own (ADR 0057),       *)
(* through power cuts and crashes at any step, restarts the person makes,  *)
(* glassd restarting by itself, IT taking back its trust in the patch, and *)
(* a patch that works, panics as the computer starts, fails glassd's      *)
(* check, panics or hangs later, once the computer is in use, or hangs as *)
(* the computer wakes from sleep.                                          *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/svc/runtime/src/hwpatch.rs `apply`: the trial's record        *)
(*    ("staging") first, then the new environment (`bectl create`), the    *)
(*    modules written into it, the next start set to it once (`bectl       *)
(*    activate -t`), then the record "staged". BeginStage, CreateEnv,      *)
(*    WriteModules, ActivateOnce, MarkStaged.                              *)
(*  - The loader starts the environment set to start once and clears that  *)
(*    as it does; otherwise the permanent one. Start.                      *)
(*  - A patch that panics restarts the computer at once (the image sets    *)
(*    debug.debugger_on_panic=0). Panic, LatePanic.                        *)
(*  - `at_start`, as glassd starts: it reads the record, which environment *)
(*    runs and whether the trial is still set to start once. On the        *)
(*    trial's environment it checks the patch and puts it on probation;    *)
(*    on the one before, it says the trial rolled back, or finishes        *)
(*    staging that a crash cut short. GlassdStart.                         *)
(*  - Probation: the patch's environment is not the permanent one until    *)
(*    the computer has been in use on it for a while (`tick`). The next    *)
(*    start is set to it, once, only as the computer shuts down cleanly    *)
(*    (`at_shutdown`, from rc.d's shutdown through `glassctl system        *)
(*    shutdown`), so a crash or a power cut on probation starts the one    *)
(*    before. Shutdown, Tick, Promote (`bectl activate`), Confirm.         *)
(*  - Probation also counts the times the computer woke from sleep on the  *)
(*    patch (`resumed`, from rc.resume's `glassctl system resume`): one     *)
(*    that breaks waking is never kept. Sleep, Wake.                       *)
(*  - The hardware watchdog (`watchdogd`, ichwd) is armed while the        *)
(*    computer runs a patch not yet kept, from glassd's start check until  *)
(*    it's kept: a hang restarts the computer, on the environment before.  *)
(*    It doesn't run while asleep, and may not be armed again as the      *)
(*    computer wakes. Through a shutdown it stays armed (`watchdogd -x`),  *)
(*    and the start after a reset it made fails the patch: the kernel      *)
(*    says so (ichwd: "resuming after hardware watchdog timeout"). Hang,   *)
(*    WatchdogReset; the person's PowerOff.                                *)
(*  - The check again, with the keys and the community setting in force    *)
(*    now: a patch IT no longer allows fails. FailOnProbation.             *)
(*  - `cancel`: the person takes the trial back before restarting, or on   *)
(*    probation. Cancel, TakeBack.                                         *)
(*  - `undo`: the person takes a kept patch out: the environment before is *)
(*    the permanent one again (UndoActivate), then the record (UndoEnd).   *)
(*  - The environment of a trial that ended without being kept is removed  *)
(*    once the computer isn't running from it. Remove.                     *)
(*                                                                         *)
(* glassd's state, the record and the audit log, is on a dataset of its    *)
(* own, outside the environments (post-install.sh).                        *)
(*                                                                         *)
(* Old switches, each a version that was wrong:                           *)
(*  - "stateInEnv": glassd's state inside the environments, as it was:     *)
(*    what glassd records on the running environment after the new one is *)
(*    made isn't in the new one, so making that one permanent loses it     *)
(*    (AU-1).                                                              *)
(*  - "activateFirst": the start-once set before the modules are all       *)
(*    written. "permanent": `bectl activate` for the trial, so a patch     *)
(*    that panics starts the computer into a panic every time.             *)
(*  - "noProbation": the patch kept at the first start that passes the     *)
(*    check, as slice 1's first code did: one that panics later (at the    *)
(*    first sleep) is then what every start uses, and the computer panics  *)
(*    again after every start.                                             *)
(*  - "trustOnceAtApply": the keys and the community setting checked only  *)
(*    when the patch is set up: IT turning them off before the restart     *)
(*    doesn't stop it being kept.                                          *)
(*  - "armAtStart", "armAtGlassdStop": probation with the start-once set   *)
(*    at every start, or whenever glassd stops: a crash then starts the    *)
(*    patch again.                                                         *)
(*  - "minutesOnly": probation counts minutes of use only, as slice 1's    *)
(*    did: a patch that hangs every time the computer wakes is kept by a  *)
(*    person who didn't close the lid that day.                            *)
(*  - "noWatchdog": no hardware watchdog: a patch that hangs leaves the    *)
(*    computer hung until the person holds the power button.               *)
(*  - "watchdogOffAtShutdown": the watchdog disarmed as the computer shuts *)
(*    down: a patch that hangs the shutdown leaves it hung.                *)
(*  - "resetUnheard": the start after a watchdog reset not told from any   *)
(*    other: a patch that hangs every shutdown, its next start already set *)
(*    to it, carries on on probation and is kept.                          *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    MaxTrials,     \* patches tried, one after another
    MaxRestarts,   \* power cuts, crashes, restarts the person makes, glassd's own
    MaxRecords,    \* audit records agents write meanwhile
    Probation,     \* ticks of use on the patch before it's kept
    Resumes,       \* wakes from sleep on the patch before it's kept
    MaxSleeps,     \* times the person closes the lid, beyond those probation needs
    Old

Envs == {"o", "t"}   \* the environment the computer ran from, the trial's
Records == {"none", "staging", "staged", "probation", "confirmed", "failed",
            "rolledback", "cancelled", "undone"}
Ended == {"confirmed", "failed", "rolledback", "cancelled", "undone"}
\* A patch: good, panics as the computer starts, fails glassd's check,
\* panics or hangs once in use, or hangs as the computer wakes.
Qualities == {"good", "panics", "unhealthy", "panicsLater", "hangsLater", "hangsOnWake"}

VARIABLES
    running,    \* "off", or the environment the computer runs from
    started,    \* glassd's start check ran since glassd started
    closing,    \* the computer is shutting down cleanly
    env,        \* the trial environment: "none", "partial", "complete"
    once,       \* the next start is the trial's, once
    perm,       \* the environment every start uses otherwise
    record,     \* the trial's record, where glassd keeps it
    recordT,    \* with "stateInEnv": the copy inside the trial environment
    step,       \* glassd's `apply` or `undo` under way: "idle", "begun",
                \* "made", "written", "activated", "undoing"
    quality,    \* "good", "panics" (as it starts), "unhealthy" (fails the
                \* check), "panicsLater" (once in use)
    allowed,    \* the keys and the community setting in force allow it
    ran,        \* ticks the computer was in use on it
    trials,
    restarts,
    records,    \* audit records agents wrote
    onlyInO,    \* with "stateInEnv": a record written on o after t was made
    lost,       \* a record that was written is gone
    earlyCrash, \* the computer stopped while in use on the patch, before
                \* its probation's worth of use, not shutting down
    keptRevoked,\* the patch was made permanent while IT didn't allow it
    asleep,     \* the computer is asleep
    woke,       \* wakes from sleep on the patch on probation
    hung,       \* the computer is hung: nothing runs, not even glassd
    watchdog,   \* the hardware watchdog is armed
    sleeps,
    fired,      \* this start came from a watchdog reset (the kernel says so)
    wdOnTrial   \* the watchdog restarted the computer on the patch before
                \* its probation was served

power == <<asleep, woke, hung, watchdog, sleeps, fired, wdOnTrial>>

vars == <<running, started, closing, env, once, perm, record, recordT, step,
          quality, allowed, ran, trials, restarts, records, onlyInO, lost,
          earlyCrash, keptRevoked, asleep, woke, hung, watchdog, sleeps, fired,
          wdOnTrial>>

TypeOK ==
    /\ running \in {"off"} \cup Envs
    /\ started \in BOOLEAN /\ closing \in BOOLEAN
    /\ env \in {"none", "partial", "complete"}
    /\ once \in BOOLEAN
    /\ perm \in Envs
    /\ record \in Records /\ recordT \in Records
    /\ step \in {"idle", "begun", "made", "written", "activated", "undoing"}
    /\ quality \in Qualities
    /\ allowed \in BOOLEAN
    /\ ran \in 0..Probation
    /\ trials \in 0..MaxTrials
    /\ restarts \in 0..MaxRestarts
    /\ records \in 0..MaxRecords
    /\ onlyInO \in BOOLEAN /\ lost \in BOOLEAN
    /\ earlyCrash \in BOOLEAN /\ keptRevoked \in BOOLEAN
    /\ asleep \in BOOLEAN /\ woke \in 0..Resumes
    /\ hung \in BOOLEAN /\ watchdog \in BOOLEAN /\ sleeps \in 0..MaxSleeps
    /\ fired \in BOOLEAN /\ wdOnTrial \in BOOLEAN

Init ==
    /\ running = "o" /\ started = TRUE /\ closing = FALSE
    /\ env = "none" /\ once = FALSE /\ perm = "o"
    /\ record = "none" /\ recordT = "none" /\ step = "idle"
    /\ quality = "good" /\ allowed = TRUE /\ ran = 0 /\ trials = 0
    /\ restarts = 0 /\ records = 0 /\ onlyInO = FALSE /\ lost = FALSE
    /\ earlyCrash = FALSE /\ keptRevoked = FALSE
    /\ asleep = FALSE /\ woke = 0 /\ hung = FALSE /\ watchdog = FALSE /\ sleeps = 0
    /\ fired = FALSE /\ wdOnTrial = FALSE

Probing == "noProbation" \notin Old
\* What the check takes into account: the keys and setting now in force.
Trusted == allowed \/ "trustOnceAtApply" \in Old

\* The probation's use: minutes, and wakes from sleep.
Served == ran = Probation /\ (woke = Resumes \/ "minutesOnly" \in Old)

\* Awake and not hung: glassd and the person can act.
Awake == running # "off" /\ ~asleep /\ ~hung

\* The record glassd reads where it runs.
Seen == IF "stateInEnv" \in Old /\ running = "t" THEN recordT ELSE record

\* glassd writes the record where it runs; outside the environments there
\* is one copy, which every environment sees.
Write(r) ==
    IF "stateInEnv" \in Old /\ running = "t"
    THEN /\ recordT' = r /\ UNCHANGED record
    ELSE IF "stateInEnv" \in Old
         THEN /\ record' = r /\ UNCHANGED recordT
         ELSE /\ record' = r /\ recordT' = r

\* An audit record glassd writes: with its state inside the environments,
\* one written on o once t exists isn't in t.
Audit ==
    onlyInO' = (onlyInO \/ ("stateInEnv" \in Old /\ running = "o" /\ env # "none"))

\* The trial's environment made permanent: with glassd's state inside the
\* environments, what was written on o since t was made is gone.
MakePermanent ==
    /\ perm' = "t"
    /\ lost' = (lost \/ onlyInO)
    /\ keptRevoked' = (keptRevoked \/ ~allowed)

\* The computer stops, as it crashes or loses power or panics, or the
\* watchdog restarts it (`byWatchdog`): the watchdog goes with it.
DownBy(byWatchdog) ==
    /\ running' = "off" /\ started' = FALSE /\ closing' = FALSE /\ step' = "idle"
    /\ earlyCrash' = (earlyCrash \/ (running = "t" /\ ~Served /\ ~closing))
    /\ asleep' = FALSE /\ hung' = FALSE /\ watchdog' = FALSE /\ UNCHANGED <<woke, sleeps>>
    /\ fired' = byWatchdog
    /\ wdOnTrial' = (wdOnTrial \/ (byWatchdog /\ running = "t" /\ ~Served))

Down == DownBy(FALSE)

-----------------------------------------------------------------------------
\* `apply`, step by step, on the running environment, with no trial open.
\* It refuses a patch the keys or the community setting don't allow.
BeginStage ==
    /\ running = "o" /\ Awake /\ started /\ ~closing /\ step = "idle" /\ trials < MaxTrials
    /\ Seen \in {"none", "failed", "rolledback", "cancelled", "undone"}
    /\ perm = "o" /\ env = "none"
    /\ Write("staging")
    /\ step' = "begun"
    /\ quality' \in Qualities
    /\ allowed' = TRUE /\ ran' = 0 /\ earlyCrash' = FALSE
    /\ woke' = 0 /\ wdOnTrial' = FALSE /\ UNCHANGED <<asleep, hung, watchdog, sleeps, fired>>
    /\ trials' = trials + 1
    /\ Audit
    /\ UNCHANGED <<running, started, closing, env, once, perm, restarts, records,
                   lost, keptRevoked>>

CreateEnv ==
    /\ running = "o" /\ Awake /\ step = "begun"
    /\ UNCHANGED power
    /\ env' = "partial"
    \* The copy inside t is the record as it was when t was made.
    /\ recordT' = record
    /\ step' = "made"
    /\ UNCHANGED <<running, started, closing, once, perm, record, quality, allowed,
                   ran, trials, restarts, records, onlyInO, lost, earlyCrash,
                   keptRevoked>>

WriteModules ==
    /\ running = "o" /\ Awake /\ UNCHANGED power
    /\ IF "activateFirst" \in Old THEN step = "activated" ELSE step = "made"
    /\ env' = "complete"
    /\ step' = "written"
    /\ UNCHANGED <<running, started, closing, once, perm, record, recordT, quality,
                   allowed, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked>>

ActivateOnce ==
    /\ running = "o" /\ Awake /\ UNCHANGED power
    /\ IF "activateFirst" \in Old THEN step = "made" ELSE step = "written"
    /\ IF "permanent" \in Old
       THEN /\ perm' = "t" /\ UNCHANGED once
       ELSE /\ once' = TRUE /\ UNCHANGED perm
    /\ step' = "activated"
    /\ UNCHANGED <<running, started, closing, env, record, recordT, quality, allowed,
                   ran, trials, restarts, records, onlyInO, lost, earlyCrash,
                   keptRevoked>>

MarkStaged ==
    /\ running = "o" /\ Awake /\ UNCHANGED power /\ step = IF "activateFirst" \in Old THEN "written" ELSE "activated"
    /\ Write("staged")
    /\ step' = "idle"
    /\ Audit
    /\ UNCHANGED <<running, started, closing, env, once, perm, quality, allowed, ran,
                   trials, restarts, records, lost, earlyCrash, keptRevoked>>

\* The person takes the trial back before restarting.
Cancel ==
    /\ running = "o" /\ Awake /\ UNCHANGED power /\ started /\ ~closing /\ step = "idle" /\ Seen = "staged" /\ once
    /\ once' = FALSE
    /\ Write("cancelled")
    /\ Audit
    /\ UNCHANGED <<running, started, closing, env, perm, step, quality, allowed, ran,
                   trials, restarts, records, lost, earlyCrash, keptRevoked>>

\* Agents keep working, and glassd keeps writing their records.
AgentRecord ==
    /\ Awake /\ records < MaxRecords /\ UNCHANGED power
    /\ records' = records + 1
    /\ Audit
    /\ UNCHANGED <<running, started, closing, env, once, perm, record, recordT, step,
                   quality, allowed, ran, trials, restarts, lost, earlyCrash,
                   keptRevoked>>

\* IT stops trusting the patch's key, or stops allowing community patches.
Revoke ==
    /\ allowed /\ record # "none" /\ UNCHANGED power
    /\ allowed' = FALSE
    /\ UNCHANGED <<running, started, closing, env, once, perm, record, recordT, step,
                   quality, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked>>

-----------------------------------------------------------------------------
\* A power cut or a crash, at any step: memory is lost, the disk is kept.
Stop ==
    /\ running # "off" /\ restarts < MaxRestarts
    /\ Down
    /\ restarts' = restarts + 1
    /\ UNCHANGED <<env, once, perm, record, recordT, quality, allowed, ran, trials,
                   records, onlyInO, lost, keptRevoked>>

\* The person restarts or shuts down. rc.d stops glassd as the computer
\* shuts down (`glassctl system shutdown`): on probation, glassd sets the
\* next start to the patch's environment, once.
Shutdown ==
    /\ Awake /\ started /\ ~closing /\ step = "idle"
    /\ restarts < MaxRestarts
    /\ closing' = TRUE
    \* Armed through the rest of the shutdown (`watchdogd -x`): a patch
    \* can hang it too.
    /\ watchdog' = (watchdog /\ "watchdogOffAtShutdown" \notin Old)
    /\ UNCHANGED <<asleep, woke, hung, sleeps, fired, wdOnTrial>>
    /\ IF running = "t" /\ Seen = "probation" /\ Probing THEN once' = TRUE
       ELSE UNCHANGED once
    /\ restarts' = restarts + 1
    /\ UNCHANGED <<running, started, env, perm, record, recordT, step, quality,
                   allowed, ran, trials, records, onlyInO, lost, earlyCrash,
                   keptRevoked>>

\* The person restarts to try a staged patch, when glassd tells them to.
TryIt ==
    /\ running = "o" /\ Awake /\ UNCHANGED power /\ started /\ ~closing /\ step = "idle" /\ Seen = "staged"
    /\ closing' = TRUE
    /\ UNCHANGED <<running, started, env, once, perm, record, recordT, step, quality,
                   allowed, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked>>

\* The rest of the shutdown, then off.
Off ==
    /\ Awake /\ closing
    /\ Down
    /\ UNCHANGED <<env, once, perm, record, recordT, quality, allowed, ran, trials,
                   restarts, records, onlyInO, lost, keptRevoked>>

\* glassd alone stops, crashing or stopped by hand, and daemon(8) or the
\* person starts it again; the computer keeps running. With
\* "armAtGlassdStop", glassd sets the next start to the patch as it stops.
GlassdRestart ==
    /\ Awake /\ UNCHANGED power /\ started /\ ~closing /\ restarts < MaxRestarts
    /\ started' = FALSE /\ step' = "idle"
    /\ IF "armAtGlassdStop" \in Old /\ running = "t" /\ Seen = "probation"
       THEN once' = TRUE ELSE UNCHANGED once
    /\ restarts' = restarts + 1
    /\ UNCHANGED <<running, closing, env, perm, record, recordT, quality, allowed,
                   ran, trials, records, onlyInO, lost, earlyCrash, keptRevoked>>

\* The loader: the environment set to start once, clearing that, else the
\* permanent one.
Start ==
    /\ running = "off" /\ UNCHANGED power
    /\ IF once
       THEN /\ running' = "t" /\ once' = FALSE
       ELSE /\ running' = perm /\ UNCHANGED once
    /\ UNCHANGED <<started, closing, env, perm, record, recordT, step, quality,
                   allowed, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked>>

\* The patch's module panics as the trial starts: the computer restarts at
\* once. Not counted against MaxRestarts: it is the patch's, not anyone's.
Panic ==
    /\ running = "t" /\ ~started /\ quality = "panics" /\ env # "none"
    /\ Down
    /\ UNCHANGED <<env, once, perm, record, recordT, quality, allowed, ran, trials,
                   restarts, records, onlyInO, lost, keptRevoked>>

\* The patch panics once the computer is in use on it, at any time.
LatePanic ==
    /\ running = "t" /\ Awake /\ started /\ quality = "panicsLater"
    /\ Down
    /\ UNCHANGED <<env, once, perm, record, recordT, quality, allowed, ran, trials,
                   restarts, records, onlyInO, lost, keptRevoked>>

\* The computer in use on the patch for a while (`tick`).
Tick ==
    /\ running = "t" /\ Awake /\ UNCHANGED power /\ started /\ ~closing /\ ran < Probation
    /\ Seen \in {"probation", "confirmed"}
    /\ ran' = ran + 1
    /\ UNCHANGED <<running, started, closing, env, once, perm, record, recordT, step,
                   quality, allowed, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked>>

\* glassd's check as it starts.
GlassdStart ==
    /\ Awake /\ ~started
    /\ ~(running = "t" /\ quality = "panics")
    /\ started' = TRUE
    /\ LET r == Seen
           passes == quality \notin {"panics", "unhealthy"} /\ env = "complete" /\ Trusted
       IN
       \/ \* Running from the trial, staged or (a crash after the start-once
          \* was set) still staging: check the patch.
          /\ running = "t" /\ r \in {"staging", "staged"}
          /\ IF passes /\ ~Probing
             THEN /\ MakePermanent /\ Write("confirmed")
                  /\ UNCHANGED once
             ELSE IF passes
             THEN /\ Write("probation")
                  /\ once' = ("armAtStart" \in Old)
                  /\ UNCHANGED <<perm, lost, keptRevoked>>
             ELSE /\ Write("failed")
                  /\ UNCHANGED <<once, perm, lost, keptRevoked>>
          /\ Audit
       \/ \* On probation, started by the watchdog: the patch hung (a
          \* shutdown it hung had set this start to it already).
          /\ running = "t" /\ r = "probation" /\ perm = "o"
          /\ fired /\ "resetUnheard" \notin Old
          /\ Write("failed")
          /\ once' = FALSE
          /\ Audit
          /\ UNCHANGED <<perm, lost, keptRevoked>>
       \/ \* On probation, started again (a clean restart, or glassd's own):
          \* the start-once is cleared, so a crash from here goes back.
          /\ running = "t" /\ r = "probation" /\ perm = "o"
          /\ ~(fired /\ "resetUnheard" \notin Old)
          /\ once' = ("armAtStart" \in Old)
          /\ UNCHANGED <<perm, record, recordT, lost, onlyInO, keptRevoked>>
       \/ \* Made permanent, and a crash before the record said so: it was
          \* kept.
          /\ running = "t" /\ r = "probation" /\ perm = "t"
          /\ Write("confirmed")
          /\ Audit
          /\ UNCHANGED <<once, perm, lost, keptRevoked>>
       \/ \* On the environment it came from, the trial still to come.
          /\ running = "o" /\ r \in {"staging", "staged"} /\ once
          /\ Write("staged")
          /\ UNCHANGED <<once, perm, lost, onlyInO, keptRevoked>>
       \/ \* On the environment it came from, the trial's start over: it
          \* didn't come back, staging never finished, or it stopped on
          \* probation (a crash, a panic or a power cut).
          /\ running = "o" /\ r \in {"staging", "staged", "probation"} /\ ~once
          /\ Write("rolledback")
          /\ Audit
          /\ UNCHANGED <<once, perm, lost, keptRevoked>>
       \/ \* Back on the environment before a kept patch, which isn't the
          \* permanent one any more: an undo a crash cut short.
          /\ running = "o" /\ r = "confirmed" /\ perm = "o"
          /\ Write("undone")
          /\ Audit
          /\ UNCHANGED <<once, perm, lost, keptRevoked>>
       \/ /\ \/ r \in {"none", "failed", "rolledback", "cancelled", "undone"}
             \/ r = "confirmed" /\ ~(running = "o" /\ perm = "o")
             \/ r = "probation" /\ running = "o" /\ once
          /\ UNCHANGED <<once, perm, record, recordT, lost, onlyInO, keptRevoked>>
    \* Running a patch not yet kept: the watchdog, from now until it's kept.
    /\ watchdog' = (running = "t" /\ perm' = "o" /\ "noWatchdog" \notin Old)
    \* What the kernel said about this start is read once.
    /\ fired' = FALSE
    /\ UNCHANGED <<asleep, woke, hung, sleeps, wdOnTrial>>
    /\ UNCHANGED <<running, closing, env, step, quality, allowed, ran, trials,
                   restarts, records, earlyCrash>>

\* Probation served: the patch's environment is the permanent one (`bectl
\* activate`), then the record says so; a crash in between and the next
\* start, from that environment, confirms it.
Promote ==
    /\ running = "t" /\ Awake /\ UNCHANGED power /\ started /\ ~closing /\ step = "idle"
    /\ Probing /\ Seen = "probation" /\ Served /\ perm = "o" /\ Trusted
    /\ MakePermanent
    /\ UNCHANGED <<running, started, closing, env, once, record, recordT, step,
                   quality, allowed, ran, trials, restarts, records, onlyInO,
                   earlyCrash>>

Confirm ==
    /\ running = "t" /\ Awake /\ started /\ Seen = "probation" /\ perm = "t"
    \* Kept: the watchdog goes.
    /\ watchdog' = FALSE /\ UNCHANGED <<asleep, woke, hung, sleeps, fired, wdOnTrial>>
    /\ Write("confirmed")
    /\ Audit
    /\ UNCHANGED <<running, started, closing, env, once, perm, step, quality, allowed,
                   ran, trials, restarts, records, lost, earlyCrash, keptRevoked>>

\* On probation, the patch no longer allowed: it fails, and the next start
\* is the one before.
FailOnProbation ==
    /\ running = "t" /\ Awake /\ UNCHANGED power /\ started /\ Seen = "probation" /\ ~Trusted
    /\ Write("failed")
    /\ perm' = "o" /\ once' = FALSE
    /\ Audit
    /\ UNCHANGED <<running, started, closing, env, step, quality, allowed, ran,
                   trials, restarts, records, lost, earlyCrash, keptRevoked>>

\* The person takes the patch back on probation.
TakeBack ==
    /\ running = "t" /\ Awake /\ UNCHANGED power /\ started /\ ~closing /\ step = "idle"
    /\ Seen = "probation" /\ perm = "o"
    /\ Write("cancelled")
    /\ once' = FALSE
    /\ Audit
    /\ UNCHANGED <<running, started, closing, env, perm, step, quality, allowed, ran,
                   trials, restarts, records, lost, earlyCrash, keptRevoked>>

\* The person takes a kept patch out: the environment before is the
\* permanent one again, then the record says so.
UndoActivate ==
    /\ running = "t" /\ Awake /\ started /\ ~closing /\ step = "idle"
    /\ Seen = "confirmed" /\ perm = "t"
    /\ perm' = "o" /\ step' = "undoing"
    \* Taken out, but still what runs until the next start: the watchdog.
    /\ watchdog' = ("noWatchdog" \notin Old)
    /\ UNCHANGED <<asleep, woke, hung, sleeps, fired, wdOnTrial>>
    /\ UNCHANGED <<running, started, closing, env, once, record, recordT, quality,
                   allowed, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked>>

UndoEnd ==
    /\ step = "undoing" /\ Awake /\ UNCHANGED power
    /\ Write("undone")
    /\ step' = "idle"
    /\ Audit
    /\ UNCHANGED <<running, started, closing, env, once, perm, quality, allowed, ran,
                   trials, restarts, records, lost, earlyCrash, keptRevoked>>

\* The environment of a trial that ended without being kept is removed once
\* nothing runs from it or will start it.
Remove ==
    /\ env # "none" /\ running = "o" /\ Awake /\ UNCHANGED power /\ started /\ step = "idle"
    /\ perm = "o" /\ ~once
    /\ Seen \in {"failed", "rolledback", "cancelled", "undone"}
    /\ env' = "none"
    /\ UNCHANGED <<running, started, closing, once, perm, record, recordT, step,
                   quality, allowed, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked>>

\* Probation's minutes are served and it waits on a wake.
NeedsWake ==
    running = "t" /\ Seen = "probation" /\ ran = Probation /\ woke < Resumes

\* The person closes the lid; glassd's work waits (PM-5).
Sleep ==
    /\ Awake /\ started /\ ~closing /\ step = "idle"
    /\ NeedsWake \/ sleeps < MaxSleeps
    /\ asleep' = TRUE
    /\ sleeps' = IF NeedsWake THEN sleeps ELSE sleeps + 1
    /\ UNCHANGED <<running, started, closing, env, once, perm, record, recordT, step,
                   quality, allowed, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked, woke, hung, watchdog, fired, wdOnTrial>>

\* The computer wakes. A patch that breaks waking hangs it, the watchdog
\* not armed again; otherwise glassd counts the wake on probation
\* (`resumed`).
Wake ==
    /\ running # "off" /\ asleep /\ ~hung
    /\ asleep' = FALSE
    /\ IF running = "t" /\ quality = "hangsOnWake"
       THEN /\ hung' = TRUE /\ watchdog' = FALSE /\ UNCHANGED <<woke, sleeps>>
       ELSE /\ woke' = IF running = "t" /\ Seen = "probation" /\ woke < Resumes
                       THEN woke + 1 ELSE woke
            /\ UNCHANGED <<hung, watchdog, sleeps>>
    /\ UNCHANGED <<fired, wdOnTrial>>
    /\ UNCHANGED <<running, started, closing, env, once, perm, record, recordT, step,
                   quality, allowed, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked>>

\* The patch hangs the computer once it's in use.
Hang ==
    /\ running = "t" /\ Awake /\ started /\ quality = "hangsLater"
    /\ hung' = TRUE
    /\ UNCHANGED <<running, started, closing, env, once, perm, record, recordT, step,
                   quality, allowed, ran, trials, restarts, records, onlyInO, lost,
                   earlyCrash, keptRevoked, asleep, woke, watchdog, sleeps, fired,
                   wdOnTrial>>

\* Hung with the watchdog armed: nothing pats it, and it restarts the
\* computer.
WatchdogReset ==
    /\ hung /\ watchdog
    /\ DownBy(TRUE)
    /\ UNCHANGED <<env, once, perm, record, recordT, quality, allowed, ran, trials,
                   restarts, records, onlyInO, lost, keptRevoked>>

\* Hung, and the person holds the power button.
PowerOff ==
    /\ hung
    /\ Down
    /\ UNCHANGED <<env, once, perm, record, recordT, quality, allowed, ran, trials,
                   restarts, records, onlyInO, lost, keptRevoked>>

Next ==
    \/ BeginStage \/ CreateEnv \/ WriteModules \/ ActivateOnce \/ MarkStaged
    \/ Cancel \/ AgentRecord \/ Revoke
    \/ Stop \/ Shutdown \/ TryIt \/ Off \/ GlassdRestart \/ Start \/ Panic
    \/ LatePanic \/ Tick \/ GlassdStart \/ Promote \/ Confirm \/ FailOnProbation
    \/ TakeBack \/ UndoActivate \/ UndoEnd \/ Remove
    \/ Sleep \/ Wake \/ Hang \/ WatchdogReset \/ PowerOff

\* The person restarts to try the patch (once it's staged), uses the
\* computer and now and then closes the lid; it starts, glassd checks, a
\* panic restarts it, it wakes, and the watchdog restarts it when hung.
\* The person's power button isn't assumed: a hang must end by itself.
Spec == Init /\ [][Next]_vars
        /\ WF_vars(Start) /\ WF_vars(Panic) /\ WF_vars(GlassdStart)
        /\ WF_vars(CreateEnv) /\ WF_vars(WriteModules) /\ WF_vars(ActivateOnce)
        /\ WF_vars(MarkStaged) /\ WF_vars(TryIt) /\ WF_vars(Off)
        /\ WF_vars(Tick) /\ WF_vars(Promote) /\ WF_vars(Confirm)
        /\ WF_vars(FailOnProbation) /\ WF_vars(UndoEnd) /\ WF_vars(Remove)
        /\ WF_vars(Wake) /\ WF_vars(WatchdogReset)
        \* Now and then the lid closes; a computer hung waking is powered
        \* off by the person, since the watchdog can't (it isn't armed
        \* again as the computer wakes).
        /\ WF_vars(Sleep /\ NeedsWake)
        /\ WF_vars(PowerOff /\ quality = "hangsOnWake")

-----------------------------------------------------------------------------
\* The environment every start uses starts: o, or a trial that passed
\* glassd's check (one that panics later still starts).
ADR0057_TheComputerAlwaysStarts ==
    perm = "o" \/ (env = "complete" /\ quality \notin {"panics", "unhealthy"})

\* A trial becomes permanent only once the computer was in use on it for
\* its probation, and woke on it as many times.
ADR0057_KeptOnlyAfterItsProbation ==
    perm = "t" => (ran = Probation /\ woke = Resumes)

\* A patch the watchdog had to restart the computer from, before its
\* probation was served, is never kept.
ADR0057_APatchTheWatchdogResetIsNeverKept ==
    perm = "t" => ~wdOnTrial

\* A patch that hangs the computer as it wakes is never what every start
\* uses.
ADR0057_NoPatchKeptThatBreaksWaking ==
    perm = "t" => quality # "hangsOnWake"

\* Running a patch not yet kept, glassd's start check done and awake (a
\* shutdown included): the watchdog is armed.
ADR0057_AWatchdogOnATrial ==
    (running = "t" /\ started /\ perm = "o" /\ ~asleep /\ ~hung) => watchdog

\* A crash, panic or power cut while the computer is in use on a patch on
\* probation goes back: the patch isn't what every start uses, and the
\* computer doesn't start it again.
ADR0057_ACrashOnProbationGoesBack ==
    earlyCrash => (perm = "o" /\ running # "t" /\ (running = "off" => ~once))

\* A patch IT no longer allows is never made the one every start uses.
ADR0057_NoPatchKeptOnceItsTrustIsGone == ~keptRevoked

\* The computer never runs from an environment whose modules aren't all
\* written.
ADR0057_NeverStartsAHalfWrittenEnvironment ==
    running = "t" => env = "complete"

\* Every record glassd wrote is still there (AU-1).
AU1_NoRecordLostToAnEnvironment == ~lost

\* A staged patch ends, kept, failed, rolled back or taken back, once the
\* person restarts and uses the computer.
ADR0057_ATrialEnds ==
    (record = "staged") ~> (record \in Ended)

\* A patch not yet kept that hangs the computer in use: the computer
\* restarts by itself, without the person's power button.
ADR0057_AHangOnATrialEndsByItself ==
    (hung /\ quality = "hangsLater" /\ perm = "o") ~> ~hung

\* After a crash on probation the trial ends, back on the environment
\* before.
ADR0057_ACrashOnProbationEndsIt ==
    earlyCrash ~> (running = "o" /\ record \in Ended)

\* A patch taken out leaves nothing behind once the computer is back on the
\* environment before.
ADR0057_AnUndoFinishes ==
    (record = "undone" /\ running = "o") ~> (env = "none" \/ record # "undone")

=============================================================================
