---------------------------- MODULE AgentComputers ----------------------------
(***************************************************************************)
(* An agent's computer over its life (ADR 0065, ADR 0066): kept between    *)
(* its tasks, each task run in it with its own socket; its person taking   *)
(* its keyboard; asleep in the store and woken; and glassd dying at any    *)
(* step. One computer, the tasks of its person's agents (ADR 0068).       *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/svc/computers/src/sandbox.rs `create` (Start: refused while   *)
(*    taken over or moving; a computer asleep is woken first), `destroy`   *)
(*    (End: the task's socket and processes go, its computer stays),       *)
(*    `take_over`, `hand_back`, `sleep` (SleepBegin: refused while a run   *)
(*    uses it, taken over or moving; SleepSave: the whole folder to the    *)
(*    store; SleepRecord: the record says asleep; SleepClear: the folder   *)
(*    but its record removed), `wake` (WakeBegin; WakeClear: what an       *)
(*    earlier wake left removed; WakeRestore; WakeRecord), `cleanup_stale` *)
(*    (Cleanup: at glassd's start, before any task).                       *)
(*    For a computer whose record says asleep, it also removes what a     *)
(*    sleep glassd didn't live through left in its folder.                 *)
(*  - crates/svc/computers/src/namespaces.rs `stop_leftovers`: a run's     *)
(*    processes are killed with it while glassd lives (`--kill-child`, a   *)
(*    PID namespace); glassd dying leaves them running, and its next start *)
(*    kills every process whose root is a computer's disk.                 *)
(*                                                                         *)
(* What glassd keeps in memory (runs, who has the keyboard, what's moving) *)
(* is lost when it dies; the record, the folder and the store are not. A   *)
(* task's own data is a version number: each write makes the next one.     *)
(*                                                                         *)
(* Old is the code before this model, by part:                             *)
(*  "noClearOnWake"  nothing removed what a sleep or wake glassd didn't    *)
(*                   live through left in the folder: wake restored beside *)
(*                   it and moved the pieces in over it, and failed from   *)
(*                   then on (WakeAlwaysWorks fails).                      *)
(*  "noLeftoverStop" glassd's start removed tasks' sockets but not their   *)
(*                   processes, which ran on after it died and wrote to    *)
(*                   the computer while it was saved to the store          *)
(*                   (NothingWrittenIsLost fails).                         *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    Tasks,          \* tasks of the agent, each run once
    MaxWrites,      \* writes a task makes to its computer
    MaxCrashes,     \* glassd dying
    Old             \* the code before this model, by part (see above)

VARIABLES
    \* On disk and in the store: they survive glassd.
    asleep,         \* the record says asleep (in the store)
    local,          \* the computer's folder: "none", "part" or "whole"
    localVer,       \* the version the folder holds, when whole
    storeVer,       \* the latest version in the store, 0 for none
    data,           \* the latest version a task wrote
    sock,           \* each task's socket file
    \* Processes: they outlive glassd until something kills them.
    proc,           \* each task's processes
    writes,         \* writes each task made
    \* glassd's memory: lost when it dies.
    up, cleaned,
    runs,           \* the tasks it runs now
    taken,          \* its person has the keyboard
    moving,         \* "no", or the step of a sleep or a wake
    tile,           \* each task: "new", "live", "done"
    crashes,
    stuck           \* a wake failed on what an earlier wake left

vars == <<asleep, local, localVer, storeVer, data, sock, proc, writes, up,
          cleaned, runs, taken, moving, tile, crashes, stuck>>

Steps == {"no", "save", "record", "clear", "wclear", "restore", "wrecord"}

TypeOK ==
    /\ asleep \in BOOLEAN
    /\ local \in {"none", "part", "whole"}
    /\ localVer \in 0..MaxWrites * 3
    /\ storeVer \in 0..MaxWrites * 3
    /\ data \in 0..MaxWrites * 3
    /\ sock \in [Tasks -> BOOLEAN]
    /\ proc \in [Tasks -> BOOLEAN]
    /\ writes \in [Tasks -> 0..MaxWrites]
    /\ up \in BOOLEAN /\ cleaned \in BOOLEAN
    /\ runs \subseteq Tasks
    /\ taken \in BOOLEAN
    /\ moving \in Steps
    /\ tile \in [Tasks -> {"new", "live", "done"}]
    /\ crashes \in 0..MaxCrashes
    /\ stuck \in BOOLEAN

Init ==
    /\ asleep = FALSE /\ local = "whole" /\ localVer = 0 /\ storeVer = 0
    /\ data = 0
    /\ sock = [t \in Tasks |-> FALSE] /\ proc = [t \in Tasks |-> FALSE]
    /\ writes = [t \in Tasks |-> 0]
    /\ up = TRUE /\ cleaned = TRUE
    /\ runs = {} /\ taken = FALSE /\ moving = "no"
    /\ tile = [t \in Tasks |-> "new"]
    /\ crashes = 0 /\ stuck = FALSE

Ready == up /\ cleaned
Unchanged(vs) == UNCHANGED vs

(* A task: its socket bound and its agent started, in a computer that is   *)
(* awake and nobody's moving or typing on.                                 *)
Start(t) ==
    /\ Ready /\ tile[t] = "new" /\ ~taken /\ moving = "no" /\ ~asleep
    /\ local = "whole"
    /\ tile' = [tile EXCEPT ![t] = "live"]
    /\ runs' = runs \cup {t}
    /\ sock' = [sock EXCEPT ![t] = TRUE]
    /\ proc' = [proc EXCEPT ![t] = TRUE]
    /\ UNCHANGED <<asleep, local, localVer, storeVer, data, writes, up,
                   cleaned, taken, moving, crashes, stuck>>

(* A process writes to the computer's disk: a live task's, or one an      *)
(* earlier glassd left running.                                            *)
Write(t) ==
    /\ proc[t] /\ writes[t] < MaxWrites /\ local = "whole"
    /\ data' = data + 1
    /\ localVer' = data + 1
    /\ writes' = [writes EXCEPT ![t] = @ + 1]
    /\ UNCHANGED <<asleep, local, storeVer, sock, proc, up, cleaned, runs,
                   taken, moving, tile, crashes, stuck>>

(* The agent exits or is stopped: teardown kills its processes and removes *)
(* its socket; the computer stays.                                          *)
End(t) ==
    /\ up /\ t \in runs
    /\ runs' = runs \ {t}
    /\ proc' = [proc EXCEPT ![t] = FALSE]
    /\ sock' = [sock EXCEPT ![t] = FALSE]
    /\ tile' = [tile EXCEPT ![t] = "done"]
    /\ UNCHANGED <<asleep, local, localVer, storeVer, data, writes, up,
                   cleaned, taken, moving, crashes, stuck>>

TakeOver ==
    /\ Ready /\ ~taken /\ moving = "no"
    /\ taken' = TRUE
    /\ UNCHANGED <<asleep, local, localVer, storeVer, data, sock, proc,
                   writes, up, cleaned, runs, moving, tile, crashes, stuck>>

HandBack ==
    /\ up /\ taken
    /\ taken' = FALSE
    /\ UNCHANGED <<asleep, local, localVer, storeVer, data, sock, proc,
                   writes, up, cleaned, runs, moving, tile, crashes, stuck>>

(* Sleep: nothing using it.                                                *)
SleepBegin ==
    /\ Ready /\ moving = "no" /\ runs = {} /\ ~taken /\ ~asleep
    /\ moving' = "save"
    /\ UNCHANGED <<asleep, local, localVer, storeVer, data, sock, proc,
                   writes, up, cleaned, runs, taken, tile, crashes, stuck>>

SleepSave ==
    /\ up /\ moving = "save"
    /\ storeVer' = localVer
    /\ moving' = "record"
    /\ UNCHANGED <<asleep, local, localVer, data, sock, proc, writes, up,
                   cleaned, runs, taken, tile, crashes, stuck>>

SleepRecord ==
    /\ up /\ moving = "record"
    /\ asleep' = TRUE
    /\ moving' = "clear"
    /\ UNCHANGED <<local, localVer, storeVer, data, sock, proc, writes, up,
                   cleaned, runs, taken, tile, crashes, stuck>>

SleepClear ==
    /\ up /\ moving = "clear"
    /\ local' = "none"
    /\ moving' = "no"
    /\ UNCHANGED <<asleep, localVer, storeVer, data, sock, proc, writes, up,
                   cleaned, runs, taken, tile, crashes, stuck>>

(* Wake: a task or its person. Its folder is cleared first: asleep, the   *)
(* store holds it all, and anything here is what a sleep or wake glassd    *)
(* didn't live through left.                                               *)
WakeBegin ==
    /\ Ready /\ moving = "no" /\ asleep /\ ~taken
    /\ IF "noClearOnWake" \in Old
          THEN IF local = "none"
                  THEN moving' = "restore" /\ UNCHANGED stuck
                  \* The pieces' renames fail over what's there.
                  ELSE moving' = "no" /\ stuck' = TRUE
          ELSE moving' = "wclear" /\ UNCHANGED stuck
    /\ UNCHANGED <<asleep, local, localVer, storeVer, data, sock, proc,
                   writes, up, cleaned, runs, taken, tile, crashes>>

WakeClear ==
    /\ up /\ moving = "wclear"
    /\ local' = "none"
    /\ moving' = "restore"
    /\ UNCHANGED <<asleep, localVer, storeVer, data, sock, proc, writes, up,
                   cleaned, runs, taken, tile, crashes, stuck>>

(* The store's version back in the folder, a piece at a time: glassd can  *)
(* die with part of it moved in.                                           *)
WakeRestorePart ==
    /\ up /\ moving = "restore" /\ local = "none"
    /\ local' = "part"
    /\ UNCHANGED <<asleep, localVer, storeVer, data, sock, proc, writes, up,
                   cleaned, runs, taken, moving, tile, crashes, stuck>>

WakeRestore ==
    /\ up /\ moving = "restore" /\ local = "part"
    /\ local' = "whole"
    /\ localVer' = storeVer
    /\ moving' = "wrecord"
    /\ UNCHANGED <<asleep, storeVer, data, sock, proc, writes, up, cleaned,
                   runs, taken, tile, crashes, stuck>>

WakeRecord ==
    /\ up /\ moving = "wrecord"
    /\ asleep' = FALSE
    /\ moving' = "no"
    /\ UNCHANGED <<local, localVer, storeVer, data, sock, proc, writes, up,
                   cleaned, runs, taken, tile, crashes, stuck>>

(* glassd dies: its memory goes; sockets, processes, folder, record and    *)
(* store stay as they were.                                                 *)
Crash ==
    /\ up /\ crashes < MaxCrashes
    /\ up' = FALSE /\ cleaned' = FALSE
    /\ runs' = {} /\ taken' = FALSE /\ moving' = "no"
    /\ crashes' = crashes + 1
    /\ tile' = [t \in Tasks |-> IF tile[t] = "live" THEN "done" ELSE tile[t]]
    /\ UNCHANGED <<asleep, local, localVer, storeVer, data, sock, proc,
                   writes, stuck>>

GlassdStarts ==
    /\ ~up
    /\ up' = TRUE
    /\ UNCHANGED <<asleep, local, localVer, storeVer, data, sock, proc,
                   writes, cleaned, runs, taken, moving, tile, crashes, stuck>>

(* cleanup_stale: tasks' sockets removed, and what's still running in a     *)
(* computer killed, before any task starts.                                 *)
Cleanup ==
    /\ up /\ ~cleaned
    /\ cleaned' = TRUE
    /\ sock' = [t \in Tasks |-> FALSE]
    /\ proc' = IF "noLeftoverStop" \in Old THEN proc ELSE [t \in Tasks |-> FALSE]
    \* A computer whose record says asleep keeps nothing here.
    /\ local' = IF asleep /\ "noClearOnWake" \notin Old THEN "none" ELSE local
    /\ UNCHANGED <<asleep, localVer, storeVer, data, writes, up, runs,
                   taken, moving, tile, crashes, stuck>>

Next ==
    \/ \E t \in Tasks : Start(t) \/ Write(t) \/ End(t)
    \/ TakeOver \/ HandBack
    \/ SleepBegin \/ SleepSave \/ SleepRecord \/ SleepClear
    \/ WakeBegin \/ WakeClear \/ WakeRestorePart \/ WakeRestore \/ WakeRecord
    \/ Crash \/ GlassdStarts \/ Cleanup

Spec == Init /\ [][Next]_vars

(* What the OS promises.                                                   *)

\* What tasks wrote is always somewhere: in the folder while it's awake,
\* in the store once the record says asleep.
NothingWrittenIsLost ==
    /\ ~asleep => (local = "whole" /\ localVer = data)
    /\ asleep => storeVer = data

\* Nothing stays on the host for a computer asleep, once its sleep is done
\* (or the start after glassd died during it).
AsleepIsOffTheHost == (asleep /\ moving = "no" /\ cleaned) => local = "none"

\* PA-2: a task's socket and processes go with it; the computer stays.
PA2_ATaskLeavesNoSocketOrProcess ==
    \A t \in Tasks : (up /\ tile[t] = "done" /\ cleaned) => ~sock[t] /\ ~proc[t]

\* An agent runs only on its computer awake, never while it's saved or
\* restored.
RunsOnlyOnAnAwakeComputer ==
    \A t \in Tasks : proc[t] => (~asleep /\ local = "whole")

\* A computer asleep can always be woken.
WakeAlwaysWorks == ~stuck

=============================================================================
