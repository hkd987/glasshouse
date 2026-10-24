---------------------------- MODULE HardwarePatch ----------------------------
(***************************************************************************)
(* A kernel patch tried in a boot environment of its own (ADR 0057),       *)
(* through power cuts and crashes at any step, restarts the person makes,  *)
(* and a patch that works, panics as the computer starts, or fails         *)
(* glassd's check.                                                          *)
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
(*    debug.debugger_on_panic=0). Panic.                                   *)
(*  - `at_start`, as glassd starts: it reads the record, which environment *)
(*    runs and whether the trial is still set to start once, and confirms  *)
(*    (`bectl activate`), says it failed, says it rolled back, or finishes  *)
(*    staging that a crash cut short. GlassdStart.                          *)
(*  - `cancel`: the person takes the trial back before restarting. Cancel. *)
(*  - The environment of a trial that failed is removed once the computer  *)
(*    isn't running from it. Remove.                                       *)
(*                                                                         *)
(* glassd's state, the record and the audit log, is on a dataset of its    *)
(* own, outside the environments (post-install.sh). Old = {"stateInEnv"}   *)
(* puts it inside them, as it was: what glassd records on the running     *)
(* environment after the new one is made isn't in the new one, so making  *)
(* that one permanent loses it (AU-1). Old = {"activateFirst"} sets the    *)
(* start-once before the modules are all written, and {"permanent"} uses   *)
(* `bectl activate` for the trial: a patch that panics then starts the    *)
(* computer into a panic every time.                                      *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    MaxTrials,     \* patches tried, one after another
    MaxRestarts,   \* power cuts and crashes, at any step
    MaxRecords,    \* audit records agents write meanwhile
    Old

Envs == {"o", "t"}   \* the environment the computer ran from, the trial's
Records == {"none", "staging", "staged", "confirmed", "failed", "rolledback", "cancelled"}

VARIABLES
    running,    \* "off", or the environment the computer runs from
    started,    \* glassd's start check ran since this start
    env,        \* the trial environment: "none", "partial", "complete"
    once,       \* the next start is the trial's, once
    perm,       \* the environment every start uses otherwise
    record,     \* the trial's record, where glassd keeps it
    recordT,    \* with "stateInEnv": the copy inside the trial environment
    step,       \* how far glassd's `apply` got: "idle", "begun", "made", "written", "activated"
    quality,    \* what the patch being tried does: "good", "panics", "unhealthy"
    trials,
    restarts,
    records,    \* audit records agents wrote
    onlyInO,    \* with "stateInEnv": a record written on o after t was made
    lost        \* a record that was written is gone

vars == <<running, started, env, once, perm, record, recordT, step, quality,
          trials, restarts, records, onlyInO, lost>>

TypeOK ==
    /\ running \in {"off"} \cup Envs
    /\ started \in BOOLEAN
    /\ env \in {"none", "partial", "complete"}
    /\ once \in BOOLEAN
    /\ perm \in Envs
    /\ record \in Records /\ recordT \in Records
    /\ step \in {"idle", "begun", "made", "written", "activated"}
    /\ quality \in {"good", "panics", "unhealthy"}
    /\ trials \in 0..MaxTrials
    /\ restarts \in 0..MaxRestarts
    /\ records \in 0..MaxRecords
    /\ onlyInO \in BOOLEAN
    /\ lost \in BOOLEAN

Init ==
    /\ running = "o" /\ started = TRUE
    /\ env = "none" /\ once = FALSE /\ perm = "o"
    /\ record = "none" /\ recordT = "none" /\ step = "idle"
    /\ quality = "good" /\ trials = 0
    /\ restarts = 0 /\ records = 0 /\ onlyInO = FALSE /\ lost = FALSE

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

-----------------------------------------------------------------------------
\* `apply`, step by step, on the running environment, with no trial open.
BeginStage ==
    /\ running = "o" /\ started /\ step = "idle" /\ trials < MaxTrials
    /\ Seen \in {"none", "failed", "rolledback", "cancelled", "confirmed"}
    /\ perm = "o" /\ env = "none"
    /\ Write("staging")
    /\ step' = "begun"
    /\ quality' \in {"good", "panics", "unhealthy"}
    /\ trials' = trials + 1
    /\ Audit
    /\ UNCHANGED <<running, started, env, once, perm, restarts, records, lost>>

CreateEnv ==
    /\ running = "o" /\ step = "begun"
    /\ env' = "partial"
    \* The copy inside t is the record as it was when t was made.
    /\ recordT' = record
    /\ step' = "made"
    /\ UNCHANGED <<running, started, once, perm, record, quality, trials, restarts,
                   records, onlyInO, lost>>

WriteModules ==
    /\ running = "o"
    /\ IF "activateFirst" \in Old THEN step = "activated" ELSE step = "made"
    /\ env' = "complete"
    /\ step' = "written"
    /\ UNCHANGED <<running, started, once, perm, record, recordT, quality,
                   trials, restarts, records, onlyInO, lost>>

ActivateOnce ==
    /\ running = "o"
    /\ IF "activateFirst" \in Old THEN step = "made" ELSE step = "written"
    /\ IF "permanent" \in Old
       THEN /\ perm' = "t" /\ UNCHANGED once
       ELSE /\ once' = TRUE /\ UNCHANGED perm
    /\ step' = "activated"
    /\ UNCHANGED <<running, started, env, record, recordT, quality, trials,
                   restarts, records, onlyInO, lost>>

MarkStaged ==
    /\ running = "o" /\ step = IF "activateFirst" \in Old THEN "written" ELSE "activated"
    /\ Write("staged")
    /\ step' = "idle"
    /\ Audit
    /\ UNCHANGED <<running, started, env, once, perm, quality, trials, restarts,
                   records, lost>>

\* The person takes the trial back before restarting.
Cancel ==
    /\ running = "o" /\ started /\ step = "idle" /\ Seen = "staged" /\ once
    /\ once' = FALSE
    /\ Write("cancelled")
    /\ Audit
    /\ UNCHANGED <<running, started, env, perm, step, quality, trials, restarts,
                   records, lost>>

\* Agents keep working, and glassd keeps writing their records.
AgentRecord ==
    /\ running # "off" /\ records < MaxRecords
    /\ records' = records + 1
    /\ Audit
    /\ UNCHANGED <<running, started, env, once, perm, record, recordT, step,
                   quality, trials, restarts, lost>>

-----------------------------------------------------------------------------
\* A power cut or a crash, at any step: memory is lost, the disk is kept.
Stop ==
    /\ running # "off" /\ restarts < MaxRestarts
    /\ running' = "off" /\ started' = FALSE
    /\ step' = "idle"
    /\ restarts' = restarts + 1
    /\ UNCHANGED <<env, once, perm, record, recordT, quality, trials, records,
                   onlyInO, lost>>

\* The person restarts to try a staged patch, when glassd tells them to.
TryIt ==
    /\ running = "o" /\ started /\ step = "idle" /\ Seen = "staged"
    /\ running' = "off" /\ started' = FALSE
    /\ UNCHANGED <<env, once, perm, record, recordT, step, quality, trials,
                   restarts, records, onlyInO, lost>>

\* The loader: the environment set to start once, clearing that, else the
\* permanent one.
Start ==
    /\ running = "off"
    /\ IF once
       THEN /\ running' = "t" /\ once' = FALSE
       ELSE /\ running' = perm /\ UNCHANGED once
    /\ UNCHANGED <<started, env, perm, record, recordT, step, quality, trials,
                   restarts, records, onlyInO, lost>>

\* The patch's module panics as the trial starts: the computer restarts at
\* once. Not counted against MaxRestarts: it is the patch's, not anyone's.
Panic ==
    /\ running = "t" /\ ~started /\ quality = "panics" /\ env # "none"
    /\ running' = "off"
    /\ UNCHANGED <<started, env, once, perm, record, recordT, step, quality,
                   trials, restarts, records, onlyInO, lost>>

\* glassd's check as it starts.
GlassdStart ==
    /\ running # "off" /\ ~started
    /\ ~(running = "t" /\ quality = "panics")
    /\ started' = TRUE
    /\ LET r == Seen IN
       \/ \* Running from the trial, staged or (a crash after the start-once
          \* was set) still staging: check the patch.
          /\ running = "t" /\ r \in {"staging", "staged"}
          /\ IF quality = "good" /\ env = "complete"
             THEN /\ perm' = "t" /\ Write("confirmed")
                  /\ lost' = (lost \/ onlyInO)
             ELSE /\ Write("failed") /\ UNCHANGED <<perm, lost>>
          /\ Audit
       \/ \* On the environment it came from, the trial still to come.
          /\ running = "o" /\ r \in {"staging", "staged"} /\ once
          /\ Write("staged")
          /\ UNCHANGED <<perm, lost, onlyInO>>
       \/ \* On the environment it came from, the trial's start over: it
          \* didn't come back, or staging never finished.
          /\ running = "o" /\ r \in {"staging", "staged"} /\ ~once
          /\ Write("rolledback")
          /\ Audit
          /\ UNCHANGED <<perm, lost>>
       \/ /\ r \in {"none", "confirmed", "failed", "rolledback", "cancelled"}
          /\ UNCHANGED <<perm, record, recordT, lost, onlyInO>>
    /\ UNCHANGED <<running, env, once, step, quality, trials, restarts, records>>

\* The environment of a trial that ended without being kept is removed once
\* nothing runs from it or will start it.
Remove ==
    /\ env # "none" /\ running = "o" /\ started /\ step = "idle"
    /\ perm = "o" /\ ~once
    /\ Seen \in {"failed", "rolledback", "cancelled"}
    /\ env' = "none"
    /\ UNCHANGED <<running, started, once, perm, record, recordT, step, quality,
                   trials, restarts, records, onlyInO, lost>>

Next ==
    \/ BeginStage \/ CreateEnv \/ WriteModules \/ ActivateOnce \/ MarkStaged
    \/ Cancel \/ AgentRecord
    \/ Stop \/ TryIt \/ Start \/ Panic \/ GlassdStart \/ Remove

\* The person restarts to try the patch (once it's staged); the computer
\* starts, glassd checks, a panic restarts it.
Spec == Init /\ [][Next]_vars
        /\ WF_vars(Start) /\ WF_vars(Panic) /\ WF_vars(GlassdStart)
        /\ WF_vars(CreateEnv) /\ WF_vars(WriteModules) /\ WF_vars(ActivateOnce)
        /\ WF_vars(MarkStaged) /\ WF_vars(TryIt)

-----------------------------------------------------------------------------
\* The environment every start uses runs: o, or a trial that started and
\* passed glassd's check.
ADR0057_TheComputerAlwaysStarts ==
    perm = "o" \/ (env = "complete" /\ quality = "good")

\* A trial becomes permanent only after glassd confirmed it.
ADR0057_OnlyAConfirmedPatchIsKept ==
    perm = "t" => (record = "confirmed" \/ recordT = "confirmed")

\* The computer never runs from an environment whose modules aren't all
\* written.
ADR0057_NeverStartsAHalfWrittenEnvironment ==
    running = "t" => env = "complete"

\* Every record glassd wrote is still there (AU-1).
AU1_NoRecordLostToAnEnvironment == ~lost

\* A staged patch ends, confirmed, failed or rolled back, once the person
\* restarts.
ADR0057_ATrialEnds ==
    (record = "staged") ~> (record \in {"confirmed", "failed", "rolledback", "cancelled"})

=============================================================================
