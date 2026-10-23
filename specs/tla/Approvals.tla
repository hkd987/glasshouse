---------------------------- MODULE Approvals ----------------------------
(***************************************************************************)
(* Approvals and the task lifecycle (DA-11, DA-13, AW-12, AW-20..22, PA-2, *)
(* PM-5), as glassd runs them: one task, its agent's concurrent Ask-tier   *)
(* calls, the person answering and pressing the tile's buttons (pause,     *)
(* take over, resume, hand back, stop), the agent exiting, the             *)
(* supervisor's teardown, the approval timeout, and the machine going to   *)
(* sleep and waking.                                                       *)
(*                                                                         *)
(* The code, step by step (each action names its step):                    *)
(*  - crates/svc/runtime/src/execute.rs  pipeline (wait_runnable, decide,  *)
(*    has_task_grant, ask, the commit loop with InFlight); ask (open,      *)
(*    "Needs you", the answer or the timeout, back_to_work, hold on a      *)
(*    timeout)                                                             *)
(*  - crates/svc/broker/src/desk.rs      ApprovalDesk: open, answer,       *)
(*    expire, end_task, has_task_grant (one lock: each call one step)      *)
(*  - crates/svc/runtime/src/runtime.rs  hold, release, live, pause,       *)
(*    resume, take_over, hand_back, stop/end_task, pause_for_sleep,        *)
(*    resume_after_sleep                                                   *)
(*  - crates/svc/runtime/src/supervise.rs run: the teardown after the      *)
(*    agent exits or is stopped                                            *)
(*  - crates/svc/runtime/src/task.rs     the gate (Run, Hold, Ended)       *)
(* One step is what the code does under the task lock, under the desk      *)
(* lock, or in one change of the gate (a tokio watch); the rest            *)
(* interleaves.                                                            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Calls,  \* the agent's calls, each an Ask-tier call of the same action
    App,    \* TRUE: an app session (a timeout doesn't pause it; no take over)
    MaxOps, \* how many of the tile's buttons the person presses in a run
    Sleeps  \* TRUE: the machine sleeps and wakes once along the way

Live     == {"Working", "NeedsYou", "Paused", "Driving"}
Terminal == {"Done", "Stopped"}
Answers  == {"none", "once", "task", "denied", "timedout", "withdrawn"}

VARIABLES
    tile,       \* TaskState.tile (setup is done: it starts Working)
    gate,       \* Task.gate
    sleeping,   \* TaskState.sleeping is Some: paused for sleep
    everEnded,  \* history: the gate has been Ended
    pending,    \* the desk's open asks (one per call that asked)
    answer,     \* how each call's ask ended, as the desk closed it
    grant,      \* the desk's grant of the action for the rest of the task
    deskEnded,  \* the desk's end_task ran for the task
    pc,         \* each call's step
    shown,      \* the call's ask has set the tile
    how,        \* why a committed call runs: "once", "task", "grant"
    committed,  \* the call passed the commit point: it runs
    inflight,   \* InFlight counts it (sleep waits for these)
    upc,        \* the person's press under way, step by step
    ops,        \* the person's presses so far
    agent,      \* "alive", "exited" (finished or crashed), "killed" (stopped)
    spc,        \* the supervisor's teardown
    ppc,        \* sleep and wake
    goingToSleep, \* Runtime.going_to_sleep: from pause_for_sleep to the wake
    asleep      \* the machine is asleep: nothing runs

taskVars  == <<tile, gate, sleeping, everEnded>>
deskVars  == <<pending, answer, grant, deskEnded>>
callVars  == <<pc, shown, how, committed, inflight>>
userVars  == <<upc, ops>>
sysVars   == <<agent, spc>>
powerVars == <<ppc, goingToSleep, asleep>>
vars == <<taskVars, deskVars, callVars, userVars, sysVars, powerVars>>

TypeOK ==
    /\ tile \in Live \cup Terminal
    /\ gate \in {"Run", "Hold", "Ended"}
    /\ sleeping \in BOOLEAN /\ asleep \in BOOLEAN
    /\ agent \in {"alive", "exited", "killed"}
    /\ pending \subseteq Calls
    /\ answer \in [Calls -> Answers]
    /\ grant \in BOOLEAN /\ deskEnded \in BOOLEAN
    /\ committed \in [Calls -> BOOLEAN] /\ inflight \in [Calls -> BOOLEAN]
    /\ ops \in 0..MaxOps

Init ==
    /\ tile = "Working" /\ gate = "Run" /\ sleeping = FALSE /\ everEnded = FALSE
    /\ pending = {} /\ answer = [c \in Calls |-> "none"] /\ grant = FALSE
    /\ deskEnded = FALSE
    /\ pc = [c \in Calls |-> "idle"] /\ shown = [c \in Calls |-> FALSE]
    /\ how = [c \in Calls |-> "none"]
    /\ committed = [c \in Calls |-> FALSE] /\ inflight = [c \in Calls |-> FALSE]
    /\ upc = "idle" /\ ops = 0
    /\ agent = "alive" /\ spc = "running"
    /\ ppc = "awake" /\ goingToSleep = FALSE /\ asleep = FALSE

Awake == ~asleep

SetGate(g) ==
    /\ gate' = g
    /\ everEnded' = (everEnded \/ g = "Ended")

(* hold and release: under the task lock, and never for an ended task:   *)
(* its tile isn't live, or its gate says Ended (Task::set_gate keeps an    *)
(* Ended gate as it is).                                                   *)
Holdable == tile \in Live /\ gate # "Ended"

Hold(kind) ==
    IF Holdable
      THEN SetGate("Hold") /\ tile' = kind /\ UNCHANGED sleeping
      ELSE UNCHANGED taskVars

\* While the machine goes to sleep, a release waits for the wake instead.
Release ==
    CASE ~Holdable -> UNCHANGED taskVars
      [] goingToSleep ->
           SetGate("Hold") /\ sleeping' = TRUE /\ tile' = "Paused"
      [] OTHER ->
           /\ SetGate("Run") /\ sleeping' = FALSE
           /\ tile' = IF pending # {} THEN "NeedsYou" ELSE "Working"

(* end_task: under the task lock, the tile (if it hasn't ended meanwhile), *)
(* nothing left to wake, and the gate Ended.                                *)
EndTask(kind) ==
    /\ tile' = IF tile \in Live THEN kind ELSE tile
    /\ sleeping' = FALSE
    /\ SetGate("Ended")

(* The desk's end_task: every open ask on the task withdrawn, its grants   *)
(* forgotten.                                                              *)
DeskEndTask ==
    /\ answer' = [c \in Calls |-> IF c \in pending THEN "withdrawn" ELSE answer[c]]
    /\ pending' = {}
    /\ grant' = FALSE
    /\ deskEnded' = TRUE

------------------------------------------------------------------------------
(* An agent's call: the pipeline in execute.rs.                            *)

Step(c, to) == pc' = [pc EXCEPT ![c] = to]

CallStart(c) ==
    /\ Awake /\ agent = "alive" /\ pc[c] = "idle"
    /\ Step(c, "gate1")
    /\ UNCHANGED <<taskVars, deskVars, shown, how, committed, inflight,
                   userVars, sysVars, powerVars>>

\* task.wait_runnable(): waits while Hold; "This task has ended." once Ended.
WaitRunnable(c, from, to) ==
    /\ Awake /\ pc[c] = from /\ gate # "Hold"
    /\ Step(c, IF gate = "Ended" THEN "done" ELSE to)
    /\ UNCHANGED <<taskVars, deskVars, shown, how, committed, inflight,
                   userVars, sysVars, powerVars>>

\* Preview and decide say Ask; desk.has_task_grant decides whether to ask.
Check(c) ==
    /\ Awake /\ pc[c] = "check"
    /\ IF grant
         THEN Step(c, "commit") /\ how' = [how EXCEPT ![c] = "grant"]
         ELSE Step(c, "open") /\ UNCHANGED how
    /\ UNCHANGED <<taskVars, deskVars, shown, committed, inflight, userVars,
                   sysVars, powerVars>>

\* ask: desk.open. An ask on a task the desk has ended is withdrawn at
\* once (its call was on its way as the task ended).
Open(c) ==
    /\ Awake /\ pc[c] = "open"
    /\ IF deskEnded
         THEN answer' = [answer EXCEPT ![c] = "withdrawn"] /\ UNCHANGED pending
         ELSE pending' = pending \cup {c} /\ UNCHANGED answer
    /\ Step(c, "show")
    /\ UNCHANGED <<taskVars, grant, deskEnded, shown, how, committed,
                   inflight, userVars, sysVars, powerVars>>

\* ask: under the task lock, Working becomes Needs you.
Show(c) ==
    /\ Awake /\ pc[c] = "show"
    /\ tile' = IF tile = "Working" THEN "NeedsYou" ELSE tile
    /\ shown' = [shown EXCEPT ![c] = TRUE]
    /\ Step(c, "wait")
    /\ UNCHANGED <<gate, sleeping, everEnded, deskVars, how, committed,
                   inflight, userVars, sysVars, powerVars>>

\* ask: the desk's answer reaches the call (answered, or withdrawn).
Reply(c) ==
    /\ Awake /\ pc[c] = "wait" /\ answer[c] # "none"
    /\ Step(c, "handle")
    /\ UNCHANGED <<taskVars, deskVars, shown, how, committed, inflight,
                   userVars, sysVars, powerVars>>

\* ask: the timeout fires first, and desk.expire closes the ask.
Timeout(c) ==
    /\ Awake /\ pc[c] = "wait" /\ c \in pending
    /\ pending' = pending \ {c}
    /\ answer' = [answer EXCEPT ![c] = "timedout"]
    /\ Step(c, "handle")
    /\ UNCHANGED <<taskVars, grant, deskEnded, shown, how, committed, inflight,
                   userVars, sysVars, powerVars>>

\* back_to_work: under the task lock, Needs you becomes Working unless
\* another of the task's asks is still open (desk.pending_for_task).
BackToWork == tile' = IF tile = "NeedsYou" /\ pending = {} THEN "Working" ELSE tile

Handle(c) ==
    /\ Awake /\ pc[c] = "handle"
    /\ CASE answer[c] \in {"once", "task"} ->
              BackToWork /\ how' = [how EXCEPT ![c] = answer[c]] /\ Step(c, "gate2")
         [] answer[c] = "denied" \/ (answer[c] = "timedout" /\ App) ->
              BackToWork /\ UNCHANGED how /\ Step(c, "done")
         [] answer[c] = "timedout" ->
              \* then hold(task, Paused)
              UNCHANGED <<tile, how>> /\ Step(c, "hold")
         [] answer[c] = "withdrawn" ->
              UNCHANGED <<tile, how>> /\ Step(c, "done")
    /\ UNCHANGED <<gate, sleeping, everEnded, deskVars, shown, committed,
                   inflight, userVars, sysVars, powerVars>>

\* The ask timed out: hold(task, Paused).
TimeoutHold(c) ==
    /\ Awake /\ pc[c] = "hold"
    /\ Hold("Paused")
    /\ Step(c, "done")
    /\ UNCHANGED <<deskVars, shown, how, committed, inflight, userVars,
                   sysVars, powerVars>>

\* The commit loop: counted in flight, and on only while the gate is Run;
\* otherwise back to wait_runnable.
Commit(c) ==
    /\ Awake /\ pc[c] = "commit"
    /\ IF gate = "Run"
         THEN /\ committed' = [committed EXCEPT ![c] = TRUE]
              /\ inflight' = [inflight EXCEPT ![c] = TRUE]
              /\ Step(c, "running")
         ELSE Step(c, "gate2") /\ UNCHANGED <<committed, inflight>>
    /\ UNCHANGED <<taskVars, deskVars, shown, how, userVars, sysVars, powerVars>>

\* The action runs and returns; the call leaves InFlight.
Finish(c) ==
    /\ Awake /\ pc[c] = "running"
    /\ inflight' = [inflight EXCEPT ![c] = FALSE]
    /\ Step(c, "done")
    /\ UNCHANGED <<taskVars, deskVars, shown, how, committed, userVars,
                   sysVars, powerVars>>

------------------------------------------------------------------------------
(* The person: answers on the desktop, and the tile's buttons, each a     *)
(* live() check and then its steps.                                        *)

\* desk.answer: an open ask only; "for this task" grants the action.
Answer(c, a) ==
    /\ Awake /\ c \in pending
    /\ pending' = pending \ {c}
    /\ answer' = [answer EXCEPT ![c] = a]
    /\ grant' = (grant \/ a = "task")
    /\ UNCHANGED <<taskVars, deskEnded, callVars, userVars, sysVars, powerVars>>

Press(to) ==
    /\ Awake /\ upc = "idle" /\ ops < MaxOps /\ tile \in Live
    /\ upc' = to /\ ops' = ops + 1
    /\ UNCHANGED <<taskVars, deskVars, callVars, sysVars, powerVars>>

\* pause, and take_over (not for an app): hold.
Pause == Press("pause")
TakeOver == ~App /\ Press("drive")

UserHold ==
    /\ Awake /\ upc \in {"pause", "drive"}
    /\ Hold(IF upc = "pause" THEN "Paused" ELSE "Driving")
    /\ upc' = "idle"
    /\ UNCHANGED <<deskVars, callVars, ops, sysVars, powerVars>>

\* resume (not while driving) and hand_back (only while driving): release.
Resume == tile # "Driving" /\ Press("release")
HandBack == tile = "Driving" /\ Press("release")

UserRelease ==
    /\ Awake /\ upc = "release"
    /\ Release
    /\ upc' = "idle"
    /\ UNCHANGED <<deskVars, callVars, ops, sysVars, powerVars>>

\* stop: end_task (the tile and the gate under the task lock), the desk,
\* the stop signal to the supervisor.
Stop == Press("stop.end")

StopEnd ==
    /\ Awake /\ upc = "stop.end"
    /\ EndTask("Stopped")
    /\ upc' = "stop.desk"
    /\ UNCHANGED <<deskVars, callVars, ops, sysVars, powerVars>>

StopDesk ==
    /\ Awake /\ upc = "stop.desk"
    /\ DeskEndTask
    /\ upc' = "stop.signal"
    /\ UNCHANGED <<taskVars, callVars, ops, sysVars, powerVars>>

StopSignal ==
    /\ Awake /\ upc = "stop.signal"
    /\ agent' = IF agent = "alive" THEN "killed" ELSE agent
    /\ upc' = "idle"
    /\ UNCHANGED <<taskVars, deskVars, callVars, ops, spc, powerVars>>

------------------------------------------------------------------------------
(* The agent exits on its own (finished or crashed), and the supervisor   *)
(* tears the task down: supervise.rs run.                                  *)

AgentExit ==
    /\ Awake /\ agent = "alive"
    /\ agent' = "exited"
    /\ UNCHANGED <<taskVars, deskVars, callVars, userVars, spc, powerVars>>

TeardownGate ==
    /\ Awake /\ spc = "running" /\ agent # "alive"
    /\ SetGate("Ended")
    /\ spc' = "desk"
    /\ UNCHANGED <<tile, sleeping, deskVars, callVars, userVars, agent,
                   powerVars>>

TeardownDesk ==
    /\ Awake /\ spc = "desk"
    /\ DeskEndTask
    /\ spc' = "tile"
    /\ UNCHANGED <<taskVars, callVars, userVars, agent, powerVars>>

\* The tile as the run ended (Stopped and Done stay; else how it went),
\* and nothing left to wake.
TeardownTile ==
    /\ Awake /\ spc = "tile"
    /\ tile' = CASE tile \in Terminal -> tile
                 [] agent = "killed" -> "Stopped"
                 [] OTHER -> "Done"
    /\ sleeping' = FALSE
    /\ spc' = "over"
    /\ UNCHANGED <<gate, everEnded, deskVars, callVars, userVars, agent,
                   powerVars>>

------------------------------------------------------------------------------
(* Sleep and wake (PM-5): pause_for_sleep, resume_after_sleep. Once.      *)

\* pause_for_sleep: the flag first, then each task.
SleepBegins ==
    /\ Sleeps /\ ppc = "awake"
    /\ goingToSleep' = TRUE /\ ppc' = "pausing"
    /\ UNCHANGED <<taskVars, deskVars, callVars, userVars, sysVars, asleep>>

\* Under the task lock: a running task holds, Paused for sleep; not one
\* whose gate says it ended while its tile still says Working.
SleepPause ==
    /\ ppc = "pausing"
    /\ IF tile \in {"Working", "NeedsYou"} /\ ~sleeping /\ gate # "Ended"
         THEN SetGate("Hold") /\ sleeping' = TRUE /\ tile' = "Paused"
         ELSE UNCHANGED taskVars
    /\ ppc' = "grace"
    /\ UNCHANGED <<deskVars, callVars, userVars, sysVars, goingToSleep, asleep>>

\* After the grace (what's in flight finishes, or two seconds), it sleeps.
Sleep ==
    /\ ppc = "grace"
    /\ asleep' = TRUE /\ ppc' = "asleep"
    /\ UNCHANGED <<taskVars, deskVars, callVars, userVars, sysVars, goingToSleep>>

\* Wake (resume_after_sleep): the flag down, then the task, if still
\* marked sleeping, released.
Wake ==
    /\ ppc = "asleep"
    /\ asleep' = FALSE /\ goingToSleep' = FALSE
    /\ ppc' = IF sleeping THEN "wake.release" ELSE "woke"
    /\ UNCHANGED <<taskVars, deskVars, callVars, userVars, sysVars>>

WakeRelease ==
    /\ ppc = "wake.release"
    /\ Release
    /\ ppc' = "woke"
    /\ UNCHANGED <<deskVars, callVars, userVars, sysVars, goingToSleep, asleep>>

------------------------------------------------------------------------------

CallStep(c) ==
    \/ CallStart(c)
    \/ WaitRunnable(c, "gate1", "check")
    \/ Check(c) \/ Open(c) \/ Show(c) \/ Reply(c) \/ Timeout(c) \/ Handle(c)
    \/ TimeoutHold(c)
    \/ WaitRunnable(c, "gate2", "commit")
    \/ Commit(c) \/ Finish(c)

UserStep ==
    \/ \E c \in Calls, a \in {"once", "task", "denied"} : Answer(c, a)
    \/ Pause \/ TakeOver \/ UserHold
    \/ Resume \/ HandBack \/ UserRelease
    \/ Stop \/ StopEnd \/ StopDesk \/ StopSignal

SystemStep ==
    \/ AgentExit \/ TeardownGate \/ TeardownDesk \/ TeardownTile
    \/ SleepBegins \/ SleepPause \/ Sleep \/ Wake \/ WakeRelease

Next == (\E c \in Calls : CallStep(c)) \/ UserStep \/ SystemStep

\* What has started goes on: a call's steps (the timer fires, answers
\* arrive), the person's press, the teardown, the machine's sleep and wake.
\* Only starting a call or a press, answering, and the agent exiting are
\* free not to happen. One condition per actor keeps the check fast.
CallGoesOn(c) == CallStep(c) /\ pc[c] # "idle"
PressGoesOn == UserHold \/ UserRelease \/ StopEnd \/ StopDesk \/ StopSignal
SystemGoesOn ==
    \/ TeardownGate \/ TeardownDesk \/ TeardownTile
    \/ SleepPause \/ Sleep \/ Wake \/ WakeRelease

Fairness ==
    /\ \A c \in Calls : WF_vars(CallGoesOn(c))
    /\ WF_vars(PressGoesOn) /\ WF_vars(SystemGoesOn)

Spec == Init /\ [][Next]_vars /\ Fairness

------------------------------------------------------------------------------
(* What the OS promises.                                                   *)

\* DA-11: an Ask-tier call runs only on an Approve answer to its own ask,
\* or on a grant for the rest of the task that an Approve gave.
DA11_RunsOnlyApproved ==
    \A c \in Calls : committed[c] =>
        \/ how[c] \in {"once", "task"} /\ answer[c] = how[c]
        \/ how[c] = "grant"
DA11_GrantsComeFromApprovals ==
    grant => \E c \in Calls : answer[c] = "task"

\* PA-2: once the task is torn down, none of its asks is open and none of
\* its grants is left.
PA2_NothingOutlivesTheTask ==
    deskEnded => pending = {} /\ ~grant

\* AW-12: while an ask the tile has shown is open, the tile says Needs you
\* (or the person holds the task, or it has ended).
AW12_NeedsYouWhileAsked ==
    \A c \in Calls :
        (c \in pending /\ shown[c] /\ gate # "Ended")
            => tile \in {"NeedsYou", "Paused", "Driving"} \cup Terminal

\* PM-5: nothing may act while the machine sleeps.
PM5_NothingRunsAsleep ==
    asleep => gate # "Run"

\* An ended task stays ended: its tile, and its gate.
EndedStaysEnded ==
    /\ [][(tile \in Terminal) => tile' = tile]_vars
    /\ [][(gate = "Ended") => gate' = "Ended"]_vars

\* Nothing of a task starts once it has ended.
NothingRunsAfterTheEnd ==
    [][\A c \in Calls : (~committed[c] /\ committed'[c]) => ~everEnded]_vars

\* Each ask is answered once.
AnsweredOnce ==
    [][\A c \in Calls : (answer[c] # "none") => answer'[c] = answer[c]]_vars

\* DA-13: every ask is answered, times out, or is withdrawn.
DA13_EveryAskEnds ==
    \A c \in Calls : (c \in pending) ~> (c \notin pending)

Symmetry == Permutations(Calls)

=============================================================================
