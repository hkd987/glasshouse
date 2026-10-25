----------------------------- MODULE Containment -----------------------------
(***************************************************************************)
(* A task's containment over its whole life and across glassd's restarts  *)
(* (PA-1, PA-2, AW-11): its jail made and torn down, its agent started    *)
(* and stopped, the person's live-task limit, and what a crash, a clean   *)
(* stop or the computer restarting under glassd leaves behind.            *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/svc/runtime/src/runtime.rs `start_task`: counts the person's *)
(*    live tasks, then inserts the new one (StartCount, StartInsert; one   *)
(*    step under the tasks' lock now: Start). `end_task` (Stop).          *)
(*    `shutdown` (ShutdownBegin: stops the live tasks, waits for the      *)
(*    supervisors it has; ShutdownExit).                                   *)
(*  - crates/svc/runtime/src/supervise.rs `run`, `setup_and_wait`: app    *)
(*    state and the opening snapshot (Prepare), the sandbox made, the task *)
(*    socket bound, the agent spawned (Spawn), waited for or killed on     *)
(*    stop (Kill, AgentExits), then teardown whatever happened.           *)
(*  - crates/lib/platform/src/sandbox/jail.rs `create` (WriteSpec,        *)
(*    MakeRoot: the root and its layout, the person's scratch `/work`,    *)
(*    JailCreate: `jail -c` mounts and starts the jail), `destroy`         *)
(*    (JailRemove: `jail -r` kills every process in it and unmounts,       *)
(*    RemoveRoot, RemoveSpec), `cleanup_stale` (Cleanup: at glassd's      *)
(*    start, every spec on disk destroyed before any task starts).         *)
(*  - runtime.rs `begin_system_check`, `check_system` and system.rs       *)
(*    `may_start` (ADR 0064, EP-1): at each glassd start the system's      *)
(*    files are checked against the list the OS key signed (CheckSystem);  *)
(*    no task starts while that runs, nor after it found them changed.    *)
(*    The files can be changed, or put right, at any time (Tamper,        *)
(*    PutRight): a change after the check is the next start's to find.    *)
(*  - The computer: glassd can die (Crash), be stopped (ShutdownBegin,    *)
(*    ShutdownExit) or the computer restart under it (Reboot: jails,       *)
(*    mounts and processes are gone, `/var/run` is emptied by cleanvar,    *)
(*    `/var/db` stays), and glassd start again (GlassdStarts).             *)
(*                                                                         *)
(* Abstractions: a task's snapshot and grants are the Approvals and Undo   *)
(* models'; the sandbox's mounts are part of its jail (jail(8) owns them); *)
(* its task socket lives in its root; a failing `jail -c` or `destroy` is  *)
(* left out (the teardown runs the same steps).                            *)
(*                                                                         *)
(* Old is the code before ADR 0053, by part:                               *)
(*  "countApart"  start_task counted the live tasks and inserted the new  *)
(*                one in two steps, so two starts at once both fit.        *)
(*  "noStopCheck" setup never looked at a stop: the agent was started,    *)
(*                then killed.                                             *)
(*  "specLast"    create made the root before writing the spec that       *)
(*                cleanup_stale finds it by.                               *)
(*  "specInRun"   the specs lived in /var/run, which the boot empties.    *)
(*  "openClosing" a start could land while glassd shut down, after its    *)
(*                stops, and run on after it exited.                       *)
(*  "noSystemCheck" before ADR 0064: a task started whatever the system's *)
(*                files were (EP1_NoAgentStartsOnAChangedSystem fails).    *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Tasks,          \* tasks one person asks for, each once
    MaxLive,        \* max_live_tasks_per_user
    MaxCrashes,     \* glassd dying without its shutdown
    MaxReboots,     \* the computer restarting under glassd (power, panic)
    MaxShutdowns,   \* glassd stopped cleanly (service glassd stop, restart)
    Old             \* the code before ADR 0053, by part (see above)

Sup == {"none", "prepare", "spec", "root", "jailc", "check", "spawn", "wait",
        "jailr", "rmroot", "rmspec", "done"}
\* Where a stop is seen before the agent starts: the supervisor's look at
\* the task before it creates the sandbox, and before it spawns the agent.
BeforeLook == {"prepare", "spec", "root", "jailc", "check"}
MaxRuns == 1 + MaxCrashes + MaxReboots + MaxShutdowns

VARIABLES
    up,         \* glassd is running
    cleaned,    \* this run's start-up cleanup (cleanup_stale) is done
    closing,    \* this run is shutting down
    run,        \* glassd's runs so far
    tile,       \* in glassd's memory: "none" (not there), "live", "ended"
    era,        \* the run each task started in (0: not started)
    req,        \* each task's start request: "idle", "counted", "done"
    seen,       \* what its count of live tasks saw
    sup,        \* each task's supervisor: where it is (Sup)
    jail,       \* the task's jail exists, with its mounts
    proc,       \* its agent's processes run
    root,       \* its root is on disk, with what the agent left in /work
    spec,       \* its spec (and conf, fstab) is on disk
    waited,     \* the supervisors a shutdown waits for
    crashes, reboots, shutdowns,
    stoppedEarly, \* ghost: each task's stop came before its supervisor's look
    lateSpawn,  \* ghost: an agent started after a stop its supervisor could see
    cleanExit,  \* ghost: glassd's last exit was a clean stop that waited
    sys,        \* this run's check of the system: "checking", "verified", "changed"
    tampered,   \* the system's files differ from the list the OS key signed
    startedUnverified \* ghost: a task started before or against the check

vars == <<up, cleaned, closing, run, tile, era, req, seen, sup, jail, proc,
          root, spec, waited, crashes, reboots, shutdowns, stoppedEarly,
          stoppedEarly, lateSpawn, cleanExit, sys, tampered, startedUnverified>>

\* The start check's variables, which most steps leave alone.
system == <<sys, tampered, startedUnverified>>

Live == {t \in Tasks : tile[t] = "live"}

TypeOK ==
    /\ up \in BOOLEAN /\ cleaned \in BOOLEAN /\ closing \in BOOLEAN
    /\ run \in 1..MaxRuns
    /\ tile \in [Tasks -> {"none", "live", "ended"}]
    /\ era \in [Tasks -> 0..MaxRuns]
    /\ req \in [Tasks -> {"idle", "counted", "done"}]
    /\ seen \in [Tasks -> 0..Cardinality(Tasks)]
    /\ sup \in [Tasks -> Sup]
    /\ jail \in [Tasks -> BOOLEAN] /\ proc \in [Tasks -> BOOLEAN]
    /\ root \in [Tasks -> BOOLEAN] /\ spec \in [Tasks -> BOOLEAN]
    /\ waited \subseteq Tasks
    /\ stoppedEarly \in [Tasks -> BOOLEAN]
    /\ lateSpawn \in BOOLEAN /\ cleanExit \in BOOLEAN
    /\ sys \in {"checking", "verified", "changed"}
    /\ tampered \in BOOLEAN /\ startedUnverified \in BOOLEAN

Init ==
    /\ up = TRUE /\ cleaned = FALSE /\ closing = FALSE /\ run = 1
    /\ tile = [t \in Tasks |-> "none"]
    /\ era = [t \in Tasks |-> 0]
    /\ req = [t \in Tasks |-> "idle"]
    /\ seen = [t \in Tasks |-> 0]
    /\ sup = [t \in Tasks |-> "none"]
    /\ jail = [t \in Tasks |-> FALSE] /\ proc = [t \in Tasks |-> FALSE]
    /\ root = [t \in Tasks |-> FALSE] /\ spec = [t \in Tasks |-> FALSE]
    /\ waited = {}
    /\ crashes = 0 /\ reboots = 0 /\ shutdowns = 0
    /\ stoppedEarly = [t \in Tasks |-> FALSE]
    /\ lateSpawn = FALSE /\ cleanExit = FALSE
    /\ sys = "checking" /\ tampered \in BOOLEAN /\ startedUnverified = FALSE

disk == <<jail, proc, root, spec>>
counts == <<crashes, reboots, shutdowns>>

-----------------------------------------------------------------------------
(* glassd's start: cleanup_stale destroys every sandbox it has a spec of  *)
(* before anything is served.                                              *)

Cleanup ==
    /\ up /\ ~cleaned
    /\ cleaned' = TRUE
    /\ jail' = [t \in Tasks |-> jail[t] /\ ~spec[t]]
    /\ proc' = [t \in Tasks |-> proc[t] /\ ~spec[t]]
    /\ root' = [t \in Tasks |-> root[t] /\ ~spec[t]]
    /\ spec' = [t \in Tasks |-> FALSE]
    /\ UNCHANGED <<up, closing, run, tile, era, req, seen, sup, waited, counts,
                   stoppedEarly, lateSpawn, cleanExit, system>>

(* Starting a task: policy and scope first (`prepare`), then the person's *)
(* live tasks counted against the limit, then the task inserted and its   *)
(* supervisor spawned. A refused start ends its request.                   *)

Admit(t) ==
    /\ tile' = [tile EXCEPT ![t] = "live"]
    /\ era' = [era EXCEPT ![t] = run]
    /\ sup' = [sup EXCEPT ![t] = "prepare"]
    /\ startedUnverified' = (startedUnverified \/ sys # "verified")

\* What `may_start` lets through: a system this run's check verified.
SystemLetsStart == sys = "verified" \/ "noSystemCheck" \in Old

Start(t) ==
    /\ "countApart" \notin Old
    /\ up /\ cleaned /\ req[t] = "idle"
    /\ req' = [req EXCEPT ![t] = "done"]
    /\ IF Cardinality(Live) < MaxLive /\ (closing => "openClosing" \in Old)
          /\ SystemLetsStart
       THEN Admit(t)
       ELSE UNCHANGED <<tile, era, sup, startedUnverified>>
    /\ UNCHANGED <<up, cleaned, closing, run, seen, disk, waited, counts,
                   stoppedEarly, lateSpawn, cleanExit, sys, tampered>>

StartCount(t) ==
    /\ "countApart" \in Old
    /\ up /\ cleaned /\ req[t] = "idle"
    /\ req' = [req EXCEPT ![t] = "counted"]
    /\ seen' = [seen EXCEPT ![t] = Cardinality(Live)]
    /\ UNCHANGED <<up, cleaned, closing, run, tile, era, sup, disk, waited,
                   counts, stoppedEarly, lateSpawn, cleanExit, system>>

StartInsert(t) ==
    /\ up /\ req[t] = "counted"
    /\ req' = [req EXCEPT ![t] = "done"]
    /\ IF seen[t] < MaxLive /\ (closing => "openClosing" \in Old)
          /\ SystemLetsStart
       THEN Admit(t)
       ELSE UNCHANGED <<tile, era, sup, startedUnverified>>
    /\ UNCHANGED <<up, cleaned, closing, run, seen, disk, waited, counts,
                   stoppedEarly, lateSpawn, cleanExit, sys, tampered>>

\* Stop (end_task): the tile and the gate say Ended at once; the supervisor
\* hears it at its next look.
Stop(t) ==
    /\ up /\ tile[t] = "live"
    /\ tile' = [tile EXCEPT ![t] = "ended"]
    /\ stoppedEarly' = [stoppedEarly EXCEPT ![t] = sup[t] \in BeforeLook]
    /\ UNCHANGED <<up, cleaned, closing, run, era, req, seen, sup, disk,
                   waited, counts, lateSpawn, cleanExit, system>>

-----------------------------------------------------------------------------
(* The supervisor, one step at a time. Teardown runs whatever happened.   *)

Step(t, from, to) == sup[t] = from /\ sup' = [sup EXCEPT ![t] = to]
Others == <<up, cleaned, closing, run, era, req, seen, waited, counts,
            stoppedEarly, cleanExit, system>>

\* App state and the opening snapshot are ready; a stop by now is seen
\* before anything is made.
Prepare(t) ==
    /\ up /\ sup[t] = "prepare"
    /\ sup' = [sup EXCEPT ![t] =
                 IF tile[t] = "ended" /\ "noStopCheck" \notin Old THEN "jailr"
                 ELSE IF "specLast" \in Old THEN "root" ELSE "spec"]
    /\ UNCHANGED <<tile, disk, lateSpawn>> /\ UNCHANGED Others

\* The spec that cleanup_stale finds a sandbox by.
WriteSpec(t) ==
    /\ up /\ Step(t, "spec", IF "specLast" \in Old THEN "jailc" ELSE "root")
    /\ spec' = [spec EXCEPT ![t] = TRUE]
    /\ UNCHANGED <<tile, jail, proc, root, lateSpawn>> /\ UNCHANGED Others

\* The root, its layout and the person's scratch folder.
MakeRoot(t) ==
    /\ up /\ Step(t, "root", IF "specLast" \in Old THEN "spec" ELSE "jailc")
    /\ root' = [root EXCEPT ![t] = TRUE]
    /\ UNCHANGED <<tile, jail, proc, spec, lateSpawn>> /\ UNCHANGED Others

\* `jail -c`: the jail and its mounts. Then the task socket is bound.
JailCreate(t) ==
    /\ up /\ Step(t, "jailc", IF "noStopCheck" \in Old THEN "spawn" ELSE "check")
    /\ jail' = [jail EXCEPT ![t] = TRUE]
    /\ UNCHANGED <<tile, proc, root, spec, lateSpawn>> /\ UNCHANGED Others

\* The look before the agent starts.
Check(t) ==
    /\ up /\ sup[t] = "check"
    /\ sup' = [sup EXCEPT ![t] = IF tile[t] = "ended" THEN "jailr" ELSE "spawn"]
    /\ UNCHANGED <<tile, disk, lateSpawn>> /\ UNCHANGED Others

Spawn(t) ==
    /\ up /\ Step(t, "spawn", "wait")
    /\ proc' = [proc EXCEPT ![t] = TRUE]
    \* A stop it could have seen came first.
    /\ lateSpawn' = (lateSpawn \/ stoppedEarly[t])
    /\ UNCHANGED <<tile, jail, root, spec>> /\ UNCHANGED Others

\* Waiting on the agent, the stop arrives: it is killed.
Kill(t) ==
    /\ up /\ tile[t] = "ended" /\ Step(t, "wait", "jailr")
    /\ proc' = [proc EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<tile, jail, root, spec, lateSpawn>> /\ UNCHANGED Others

\* The agent finishes (or fails) by itself.
AgentExits(t) ==
    /\ up /\ proc[t] /\ tile[t] = "live" /\ Step(t, "wait", "jailr")
    /\ proc' = [proc EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<tile, jail, root, spec, lateSpawn>> /\ UNCHANGED Others

\* An agent left running while glassd is down quits on its own; its jail
\* persists.
AgentQuits(t) ==
    /\ proc[t] /\ sup[t] = "none"
    /\ proc' = [proc EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<sup, tile, jail, root, spec, lateSpawn>> /\ UNCHANGED Others

\* destroy: `jail -r` stops every process and unmounts, then the root,
\* then the spec.
JailRemove(t) ==
    /\ up /\ Step(t, "jailr", "rmroot")
    /\ jail' = [jail EXCEPT ![t] = FALSE]
    /\ proc' = [proc EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<tile, root, spec, lateSpawn>> /\ UNCHANGED Others

RemoveRoot(t) ==
    /\ up /\ Step(t, "rmroot", "rmspec")
    /\ root' = [root EXCEPT ![t] = FALSE]
    /\ UNCHANGED <<tile, jail, proc, spec, lateSpawn>> /\ UNCHANGED Others

\* The tile says how it ended once the sandbox is gone.
RemoveSpec(t) ==
    /\ up /\ Step(t, "rmspec", "done")
    /\ spec' = [spec EXCEPT ![t] = FALSE]
    /\ tile' = [tile EXCEPT ![t] = "ended"]
    /\ UNCHANGED <<jail, proc, root, lateSpawn>> /\ UNCHANGED Others

-----------------------------------------------------------------------------
(* The computer and glassd.                                                *)

\* glassd's memory: tasks, supervisors and requests are gone.
Forget ==
    /\ up' = FALSE /\ cleaned' = FALSE /\ closing' = FALSE
    /\ tile' = [t \in Tasks |-> "none"]
    /\ sup' = [t \in Tasks |-> "none"]
    /\ req' = [t \in Tasks |-> IF req[t] = "counted" THEN "done" ELSE req[t]]
    /\ waited' = {}

Crash ==
    /\ up /\ crashes < MaxCrashes
    /\ Forget /\ crashes' = crashes + 1 /\ cleanExit' = FALSE
    /\ UNCHANGED <<run, era, seen, disk, reboots, shutdowns, stoppedEarly,
                   lateSpawn, system>>

\* The power goes, or the kernel panics: jails, mounts and processes are
\* gone with it, and the boot empties /var/run.
Reboot ==
    /\ reboots < MaxReboots
    /\ Forget /\ reboots' = reboots + 1 /\ cleanExit' = FALSE
    /\ jail' = [t \in Tasks |-> FALSE]
    /\ proc' = [t \in Tasks |-> FALSE]
    /\ spec' = IF "specInRun" \in Old THEN [t \in Tasks |-> FALSE] ELSE spec
    /\ UNCHANGED <<run, era, seen, root, crashes, shutdowns, stoppedEarly,
                   lateSpawn, system>>

\* `shutdown`: stops the live tasks, then waits for the supervisors there
\* are (a start after its stops isn't stopped).
ShutdownBegin ==
    /\ up /\ cleaned /\ ~closing /\ shutdowns < MaxShutdowns
    /\ closing' = TRUE
    /\ tile' = [t \in Tasks |-> IF tile[t] = "live" THEN "ended" ELSE tile[t]]
    /\ waited' = {t \in Tasks : sup[t] \notin {"none", "done"}}
    /\ UNCHANGED <<up, cleaned, run, era, req, seen, sup, disk, counts,
                   stoppedEarly, lateSpawn, cleanExit, system>>

\* Every supervisor it waited for has torn its sandbox down.
ShutdownExit ==
    /\ up /\ closing /\ \A t \in waited : sup[t] = "done"
    /\ Forget /\ shutdowns' = shutdowns + 1 /\ cleanExit' = TRUE
    /\ UNCHANGED <<run, era, seen, disk, crashes, reboots, stoppedEarly,
                   lateSpawn, system>>

\* begin_system_check: no task starts until the check is done.
GlassdStarts ==
    /\ ~up /\ run < MaxRuns
    /\ up' = TRUE /\ run' = run + 1 /\ cleanExit' = FALSE
    /\ sys' = "checking"
    /\ UNCHANGED <<cleaned, closing, tile, era, req, seen, sup, disk, waited,
                   counts, stoppedEarly, lateSpawn, tampered, startedUnverified>>

\* check_system: the files against the list, as they are now.
CheckSystem ==
    /\ up /\ sys = "checking"
    /\ sys' = IF tampered THEN "changed" ELSE "verified"
    /\ UNCHANGED <<up, cleaned, closing, run, tile, era, req, seen, sup, disk,
                   waited, counts, stoppedEarly, lateSpawn, cleanExit, tampered,
                   startedUnverified>>

\* Root changes a system file (the flag taken off first), or IT puts the
\* system right (an update taken out, the computer installed again).
Tamper ==
    /\ ~tampered /\ tampered' = TRUE
    /\ UNCHANGED <<up, cleaned, closing, run, tile, era, req, seen, sup, disk,
                   waited, counts, stoppedEarly, lateSpawn, cleanExit, sys,
                   startedUnverified>>

PutRight ==
    /\ tampered /\ tampered' = FALSE
    /\ UNCHANGED <<up, cleaned, closing, run, tile, era, req, seen, sup, disk,
                   waited, counts, stoppedEarly, lateSpawn, cleanExit, sys,
                   startedUnverified>>

-----------------------------------------------------------------------------

Next ==
    \/ Cleanup \/ Crash \/ Reboot \/ ShutdownBegin \/ ShutdownExit \/ GlassdStarts
    \/ CheckSystem \/ Tamper \/ PutRight
    \/ \E t \in Tasks :
        \/ Start(t) \/ StartCount(t) \/ StartInsert(t) \/ Stop(t)
        \/ Prepare(t) \/ WriteSpec(t) \/ MakeRoot(t) \/ JailCreate(t)
        \/ Check(t) \/ Spawn(t) \/ Kill(t) \/ AgentExits(t) \/ AgentQuits(t)
        \/ JailRemove(t) \/ RemoveRoot(t) \/ RemoveSpec(t)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* What the OS promises.                                                   *)

\* The person's live tasks never pass the limit, however many start at once.
LiveLimitHolds == Cardinality(Live) <= MaxLive

\* PA-1: an agent runs only inside its own task's jail.
PA1_AnAgentRunsOnlyInItsJail == \A t \in Tasks : proc[t] => jail[t]

\* PA-2: a task torn down leaves nothing: no jail, mount, process, root
\* (what the agent left in /work) or spec.
PA2_NothingOutlivesTheTask ==
    \A t \in Tasks : sup[t] = "done" => ~(jail[t] \/ proc[t] \/ root[t] \/ spec[t])

\* PA-2 across glassd's runs: once glassd has started again, before any
\* task starts, nothing of an earlier run's tasks is left, whether glassd
\* crashed, was stopped, or the computer restarted under it.
PA2_NothingOfAnEarlierRunOnceStarted ==
    (up /\ cleaned) =>
        \A t \in Tasks : era[t] \in 1..(run - 1) =>
            ~(jail[t] \/ proc[t] \/ root[t] \/ spec[t])

\* AW-11: a stop the supervisor could see before the agent starts keeps it
\* from starting.
AW11_AStoppedTaskNeverStartsItsAgent == ~lateSpawn

\* EP-1 (ADR 0064): no task starts in a run of glassd before its check of
\* the system's files, nor after the check found them changed.
EP1_NoAgentStartsOnAChangedSystem == ~startedUnverified

\* PA-2: glassd stopped cleanly leaves no agent running and no jail, once
\* the teardowns it waited for are done.
PA2_ACleanStopLeavesNoAgentRunning ==
    cleanExit => \A t \in Tasks : ~proc[t] /\ ~jail[t]

=============================================================================
