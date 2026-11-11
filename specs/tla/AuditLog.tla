------------------------------ MODULE AuditLog ------------------------------
(***************************************************************************)
(* The audit log (AU-1, AU-2, AU-3): each call an agent makes is recorded  *)
(* in a hash-chained file, through a full disk, glassd crashing mid-write  *)
(* and starting again, the index of each task's records, and the readers   *)
(* that run beside the writers: a task's view, `glassctl verify`, and the  *)
(* last day's count glassd reports to the console.                         *)
(*                                                                         *)
(* This is the one file records are written to. That file closed as a      *)
(* segment and a new one begun, and old segments removed under a retention *)
(* rule, is AuditSegments.tla.                                             *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/lib/audit/src/log.rs     AuditLog::open (read_tail, truncate a *)
(*                                   torn tail, log_recovered), append_all *)
(*                                   (a run of records as one write and    *)
(*                                   one fsync under the writer's lock; a  *)
(*                                   failed write leaves the writer        *)
(*                                   unusable, or reopens it, cutting back *)
(*                                   to the last record it acknowledged:   *)
(*                                   cut_to),                              *)
(*                                   offsets (the index: a pass without    *)
(*                                   the writer, then the rest with it),   *)
(*                                   task_records, verify/read             *)
(*                                   (for_each_record), read_since         *)
(*   crates/svc/runtime/src/runtime.rs  Runtime::audit (whoever finds      *)
(*                                   nobody writing writes everything      *)
(*                                   waiting as one run: write_waiting;    *)
(*                                   records that couldn't be written      *)
(*                                   wait, in order), audit_healthy        *)
(*   crates/svc/runtime/src/execute.rs  call_tool: the check that the log  *)
(*                                   is healthy, a change's record written *)
(*                                   before it runs (audit_ahead), the     *)
(*                                   provider's invoke, record_with, and   *)
(*                                   what the agent gets; undo.rs writes   *)
(*                                   its own ahead of an undo the same way *)
(*                                                                         *)
(* A line is the record of one event: a change's record written before it  *)
(* runs ("start", ActionStarted), a call's outcome ("end", Action), that a *)
(* change was refused because its start record couldn't be written        *)
(* ("refused"), or a log_recovered note ("lr"). Writing one is two steps a *)
(* reader can fall                                                         *)
(* between: its bytes start to land (the line is unfinished), then the     *)
(* rest and the fsync. Positions stand for byte offsets. Complete lines    *)
(* are never altered: tampering is verify's unit tests' (AU-2), and a last *)
(* record that doesn't match its hash being set aside by open is log.rs's  *)
(* (au2_a_damaged_last_record_...).                                        *)
(*                                                                         *)
(* Records share syncs (audit 9.1): a run is the records of the calls that *)
(* were waiting when the writer was taken (Lock, Join), written as lines   *)
(* in order with one fsync. While it lands, its earlier lines are whole in *)
(* the file and its last unfinished, and none is acknowledged: a reader    *)
(* can see whole lines of a run that then fails. A run that fails fails    *)
(* for every call in it (Cut), and the writer cuts back to the last record *)
(* it acknowledged when it opens the file again, whole lines and all.      *)
(* A write that fails with its last line landed whole (its fsync failed)   *)
(* is the same here as one whose last line is unfinished. (Across a crash  *)
(* the writer's memory of where its last acknowledged record ended is      *)
(* gone, and whole lines stay: Crash.)                                     *)
(*                                                                         *)
(* The calls are an agent's: Reads are reads (no side effect), the rest    *)
(* change something or send data off the computer. Each call runs once:    *)
(* the check, a change's start record, the provider, then its outcome.     *)
(*                                                                         *)
(* Old holds the code before ADR 0053, for showing each fix is needed:     *)
(*   "tornIsCorrupt"     readers took an unfinished last line (a write     *)
(*                       under way, or one a full disk cut short) for      *)
(*                       corruption                                        *)
(*   "poisonedForever"   a failed write left the writer unusable until     *)
(*                       glassd restarted                                  *)
(*   "unrecordedDropped" a record that couldn't be written was dropped,    *)
(*                       and the call's result went back anyway            *)
(*   "readsWhileDown"    reads ran while the log was down                  *)
(*   "readGiven"         a read whose record couldn't be written still     *)
(*                       gave the agent what it read                       *)
(* before the audit of 2026-11 (4.7):                                      *)
(*   "wholeLineKept"     a write that failed after its whole line landed   *)
(*                       left the line in the file, and the writer chained *)
(*                       onto it; the runtime wrote the record again, so   *)
(*                       it stood in the log twice                         *)
(* and before ADR 0054:                                                    *)
(*   "noWriteAhead"      a change ran before anything of it was on the     *)
(*                       record, so glassd stopping before its outcome     *)
(*                       was written left it off for good                  *)
(*   "refusalNotKept"    the first version of that fix: a start record the *)
(*                       log couldn't take left nothing waiting, so the    *)
(*                       log never counted as down again (TLC: the log     *)
(*                       never recovers), or, as the code then read an     *)
(*                       empty queue, counted as up while the disk was     *)
(*                       still full                                        *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Calls,        \* the agents' calls
    Reads,        \* those that only read
    ReadTask,     \* the task the reads are for, whose records are viewed
    OtherTask,    \* the task of the rest
    MaxFull,      \* times the disk fills
    MaxCrashes,   \* times glassd stops without warning
    MaxViews,     \* times a task's records are asked for
    Old

None == "none"
View == ReadTask
TaskOf == [c \in Calls |-> IF c \in Reads THEN ReadTask ELSE OtherTask]

Line(id, kind, task, done) == [id |-> id, kind |-> kind, task |-> task, done |-> done]
Note == Line("lr", "lr", None, TRUE)

VARIABLES
    file,       \* the log's lines, in order
    up,         \* glassd is running
    wok,        \* the writer is usable (its state is Some)
    wlen,       \* the writer's length: the lines appended and acknowledged
    lock,       \* who holds the writer: None or a call
    run,        \* the calls whose records the holder writes as one run, in order
    call,       \* each call's step
    ahead,      \* the calls whose append under way is their start record
    ran,        \* the calls whose provider ran (ghost)
    saw,        \* calls that saw the log healthy as they were checked (ghost)
    acked,      \* records whose append returned Ok (ghost)
    lost,       \* records glassd lost by stopping before they were written (ghost)
    healthy,    \* the runtime's audit_ok
    pending,    \* records waiting to be written, in order: [id, kind]
    index,      \* the task index: built or not, how far, and its positions
    view,       \* a task's records being read (task_records)
    vfy,        \* verify / read
    rep,        \* read_since for the console's report: "none", "ok", "corrupt"
    full, fulls, crashes

vars == <<file, up, wok, wlen, lock, run, call, ahead, ran, saw, acked, lost, healthy,
          pending, index, view, vfy, rep, full, fulls, crashes>>

NoIndex == [built |-> FALSE, to |-> 0, at |-> <<>>]
NoView == [pc |-> "idle", n |-> 0, must |-> {}, len1 |-> 0, at |-> <<>>, got |-> <<>>]

Range(s) == {s[i] : i \in DOMAIN s}
Last(s) == s[Len(s)]
Unfinished == Len(file) > 0 /\ ~Last(file).done
\* The file without an unfinished last line.
Finished == IF Unfinished THEN SubSeq(file, 1, Len(file) - 1) ELSE file
Recorded(id) == \E i \in DOMAIN file : file[i].id = id /\ file[i].kind = "end" /\ file[i].done
Started(id) == \E i \in DOMAIN file : file[i].id = id /\ file[i].kind = "start" /\ file[i].done
\* Positions of f's lines with a task, from lo to hi, in order.
RECURSIVE TaskLinesIn(_, _, _)
TaskLinesIn(f, lo, hi) ==
    IF lo > hi THEN <<>>
    ELSE IF f[lo].task # None THEN <<lo>> \o TaskLinesIn(f, lo + 1, hi)
    ELSE TaskLinesIn(f, lo + 1, hi)
TaskLines(lo, hi) == TaskLinesIn(file, lo, hi)

\* The writer's lock taken and given back by the steps below; the index
\* is kept up as each line is finished, the writer still held.
Indexed(n) ==
    IF index.built
    THEN [index EXCEPT !.to = n,
                       !.at = IF file'[n].task # None THEN @ \o <<n>> ELSE @]
    ELSE index

Init ==
    /\ file = <<>> /\ up = TRUE /\ wok = TRUE /\ wlen = 0 /\ lock = None /\ run = <<>>
    /\ call = [c \in Calls |-> "idle"] /\ ahead = {} /\ ran = {} /\ saw = {}
    /\ acked = {} /\ lost = {} /\ healthy = TRUE /\ pending = <<>>
    /\ index = NoIndex /\ view = NoView /\ vfy = [pc |-> "idle", pos |-> 0]
    /\ rep = "none" /\ full = FALSE /\ fulls = 0 /\ crashes = 0

-----------------------------------------------------------------------------
(* A call: execute.rs refuses it while the log is down (only changes, in   *)
(* the old code), runs the provider, then appends its record.             *)

Check(c) ==
    /\ up /\ call[c] = "idle"
    /\ LET refused == ~healthy /\ (c \notin Reads \/ "readsWhileDown" \notin Old)
           first == c \notin Reads /\ "noWriteAhead" \notin Old
       IN /\ call' = [call EXCEPT ![c] = IF refused THEN "refused"
                                         ELSE IF first THEN "append" ELSE "checked"]
          /\ ahead' = IF ~refused /\ first THEN ahead \cup {c} ELSE ahead
    /\ saw' = IF healthy THEN saw \cup {c} ELSE saw
    /\ UNCHANGED <<file, up, wok, wlen, lock, ran, acked, lost, healthy, pending,
                   index, view, vfy, rep, full, fulls, crashes, run>>

\* A read, or a change once its start record is written.
Invoke(c) ==
    /\ up /\ call[c] \in {"checked", "started"}
    /\ ran' = ran \cup {c}
    /\ call' = [call EXCEPT ![c] = "append"]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, saw, acked, lost, healthy, pending,
                   index, view, vfy, rep, full, fulls, crashes, run>>

\* AuditLog::reopen, when a write failed earlier: an unfinished last line
\* (never acknowledged) is cut off and noted. On a full disk the note can't
\* be written and the writer stays unusable (the cut may have happened).
\* A failed run may have left lines past the last record the writer
\* acknowledged: whole ones and, last, an unfinished one. Opening the file
\* again cuts back to that record (cut_to). The old code cut only an
\* unfinished last line ("wholeLineKept").
NeedsCut == IF "wholeLineKept" \in Old THEN Unfinished ELSE Len(file) > wlen
CutBack == IF "wholeLineKept" \in Old THEN Finished ELSE SubSeq(file, 1, wlen)

ReopenOk ==
    /\ ~full
    /\ file' = IF NeedsCut THEN CutBack \o <<Note>> ELSE file
    /\ wok' = TRUE /\ wlen' = Len(file')
    /\ index' = IF NeedsCut THEN Indexed(Len(file')) ELSE index

\* Runtime::audit: whoever finds nobody writing takes the writer for
\* everything that is waiting, its own record among it (a run).
Lock(c) ==
    /\ up /\ call[c] = "append" /\ lock = None
    /\ \/ /\ wok
          /\ lock' = c /\ call' = [call EXCEPT ![c] = "write"] /\ run' = <<c>>
          /\ UNCHANGED <<file, wok, wlen, index>>
       \/ /\ ~wok /\ "poisonedForever" \in Old
          /\ call' = [call EXCEPT ![c] = "failed"]
          /\ UNCHANGED <<file, lock, wok, wlen, index, run>>
       \/ /\ ~wok /\ "poisonedForever" \notin Old /\ ReopenOk
          /\ lock' = c /\ call' = [call EXCEPT ![c] = "write"] /\ run' = <<c>>
       \/ /\ ~wok /\ "poisonedForever" \notin Old /\ full
          /\ file' \in {file, IF NeedsCut THEN CutBack ELSE file}
          /\ call' = [call EXCEPT ![c] = "failed"]
          /\ UNCHANGED <<lock, wok, wlen, index, run>>
    /\ UNCHANGED <<ahead, up, ran, saw, acked, lost, healthy, pending, view, vfy, rep,
                   full, fulls, crashes>>

\* Another call's record was waiting as the writer was taken: it is in the
\* same run, before or after the holder's own.
Join(d) ==
    /\ up /\ call[d] = "append" /\ lock \notin {None, d} /\ call[lock] = "write"
    /\ run' \in {<<d>> \o run, run \o <<d>>}
    /\ call' = [call EXCEPT ![d] = "joined"]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, ran, saw, acked, lost, healthy,
                   pending, index, view, vfy, rep, full, fulls, crashes>>

\* The run's bytes start to land, its lines in order: all but the last are
\* whole in the file, none is acknowledged. On a full disk none land.
Begin(c) ==
    /\ up /\ call[c] = "write" /\ lock = c
    /\ IF full
       THEN /\ wok' = FALSE /\ lock' = None
            /\ call' = [x \in Calls |-> IF x \in Range(run) THEN "failed" ELSE call[x]]
            /\ UNCHANGED file
       ELSE /\ file' = file \o [i \in 1..Len(run) |->
                          Line(run[i], IF run[i] \in ahead THEN "start" ELSE "end",
                               TaskOf[run[i]], i < Len(run))]
            /\ call' = [x \in Calls |-> IF x \in Range(run) THEN "landing" ELSE call[x]]
            /\ UNCHANGED <<wok, lock>>
    /\ UNCHANGED <<ahead, up, wlen, run, ran, saw, acked, lost, healthy, pending, index,
                   view, vfy, rep, full, fulls, crashes>>

\* The rest lands and the run is fsynced, once: every record of it is
\* acknowledged.
End(c) ==
    /\ up /\ call[c] = "landing" /\ lock = c /\ ~full
    /\ file' = [file EXCEPT ![Len(file)].done = TRUE]
    /\ wlen' = Len(file)
    /\ index' = IF index.built
                THEN [built |-> TRUE, to |-> Len(file),
                      at |-> index.at \o TaskLinesIn(file', index.to + 1, Len(file))]
                ELSE index
    /\ lock' = None
    /\ call' = [x \in Calls |-> IF x \notin Range(run) THEN call[x]
                                ELSE IF x \in ahead THEN "started" ELSE "written"]
    /\ ahead' = ahead \ Range(run)
    /\ acked' = acked \cup (Range(run) \ ahead)
    /\ UNCHANGED <<up, wok, run, ran, saw, lost, healthy, pending, view, vfy, rep,
                   full, fulls, crashes>>

\* The disk fills under the run, or its fsync fails with every line landed:
\* the whole run failed, whatever of it is in the file, and each of its
\* calls is told. The writer won't chain onto what is there: it remembers
\* where its last acknowledged record ends and cuts back to it when it
\* opens the file again. It used to leave a whole line and chain onto it
\* ("wholeLineKept").
Cut(c) ==
    /\ up /\ call[c] = "landing" /\ lock = c /\ full
    /\ wok' = FALSE /\ lock' = None
    /\ call' = [x \in Calls |-> IF x \in Range(run) THEN "failed" ELSE call[x]]
    /\ file' \in IF "wholeLineKept" \in Old
                 THEN {file, [file EXCEPT ![Len(file)].done = TRUE]}
                 ELSE {file}
    /\ UNCHANGED <<ahead, up, wlen, run, ran, saw, acked, lost, healthy, pending, index,
                   view, vfy, rep, full, fulls, crashes>>

\* What the agent gets once the record is written.
Given(c) ==
    /\ up /\ call[c] = "written"
    /\ call' = [call EXCEPT ![c] = "given"]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, ran, saw, acked, lost, healthy,
                   pending, index, view, vfy, rep, full, fulls, crashes, run>>

\* Runtime::audit on a failed append: the log is unhealthy. The record
\* waits to be written; a read's result is withheld, since what it read
\* isn't on the record, and a change's result goes back (it happened).
\* A start record that can't be written: the change doesn't run, and that
\* it was refused waits to be written instead (audit_ahead), so the log
\* stays down until there's room.
Failed(c) ==
    /\ up /\ call[c] = "failed"
    /\ healthy' = FALSE
    /\ IF c \in ahead
       THEN /\ call' = [call EXCEPT ![c] = "refused"]
            /\ ahead' = ahead \ {c}
            /\ pending' = IF "refusalNotKept" \in Old THEN pending
                          ELSE Append(pending, [id |-> c, kind |-> "refused"])
       ELSE IF "unrecordedDropped" \in Old
       THEN /\ call' = [call EXCEPT ![c] = "given"]
            /\ UNCHANGED <<pending, ahead, run>>
       ELSE /\ pending' = Append(pending, [id |-> c, kind |-> "end"])
            /\ call' = [call EXCEPT ![c] =
                          IF c \in Reads /\ "readGiven" \notin Old
                          THEN "withheld" ELSE "given"]
            /\ UNCHANGED ahead
    /\ UNCHANGED <<file, up, wok, wlen, lock, ran, saw, acked, lost, index, view,
                   vfy, rep, full, fulls, crashes, run>>

\* Runtime::audit_healthy, and every later audit: the records waiting are
\* written first, in order, as one run, the writer reopened if it must be;
\* with them written, the log is healthy again.
Flush ==
    /\ up /\ lock = None /\ pending # <<>> /\ "unrecordedDropped" \notin Old
    /\ \/ /\ ~full /\ (wok \/ "poisonedForever" \notin Old)
          /\ LET reopened == IF ~wok /\ NeedsCut
                             THEN CutBack \o <<Note>>
                             ELSE file
             IN /\ file' = reopened \o [i \in 1..Len(pending) |->
                              Line(pending[i].id, pending[i].kind,
                                   TaskOf[pending[i].id], TRUE)]
                /\ index' = IF index.built
                            THEN [built |-> TRUE, to |-> Len(file'),
                                  at |-> index.at \o TaskLinesIn(file', index.to + 1, Len(file'))]
                            ELSE index
          /\ wok' = TRUE /\ wlen' = Len(file')
          /\ acked' = acked \cup {pending[i].id : i \in DOMAIN pending}
          /\ pending' = <<>>
          /\ healthy' = TRUE
       \/ /\ full
          /\ wok' = FALSE
          /\ UNCHANGED <<file, index, wlen, acked, pending, healthy>>
    /\ UNCHANGED <<ahead, up, lock, run, call, ran, saw, lost, view, vfy, rep, full, fulls,
                   crashes>>

-----------------------------------------------------------------------------
(* The world.                                                              *)

DiskFills ==
    /\ ~full /\ fulls < MaxFull
    /\ full' = TRUE /\ fulls' = fulls + 1
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, view, vfy, rep, crashes, run>>

\* The person frees space (their own files, not through glassd).
DiskFreed ==
    /\ full /\ full' = FALSE
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, view, vfy, rep, fulls, crashes, run>>

\* glassd stops without warning. Of a run under way any part may be on the
\* disk: none of it, some of its lines whole, the last cut short or whole
\* though never acknowledged. Whatever was in memory is gone: the records
\* waiting, those of calls that ran but weren't written, and where the
\* last acknowledged record ended.
Crash ==
    /\ up /\ crashes < MaxCrashes
    /\ \/ /\ \E c \in Calls : call[c] = "landing"
          /\ file' \in {SubSeq(file, 1, k) : k \in wlen..Len(file)}
                       \cup {[file EXCEPT ![Len(file)].done = TRUE]}
       \/ /\ ~\E c \in Calls : call[c] = "landing"
          /\ UNCHANGED file
    /\ up' = FALSE /\ crashes' = crashes + 1
    /\ lost' = lost \cup {pending[i].id : i \in DOMAIN pending}
               \cup {c \in ran : call[c] \in {"append", "joined", "write", "landing", "failed"}}
    /\ pending' = <<>> /\ lock' = None /\ run' = <<>> /\ wok' = FALSE
    /\ call' = [c \in Calls |-> IF call[c] \in {"given", "withheld", "refused"}
                                THEN call[c] ELSE "dead"]
    /\ ahead' = {}
    /\ index' = NoIndex /\ view' = [NoView EXCEPT !.n = view.n]
    /\ vfy' = [pc |-> IF vfy.pc = "run" THEN "dead" ELSE vfy.pc, pos |-> 0]
    /\ UNCHANGED <<wlen, ran, saw, acked, healthy, rep, full, fulls>>

\* glassd starts: AuditLog::open cuts an unfinished last line off and
\* notes it, then glassd records its start. On a full disk it can't, and
\* doesn't start (fail closed).
Restart ==
    /\ ~up /\ ~full
    /\ file' = IF Unfinished THEN Finished \o <<Note>> ELSE file
    /\ up' = TRUE /\ wok' = TRUE /\ wlen' = Len(file') /\ healthy' = TRUE
    /\ UNCHANGED <<ahead, lock, call, ran, saw, acked, lost, pending, index, view, vfy,
                   rep, full, fulls, crashes, run>>

-----------------------------------------------------------------------------
(* Readers.                                                                *)

\* task_records for View: from the index once it's built; building it is
\* a pass over what the writer had written, without holding it, then the
\* rest with it held.
ViewStart ==
    /\ up /\ view.pc = "idle" /\ view.n < MaxViews
    /\ view' = [view EXCEPT !.pc = IF index.built THEN "read" ELSE "len",
                            !.n = @ + 1,
                            !.must = {c \in acked : TaskOf[c] = View},
                            !.at = index.at]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, vfy, rep, full, fulls, crashes, run>>

ViewLen ==
    /\ up /\ view.pc = "len" /\ lock = None
    /\ view' = IF wok THEN [view EXCEPT !.pc = "scan", !.len1 = wlen]
               ELSE [view EXCEPT !.pc = "error"]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, vfy, rep, full, fulls, crashes, run>>

ViewScan ==
    /\ up /\ view.pc = "scan"
    /\ view' = [view EXCEPT !.pc = "rest", !.at = TaskLines(1, view.len1)]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, vfy, rep, full, fulls, crashes, run>>

ViewRest ==
    /\ up /\ view.pc = "rest" /\ lock = None
    /\ IF wok
       THEN LET at == view.at \o TaskLines(view.len1 + 1, wlen)
            IN /\ index' = [built |-> TRUE, to |-> wlen, at |-> at]
               /\ view' = [view EXCEPT !.pc = "read", !.at = at]
       ELSE /\ view' = [view EXCEPT !.pc = "error"]
            /\ UNCHANGED index
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, vfy, rep, full, fulls, crashes, run>>

ViewRead ==
    /\ up /\ view.pc = "read"
    /\ view' = [view EXCEPT !.pc = "done",
                  !.got = [i \in DOMAIN view.at |-> file[view.at[i]]]]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, vfy, rep, full, fulls, crashes, run>>

ViewAgain ==
    /\ view.pc \in {"done", "error"}
    /\ view' = [NoView EXCEPT !.n = view.n]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, vfy, rep, full, fulls, crashes, run>>

\* verify / read: line by line from the start while writers append.
VerifyStart ==
    /\ up /\ vfy.pc = "idle"
    /\ vfy' = [pc |-> "run", pos |-> 1]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, view, rep, full, fulls, crashes, run>>

VerifyStep ==
    /\ vfy.pc = "run"
    /\ vfy' = IF vfy.pos > Len(file) THEN [vfy EXCEPT !.pc = "ok"]
              ELSE IF file[vfy.pos].done THEN [vfy EXCEPT !.pos = @ + 1]
              ELSE IF "tornIsCorrupt" \in Old THEN [vfy EXCEPT !.pc = "corrupt"]
              ELSE [vfy EXCEPT !.pc = "ok"]
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, view, rep, full, fulls, crashes, run>>

\* read_since, every minute for the console's report: from the end back.
Report ==
    /\ up /\ rep = "none"
    /\ rep' = IF Unfinished /\ "tornIsCorrupt" \in Old THEN "corrupt" ELSE "ok"
    /\ UNCHANGED <<ahead, file, up, wok, wlen, lock, call, ran, saw, acked, lost,
                   healthy, pending, index, view, vfy, full, fulls, crashes, run>>

-----------------------------------------------------------------------------
Next ==
    \/ \E c \in Calls : Check(c) \/ Invoke(c) \/ Lock(c) \/ Join(c) \/ Begin(c) \/ End(c)
                        \/ Cut(c) \/ Given(c) \/ Failed(c)
    \/ Flush \/ DiskFills \/ DiskFreed \/ Crash \/ Restart
    \/ ViewStart \/ ViewLen \/ ViewScan \/ ViewRest \/ ViewRead \/ ViewAgain
    \/ VerifyStart \/ VerifyStep \/ Report

Spec == Init /\ [][Next]_vars

\* The log recovers once there's room again, without a restart.
Fairness ==
    /\ WF_vars(Flush) /\ WF_vars(DiskFreed) /\ WF_vars(Restart)
    /\ \A c \in Calls : WF_vars(Lock(c) \/ Begin(c) \/ End(c) \/ Failed(c))
LiveSpec == Spec /\ Fairness

-----------------------------------------------------------------------------
TypeOK ==
    /\ \A i \in DOMAIN file : file[i].id \in Calls \cup {"lr"}
    /\ \A i \in DOMAIN file : file[i].kind \in {"start", "end", "refused", "lr"}
    /\ ahead \subseteq Calls \ Reads
    /\ healthy \in BOOLEAN /\ wok \in BOOLEAN /\ full \in BOOLEAN
    /\ rep \in {"none", "ok", "corrupt"}

\* AU-2: the chain is never extended past a line a write left unfinished:
\* only the last line can be.
AU2_OnlyTheLastLineCanBeUnfinished ==
    \A i \in 1..(Len(file) - 1) : file[i].done

\* AU-2: verify, and every reader of the whole log, says it's corrupt only
\* if a written record is (never here): not for a write under way, nor for
\* one a full disk or a crash cut short.
AU2_VerifyNeverCallsAGoodLogCorrupt == vfy.pc # "corrupt"
AU2_TheReportReadsAGoodLog == rep # "corrupt"

\* AU-3: a task's records, from the index, are every one acknowledged
\* before they were asked for, once each, in order, and nothing else.
AU3_ATaskHasEveryRecordOnce ==
    view.pc = "done" =>
        /\ \A c \in view.must : \E i \in DOMAIN view.got : view.got[i].id = c
        /\ \A i \in DOMAIN view.got : view.got[i].done
        /\ \A i, j \in DOMAIN view.at : i < j => view.at[i] < view.at[j]

\* AU-1: a call that ran and gave its result is on the record, or its
\* record waits to be written; only glassd stopping first can lose it.
AU1_WhatRanIsRecorded ==
    \A c \in ran : call[c] \in {"given", "withheld"} =>
        \/ Recorded(c) \/ c \in lost
        \/ \E i \in DOMAIN pending : pending[i].id = c /\ pending[i].kind = "end"

\* AU-1: nothing an agent read reaches it unless it's on the record.
AU1_NoUnrecordedReadReachesTheAgent ==
    \A c \in Reads : call[c] = "given" => Recorded(c) \/ c \in lost

\* Fail closed: a call runs only if the log was healthy when it asked.
AU1_NothingRunsWhileTheLogIsDown == ran \subseteq saw

\* AU-1 (ADR 0054): a change runs only once its start record is on disk,
\* so glassd stopping, with its outcome waiting for a full disk or not
\* yet written, can't leave it off the record. Only its outcome can be lost.
AU1_AChangeIsOnRecordBeforeItRuns == \A c \in ran \ Reads : Started(c)

\* A change's outcome, once written, follows its start record.
AU1_AnOutcomeFollowsItsStart ==
    \A i \in DOMAIN file :
        (file[i].kind = "end" /\ file[i].id \notin Reads) =>
            \E j \in 1..(i - 1) :
                file[j].id = file[i].id /\ file[j].kind = "start" /\ file[j].done

\* AU-1: a record is in the log once. A record whose append failed is
\* written again by the runtime, so none of the failed writing may stay.
AU1_NoRecordTwice ==
    \A i, j \in DOMAIN file :
        (i < j /\ file[i].done /\ file[j].done /\ file[i].id # "lr") =>
            ~(file[i].id = file[j].id /\ file[i].kind = file[j].kind)

\* Without a restart, the log takes records again once the disk has room.
AU1_TheLogRecovers == (up /\ ~healthy) ~> (healthy \/ ~up)
=============================================================================
