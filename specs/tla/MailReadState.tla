--------------------------- MODULE MailReadState ---------------------------
(***************************************************************************)
(* Mail's read state on this computer, kept in step with the person's mail *)
(* server (ADR 0037 section 6, ADR 0050; EM-3, IN-2).                      *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/apps/mail/src/handle.rs   MailHandle::sync, tell_server        *)
(*   crates/apps/mail/src/marks.rs    mark_messages, mark_back             *)
(*   crates/apps/mail/src/moves.rs    move_messages, move_on_server,       *)
(*                                    move_back                            *)
(*   crates/apps/mail/src/cache.rs    keep, remove, rename, set_read       *)
(*   crates/svc/runtime/src/undo.rs   a change's paths put back only when  *)
(*                                    still as the task left them, then    *)
(*                                    the provider's revert hook           *)
(*                                                                         *)
(* The server: each message in one folder under a UID of that folder's     *)
(* UIDVALIDITY, read (\Seen) or not. Another client (the person's phone)   *)
(* marks and moves mail, and a folder's UIDVALIDITY may change.            *)
(* This computer: ~/Mail, a file per message named after its folder's      *)
(* UIDVALIDITY and its UID, and a read marker per message (.read/<file>);  *)
(* glassd's own sync state per folder (where the sync left off) with the   *)
(* marks made here that the server hasn't heard yet.                       *)
(*                                                                         *)
(* A sync holds the person's lock from reading the server to writing the   *)
(* cache: two steps here, since the server, and this computer's moves and  *)
(* undo's file restores (which take no lock), go on in between. A mark,    *)
(* and undo's hook for one, each run under the lock in one step. A move is *)
(* two steps (MOVE on the server, then the file renamed here), and so is   *)
(* undoing one (files put back, then the hook moves it back on the         *)
(* server); so is undoing a mark (marker put back, then the hook).         *)
(*                                                                         *)
(* Fixes names ADR 0051's fixes the code has; {} is the code before them:  *)
(*   found          a mark of a UID the server doesn't have says so        *)
(*   validity       marks and moves name their UIDVALIDITY; the server     *)
(*                  refuses another                                        *)
(*   markBack       undo's hook tells the server what the undo put back    *)
(*   clearWaiting   a mark that reaches the server drops an older one      *)
(*                  waiting for the same message                           *)
(*   moveLocked     a move (and undo's) holds the person's lock            *)
(*   pushBeforeMove a mark waiting goes to the server before a move        *)
(*   undoMarkers    a move's change names the read markers too             *)
(*   moveBackFailure an undo the server refuses leaves the files as the    *)
(*                  move left them                                         *)
(*   heal           a sync finds mail the cache lost or never had          *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Msgs,         \* the messages: 1, 2, ... (their identity, as content)
    Folders,      \* folders that keep read state (Inbox, Archive)
    MaxMoves,     \* moves by this computer, the phone and undo, together
    MaxResets,    \* UIDVALIDITY changes
    MaxUndos,     \* undos of marks and of moves, together
    Fixes         \* which of ADR 0051's fixes the code has

None == [v |-> 0, next |-> 0, exists |-> 0]   \* no sync state yet
NoSync == [f |-> "none"]
NoMove == [m |-> 0]

VARIABLES
    \* The server.
    sLoc, sUid, sSeen, sVal, sNext,
    \* This computer's cache: files [f, v, u, m] and read markers [f, v, u].
    files, readMk,
    \* glassd's sync state per folder, and marks waiting for the server:
    \* sets of [u, s], at most one per UID.
    st, pend,
    \* The world: the server in reach; the person's lock and a sync that
    \* holds it; a move halfway; undo records; undos halfway; bounds.
    online, lock, syn, mv, undoMarks, undoMoves, backMark, backMove,
    moves, resets, undos,
    \* Ghosts: what was decided here about each message and whether
    \* something later superseded it; whether each folder's cache reflects
    \* the server as last read with nothing changed since; a mark or move
    \* that named a UID under another UIDVALIDITY.
    intent, sup, stale, fresh, badPush

vars == <<sLoc, sUid, sSeen, sVal, sNext, files, readMk, st, pend, online,
          lock, syn, mv, undoMarks, undoMoves, backMark, backMove, moves,
          resets, undos, intent, sup, stale, fresh, badPush>>

-----------------------------------------------------------------------------
\* Helpers.

AtUid(f, u) == {m \in Msgs : sLoc[m] = f /\ sUid[m] = u}
InFolder(f) == {m \in Msgs : sLoc[m] = f}
FileNamed(f, v, u) == {x \in files : x.f = f /\ x.v = v /\ x.u = u}
Mk(x) == [f |-> x.f, v |-> x.v, u |-> x.u]
Marked(x) == Mk(x) \in readMk
Max(S) == CHOOSE x \in S : \A y \in S : y <= x

\* UIDs in f numbered from 1 by message, as the server hands them out.
Numbering(f, loc) == [m \in {k \in Msgs : loc[k] = f} |->
                        Cardinality({k \in Msgs : loc[k] = f /\ k <= m})]

\* The server changed in fs other than by this computer keeping in step.
Changed(fs) ==
    /\ stale' = [f \in Folders |-> IF f \in fs THEN TRUE ELSE stale[f]]
    /\ fresh' = [f \in Folders |-> IF f \in fs THEN FALSE ELSE fresh[f]]

\* link.mark of UIDs `us` in f, made under UIDVALIDITY v: the server marks
\* whatever has those UIDs now. If its UIDVALIDITY isn't v, that's other
\* mail.
StoreSeen(f, us, s) ==
    sSeen' = [m \in Msgs |-> IF sLoc[m] = f /\ sUid[m] \in us THEN s ELSE sSeen[m]]

\* The marks waiting in f, as a function of UID.
Waiting(f, u) == {p \in pend[f] : p.u = u}

\* A mark of x made now is superseded from the start when it can only
\* wait (out of reach) and the server hasn't x where this computer has it:
\* another device filed it elsewhere or the folder was numbered afresh
\* first, and that stands (ghost; ADR 0050's offline marks).
Moot(x) == ~online /\ (sVal[x.f] # x.v \/ AtUid(x.f, x.u) = {})

\* The change undo will take back, if any: which one is chosen as it is
\* made (one slot for the marks and the moves together), so the record of
\* every change made needn't be kept.
Record(held, r) ==
    IF undos < MaxUndos /\ undoMarks = {} /\ undoMoves = {}
    THEN {held, {r}} ELSE {held}

-----------------------------------------------------------------------------
Init ==
    /\ sLoc \in [Msgs -> Folders]
    /\ sUid = [m \in Msgs |-> Numbering(sLoc[m], sLoc)[m]]
    /\ sSeen \in [Msgs -> BOOLEAN]
    /\ sVal = [f \in Folders |-> 1]
    /\ sNext = [f \in Folders |-> 1 + Cardinality(InFolder(f))]
    /\ files = {} /\ readMk = {}
    /\ st = [f \in Folders |-> None]
    /\ pend = [f \in Folders |-> {}]
    /\ online = TRUE /\ lock = FALSE /\ syn = NoSync /\ mv = NoMove
    /\ undoMarks = {} /\ undoMoves = {} /\ backMark = NoMove /\ backMove = NoMove
    /\ moves = 0 /\ resets = 0 /\ undos = 0
    /\ intent = [m \in Msgs |-> "none"] /\ sup = [m \in Msgs |-> FALSE]
    /\ stale = [f \in Folders |-> TRUE] /\ fresh = [f \in Folders |-> FALSE]
    /\ badPush = FALSE

-----------------------------------------------------------------------------
\* The world.

GoOffline ==
    /\ online /\ online' = FALSE
    /\ UNCHANGED <<sLoc, sUid, sSeen, sVal, sNext, files, readMk, st, pend,
                   lock, syn, mv, undoMarks, undoMoves, backMark, backMove,
                   moves, resets, undos, intent, sup, stale, fresh, badPush>>

GoOnline ==
    /\ ~online /\ online' = TRUE
    /\ UNCHANGED <<sLoc, sUid, sSeen, sVal, sNext, files, readMk, st, pend,
                   lock, syn, mv, undoMarks, undoMoves, backMark, backMove,
                   moves, resets, undos, intent, sup, stale, fresh, badPush>>

\* The phone marks a message: what was decided here about it is superseded.
PhoneMark(m, s) ==
    /\ sSeen[m] # s
    /\ sSeen' = [sSeen EXCEPT ![m] = s]
    /\ sup' = [sup EXCEPT ![m] = TRUE]
    /\ Changed({sLoc[m]})
    /\ UNCHANGED <<sLoc, sUid, sVal, sNext, files, readMk, st, pend, online,
                   lock, syn, mv, undoMarks, undoMoves, backMark, backMove,
                   moves, resets, undos, intent, badPush>>

\* The phone files a message elsewhere. A mark of it waiting here names
\* its old UID: the phone's move supersedes it.
PhoneMove(m, to) ==
    /\ moves < MaxMoves /\ sLoc[m] # to
    /\ sLoc' = [sLoc EXCEPT ![m] = to]
    /\ sUid' = [sUid EXCEPT ![m] = sNext[to]]
    /\ sNext' = [sNext EXCEPT ![to] = @ + 1]
    /\ moves' = moves + 1
    /\ sup' = [sup EXCEPT ![m] = TRUE]
    /\ Changed({sLoc[m], to})
    /\ UNCHANGED <<sSeen, sVal, files, readMk, st, pend, online, lock, syn,
                   mv, undoMarks, undoMoves, backMark, backMove, resets, undos,
                   intent, badPush>>

\* A folder's UIDVALIDITY changes and its mail is numbered afresh. Nothing
\* kept here under the old one names anything; marks waiting are moot.
Reset(f) ==
    /\ resets < MaxResets
    /\ sVal' = [sVal EXCEPT ![f] = @ + 1]
    /\ sUid' = [m \in Msgs |-> IF sLoc[m] = f THEN Numbering(f, sLoc)[m] ELSE sUid[m]]
    /\ sNext' = [sNext EXCEPT ![f] = Cardinality(InFolder(f)) + 1]
    /\ resets' = resets + 1
    /\ sup' = [m \in Msgs |-> IF sLoc[m] = f THEN TRUE ELSE sup[m]]
    /\ Changed({f})
    /\ UNCHANGED <<sLoc, sSeen, files, readMk, st, pend, online, lock, syn,
                   mv, undoMarks, undoMoves, backMark, backMove, moves, undos,
                   intent, badPush>>

-----------------------------------------------------------------------------
\* The sync of one folder (MailHandle::sync), under the person's lock.

\* Where the sync left off; with no record, what the cache holds.
Since(f) ==
    IF st[f] # None THEN st[f]
    ELSE LET vs == {x.v : x \in {y \in files : y.f = f}}
         IN IF vs = {} THEN None
            ELSE LET v == Max(vs)
                     us == {x.u : x \in {y \in files : y.f = f /\ y.v = v}}
                 IN [v |-> v, next |-> Max(us) + 1, exists |-> Cardinality(us)]

\* link.sync: what the server says, in one read.
SyncRead(f) ==
    /\ online /\ ~lock /\ syn = NoSync
    /\ LET since == Since(f)
           known == {x.u : x \in {y \in files : y.f = f /\ y.v = since.v}}
           reset == since # None /\ since.v # sVal[f]
           here == {[u |-> sUid[m], m |-> m] : m \in InFolder(f)}
           \* UIDNEXT and EXISTS both unmoved: nothing came or went.
           \* Fixed: and the cache holds as many as the server, or a file
           \* lost or left here by this computer would never be put right.
           quiet == since # None /\ ~reset /\ since.next = sNext[f]
                    /\ since.exists = Cardinality(InFolder(f))
                    /\ ("heal" \notin Fixes \/ Cardinality(known) = Cardinality(InFolder(f)))
           \* Fixed: every message the cache lacks, not only those new
           \* since UIDNEXT.
           new == IF since = None \/ reset THEN here
                  ELSE IF quiet THEN {}
                  ELSE {n \in here : ("heal" \in Fixes \/ n.u >= since.next) /\ n.u \notin known}
           vanished == IF since = None \/ reset \/ quiet THEN {}
                       ELSE known \ {n.u : n \in here}
       IN syn' = [f |-> f, v |-> sVal[f], reset |-> reset, new |-> new,
                  vanished |-> vanished,
                  unseen |-> {sUid[m] : m \in {k \in InFolder(f) : ~sSeen[k]}},
                  state |-> [v |-> sVal[f], next |-> sNext[f],
                             exists |-> Cardinality(InFolder(f))]]
    /\ lock' = TRUE
    /\ stale' = [stale EXCEPT ![f] = FALSE]
    /\ UNCHANGED <<sLoc, sUid, sSeen, sVal, sNext, files, readMk, st, pend,
                   online, mv, undoMarks, undoMoves, backMark, backMove, moves,
                   resets, undos, intent, sup, fresh, badPush>>

\* What it writes: files under another UIDVALIDITY go, new ones come,
\* vanished ones go; marks waiting go to the server first (all at once
\* per state, failing together when out of reach); then each marker
\* follows the server, or a mark still waiting.
SyncApply ==
    /\ syn # NoSync
    /\ LET f == syn.f
           v == syn.v
           voided == {x \in files : x.f = f /\ x.v # v}
           gone == {x \in files : x.f = f /\ x.v = v /\ x.u \in syn.vanished}
           kept == {[f |-> f, v |-> v, u |-> n.u, m |-> n.m] :
                      n \in {k \in syn.new : FileNamed(f, v, k.u) = {}}}
           files1 == (files \ (voided \cup gone)) \cup kept
           marks0 == IF syn.reset THEN {} ELSE pend[f]
           \* Refused (the folder's UIDVALIDITY moved since the read):
           \* dropped, the server's state stands. Out of reach: kept.
           refused == online /\ "validity" \in Fixes /\ sVal[f] # v
           pushed == IF online /\ ~refused THEN marks0 ELSE {}
           marks1 == IF refused THEN {} ELSE marks0 \ pushed
           unseen1 == (syn.unseen \ {p.u : p \in {q \in pushed : q.s}})
                      \cup {p.u : p \in {q \in pushed : ~q.s}}
           readNow(x) == IF \E p \in marks1 : p.u = x.u
                         THEN (CHOOSE p \in marks1 : p.u = x.u).s
                         ELSE x.u \notin unseen1
           ours == {x \in files1 : x.f = f /\ x.v = v}
       IN /\ files' = files1
          /\ readMk' = {k \in readMk : k.f # f \/ (k.v # v /\ FileNamed(f, k.v, k.u) \cap files1 # {})}
                       \cup {Mk(x) : x \in {y \in ours : readNow(y)}}
                       \* A marker with no file, before or after, stays (stray).
                       \cup {k \in readMk : k.f = f /\ k.v = v /\ FileNamed(f, v, k.u) = {}
                                /\ \A x \in files1 : ~(x.f = f /\ x.v = v /\ x.u = k.u)}
          /\ IF pushed # {}
             THEN /\ sSeen' = [m \in Msgs |->
                       IF sLoc[m] = f /\ \E p \in pushed : p.u = sUid[m]
                       THEN (CHOOSE p \in pushed : p.u = sUid[m]).s
                       ELSE sSeen[m]]
                  /\ badPush' = (badPush \/ sVal[f] # v)
             ELSE UNCHANGED <<sSeen, badPush>>
          /\ pend' = [pend EXCEPT ![f] = marks1]
          /\ st' = [st EXCEPT ![f] = syn.state]
          /\ fresh' = [fresh EXCEPT ![f] = ~stale[f]]
          \* Marks dropped because the folder was numbered afresh first, or
          \* refused for that: the new UIDVALIDITY superseded them (ghost).
          \* (One pushed where the server has nothing now is superseded only
          \* when another device moved the message: PhoneMove says so.)
          /\ sup' = [m \in Msgs |->
                     IF (syn.reset \/ refused) /\ \E p \in pend[f] : p.m = m
                     THEN TRUE ELSE sup[m]]
    /\ lock' = FALSE /\ syn' = NoSync
    /\ UNCHANGED <<sLoc, sUid, sVal, sNext, online, mv, undoMarks, undoMoves,
                   backMark, backMove, moves, resets, undos, intent, stale>>

-----------------------------------------------------------------------------
\* Telling the server of one message's mark (MailHandle::tell_server),
\* under the lock: in reach, the folder's state as last synced must name
\* x's UIDVALIDITY or it refuses ("list it again"); then link.mark. Out of
\* reach, the mark waits under that UIDVALIDITY. TRUE when it went or
\* waits; FALSE when refused.
Tell(x, s) ==
    \/ /\ online /\ st[x.f] # None /\ st[x.f].v = x.v
       /\ IF \/ "validity" \in Fixes /\ sVal[x.f] # x.v
             \/ "found" \in Fixes /\ AtUid(x.f, x.u) = {}
          THEN FALSE    \* the link refuses: another UIDVALIDITY now, or
                        \* no such message there any more
          ELSE /\ StoreSeen(x.f, {x.u}, s)
               /\ badPush' = (badPush \/ sVal[x.f] # x.v)
               \* Fixed: an older mark of it still waiting is out of date.
               /\ pend' = IF "clearWaiting" \in Fixes
                          THEN [pend EXCEPT ![x.f] = {p \in @ : p.u # x.u}]
                          ELSE pend
    \* Out of reach: the folder's state as last synced is checked first
    \* all the same (it needs no server), then the mark waits.
    \/ /\ ~online /\ st[x.f] # None /\ st[x.f].v = x.v
       /\ pend' = [pend EXCEPT ![x.f] = {p \in @ : p.u # x.u} \cup {[u |-> x.u, s |-> s, m |-> x.m]}]
       /\ UNCHANGED <<sSeen, badPush>>

\* mail.mark of one message (mark_messages), under the lock: the server
\* first, then the marker.
Mark(x, s) ==
    /\ ~lock /\ x \in files /\ Marked(x) # s
    /\ Tell(x, s)
    /\ readMk' = IF s THEN readMk \cup {Mk(x)} ELSE readMk \ {Mk(x)}
    /\ intent' = [intent EXCEPT ![x.m] = IF s THEN "read" ELSE "unread"]
    /\ sup' = [sup EXCEPT ![x.m] = Moot(x)]
    /\ undoMarks' \in Record(undoMarks, [x |-> x, s |-> s])
    /\ UNCHANGED <<sLoc, sUid, sVal, sNext, files, st, online, lock, syn, mv,
                   undoMoves, backMark, backMove, moves, resets, undos, stale,
                   fresh>>

\* Undo of a mark, first: the marker put back as it was, when still as the
\* mark left it (undo.rs); no lock.
UndoMarkHere(r) ==
    /\ r \in undoMarks /\ backMark = NoMove
    /\ undoMarks' = undoMarks \ {r}
    /\ undos' = undos + 1
    /\ IF Marked(r.x) = r.s
       THEN /\ readMk' = IF r.s THEN readMk \ {Mk(r.x)} ELSE readMk \cup {Mk(r.x)}
            /\ backMark' = r
       ELSE UNCHANGED <<readMk, backMark>>
    /\ UNCHANGED <<sLoc, sUid, sSeen, sVal, sNext, files, st, pend, online,
                   lock, syn, mv, undoMoves, backMove, moves, resets, intent,
                   sup, stale, fresh, badPush>>

\* Then the hook (mark_back), under the lock: it tells the server what the
\* marker says now. Fixed: what the undo put back, which the change names.
MarkBack ==
    /\ backMark # NoMove /\ ~lock
    /\ LET r == backMark
           s == IF "markBack" \in Fixes THEN ~r.s ELSE Marked(r.x)
       IN \/ /\ ("markBack" \notin Fixes \/ r.x \in files)
             /\ Tell(r.x, s)
             /\ readMk' = IF "markBack" \in Fixes /\ r.x \in files
                          THEN IF s THEN readMk \cup {Mk(r.x)} ELSE readMk \ {Mk(r.x)}
                          ELSE readMk
             /\ intent' = [intent EXCEPT ![r.x.m] = IF ~r.s THEN "read" ELSE "unread"]
             /\ sup' = [sup EXCEPT ![r.x.m] = Moot(r.x)]
          \* Refused: the change is reported not undone; the person's
          \* decision stands.
          \/ /\ \/ /\ online
                   /\ \/ st[r.x.f] = None
                      \/ st[r.x.f].v # r.x.v
                      \/ ("validity" \in Fixes /\ sVal[r.x.f] # r.x.v)
                      \/ ("found" \in Fixes /\ AtUid(r.x.f, r.x.u) = {})
                \* Fixed: the message isn't in the folder here any more,
                \* or is under a UIDVALIDITY the folder has left behind.
                \/ ("markBack" \in Fixes /\ r.x \notin files)
                \/ ~online /\ (st[r.x.f] = None \/ st[r.x.f].v # r.x.v)
             /\ UNCHANGED <<sSeen, badPush, pend, readMk, intent, sup>>
    /\ backMark' = NoMove
    /\ UNCHANGED <<sLoc, sUid, sVal, sNext, files, st, online, lock, syn, mv,
                   undoMarks, undoMoves, backMove, moves, resets, undos, stale,
                   fresh>>

-----------------------------------------------------------------------------
\* mail.move of one message (move_on_server): no lock. First MOVE on the
\* server, then the file renamed here under the destination's UIDVALIDITY
\* as last synced; the read marker goes with it (cache.rename).

MoveOnServer(x, to) ==
    /\ online /\ mv = NoMove /\ moves < MaxMoves
    \* Fixed: under the person's lock, MOVE to rename, so no mark or sync
    \* comes between.
    /\ "moveLocked" \notin Fixes \/ ~lock
    /\ lock' = ("moveLocked" \in Fixes \/ lock)
    /\ x \in files /\ x.f # to
    /\ st[to] # None                        \* else it syncs `to` first
    /\ st[x.f] # None /\ st[x.f].v = x.v    \* "changed on the server since"
    /\ AtUid(x.f, x.u) # {}                 \* else refused: nothing moves
    /\ "validity" \notin Fixes \/ sVal[x.f] = x.v   \* fixed: else refused
    /\ LET m == CHOOSE k \in AtUid(x.f, x.u) : TRUE
           \* Fixed: a mark of it still waiting goes first, so the MOVE
           \* takes the flag with it.
           waiting == IF "pushBeforeMove" \in Fixes THEN Waiting(x.f, x.u) ELSE {}
       IN /\ sLoc' = [sLoc EXCEPT ![m] = to]
          /\ sUid' = [sUid EXCEPT ![m] = sNext[to]]
          /\ sNext' = [sNext EXCEPT ![to] = @ + 1]
          /\ sSeen' = IF waiting = {} THEN sSeen
                      ELSE [sSeen EXCEPT ![m] = (CHOOSE p \in waiting : TRUE).s]
          /\ pend' = [pend EXCEPT ![x.f] = @ \ waiting]
          /\ mv' = [m |-> x.m, x |-> x, to |-> to, nv |-> st[to].v, nu |-> sNext[to]]
    /\ badPush' = (badPush \/ sVal[x.f] # x.v)
    /\ moves' = moves + 1
    /\ Changed({x.f, to})
    /\ UNCHANGED <<sVal, files, readMk, st, online, syn, undoMarks,
                   undoMoves, backMark, backMove, resets, undos, intent, sup>>

MoveHere ==
    /\ mv # NoMove
    /\ LET x == mv.x
           y == [f |-> mv.to, v |-> mv.nv, u |-> mv.nu, m |-> mv.m]
       IN IF x \in files
          THEN /\ files' = (files \ ({x} \cup FileNamed(y.f, y.v, y.u))) \cup {y}
               /\ readMk' = (readMk \ {Mk(x), Mk(y)}) \cup IF Marked(x) THEN {Mk(y)} ELSE {}
               /\ undoMoves' \in Record(undoMoves, [was |-> x, now |-> y, read |-> Marked(x)])
          \* A sync took the old file meanwhile: "updating this computer's
          \* copy" fails; the destination's next sync brings it.
          ELSE UNCHANGED <<files, readMk, undoMoves>>
    /\ mv' = NoMove
    /\ lock' = IF "moveLocked" \in Fixes THEN FALSE ELSE lock
    /\ UNCHANGED <<sLoc, sUid, sSeen, sVal, sNext, st, pend, online, syn,
                   undoMarks, backMark, backMove, moves, resets, undos, intent,
                   sup, stale, fresh, badPush>>

\* Undo of a move, first: both files put back as the move left them (and,
\* fixed, the read markers, which the change names too), when still so.
UndoMoveHere(r) ==
    /\ backMove = NoMove /\ r \in undoMoves
    /\ undoMoves' = undoMoves \ {r}
    /\ undos' = undos + 1
    /\ IF r.now \in files /\ FileNamed(r.was.f, r.was.v, r.was.u) = {}
          /\ ("undoMarkers" \notin Fixes \/ Marked(r.now) = r.read)
       THEN /\ files' = (files \ {r.now}) \cup {r.was}
            /\ readMk' = IF "undoMarkers" \in Fixes
                         THEN (readMk \ {Mk(r.now)}) \cup IF r.read THEN {Mk(r.was)} ELSE {}
                         ELSE readMk
            /\ backMove' = r
            \* A change here the server hasn't had (yet): the next sync of
            \* each folder checks it whole.
            /\ Changed({r.was.f, r.now.f})
       ELSE UNCHANGED <<files, readMk, backMove, stale, fresh>>
    /\ UNCHANGED <<sLoc, sUid, sSeen, sVal, sNext, st, pend, online, lock, syn,
                   mv, undoMarks, backMark, moves, resets, intent, sup, badPush>>

\* Then the hook (move_back): MOVE back on the server; the restored file
\* takes the UID the server gave it there, when the folder's UIDVALIDITY
\* is still the one it names, or a sync of the folder sorts it out.
MoveBack ==
    /\ backMove # NoMove
    \* Fixed: a move like any other, under the person's lock.
    /\ "moveLocked" \notin Fixes \/ ~lock
    /\ LET r == backMove
           x == r.was
           y == r.now
           can == online /\ st[y.f] # None /\ st[x.f] # None
                  /\ AtUid(y.f, y.u) # {} /\ moves < MaxMoves
                  /\ ("validity" \notin Fixes \/ sVal[y.f] = y.v)
           \* Fixed: a mark of it waiting under its UID there goes first.
           waiting == IF "pushBeforeMove" \in Fixes THEN Waiting(y.f, y.u) ELSE {}
       IN IF can
          THEN LET m == CHOOSE k \in AtUid(y.f, y.u) : TRUE
                   z == [f |-> x.f, v |-> x.v, u |-> sNext[x.f], m |-> x.m]
               IN /\ sLoc' = [sLoc EXCEPT ![m] = x.f]
                  /\ sUid' = [sUid EXCEPT ![m] = sNext[x.f]]
                  /\ sNext' = [sNext EXCEPT ![x.f] = @ + 1]
                  /\ sSeen' = IF waiting = {} THEN sSeen
                              ELSE [sSeen EXCEPT ![m] = (CHOOSE p \in waiting : TRUE).s]
                  /\ pend' = [pend EXCEPT ![y.f] = @ \ waiting]
                  /\ moves' = moves + 1
                  /\ badPush' = (badPush \/ sVal[y.f] # y.v)
                  /\ Changed({x.f, y.f})
                  /\ UNCHANGED sup
                  /\ IF st[x.f].v = x.v /\ x \in files
                     THEN /\ files' = (files \ {x}) \cup {z}
                          /\ readMk' = IF Marked(x) THEN (readMk \ {Mk(x)}) \cup {Mk(z)}
                                       ELSE readMk \ {Mk(z)}
                     ELSE UNCHANGED <<files, readMk>>
          \* The server couldn't move it back: the change is not undone.
          \* Fixed: the files go back to how the move left them, too.
          \* Fixed: the files go back to exactly how the move left it: the
          \* restored file and anything done to it meanwhile go (marks of
          \* it waiting are superseded by the failed undo, ghost), and the
          \* moved file is back with its marker as the move left it, unless
          \* a sync of that folder has the message already, or the folder
          \* was numbered afresh (its sync has it under the new numbers).
          ELSE /\ IF "moveBackFailure" \in Fixes /\ x \in files
                  THEN /\ files' = (files \ {x})
                                  \cup IF st[y.f].v = y.v /\ FileNamed(y.f, y.v, y.u) = {}
                                     THEN {y} ELSE {}
                       /\ readMk' = (readMk \ {Mk(x)})
                                    \cup IF st[y.f].v = y.v /\ FileNamed(y.f, y.v, y.u) = {} /\ r.read
                                       THEN {Mk(y)} ELSE {}
                       /\ pend' = [pend EXCEPT ![x.f] = {p \in @ : p.u # x.u}]
                       /\ sup' = [m \in Msgs |-> sup[m] \/ \E p \in Waiting(x.f, x.u) : p.m = m]
                  ELSE UNCHANGED <<files, readMk, pend, sup>>
               \* This computer's copy, changed without the server: the next
               \* sync of each checks it whole.
               /\ IF "moveBackFailure" \in Fixes /\ x \in files
                  THEN Changed({x.f, y.f})
                  ELSE UNCHANGED <<stale, fresh>>
               /\ UNCHANGED <<sLoc, sUid, sNext, sSeen, moves, badPush>>
    /\ backMove' = NoMove
    /\ UNCHANGED <<sVal, st, online, lock, syn, mv, undoMarks, undoMoves,
                   backMark, resets, undos, intent>>

-----------------------------------------------------------------------------
Next ==
    \/ GoOffline \/ GoOnline
    \/ \E m \in Msgs, s \in BOOLEAN : PhoneMark(m, s)
    \/ \E m \in Msgs, f \in Folders : PhoneMove(m, f)
    \/ \E f \in Folders : Reset(f)
    \/ \E f \in Folders : SyncRead(f)
    \/ SyncApply
    \/ \E x \in files, s \in BOOLEAN : Mark(x, s)
    \/ \E r \in undoMarks : UndoMarkHere(r)
    \/ MarkBack
    \/ \E x \in files, f \in Folders : MoveOnServer(x, f)
    \/ MoveHere
    \/ \E r \in undoMoves : UndoMoveHere(r)
    \/ MoveBack

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* What the OS promises.

\* EM-3: a folder synced since its last change on the server, with no mark
\* waiting and nothing of this computer's halfway, shows each message read
\* exactly when the server has it read, and holds every message the server
\* has there.
Quiet(f) == fresh[f] /\ pend[f] = {} /\ mv = NoMove /\ backMove = NoMove
            /\ backMark = NoMove /\ syn = NoSync

EM3_ReadAsTheServerHasIt ==
    \A f \in Folders : Quiet(f) =>
        \A x \in files : (x.f = f /\ x.v = sVal[f]) =>
            \A m \in AtUid(f, x.u) : Marked(x) = sSeen[m]

EM3_EveryMessageKept ==
    \A f \in Folders : Quiet(f) =>
        \A m \in InFolder(f) : FileNamed(f, sVal[f], sUid[m]) # {}

\* And only those: each file in a quiet folder is the message the server
\* has under its name, so a message is never in two quiet folders. (A
\* folder not synced since another device filed mail away shows it until
\* its next sync: each folder is the server as of its own last sync.)
EM3_OnlyWhatTheServerHas ==
    \A f \in Folders : Quiet(f) =>
        \A x \in files : x.f = f => x.v = sVal[f] /\ AtUid(f, x.u) = {x.m}

\* ADR 0050: a mark made here is never lost. Until something later (the
\* phone's mark or move, a new UIDVALIDITY, an undo) supersedes it, the
\* server has it, or it waits for the server under the message's UID.
\* Offline, a mark waits under the UID this computer has for the message;
\* if the server had already filed it elsewhere (another device, or this
\* computer's own undo halfway: the file back, the server not yet), the
\* server has nothing there when the mark arrives, and the move stands
\* (Moot). A mark lost to this computer's own move is a bug.
ADR0050_MarksAreNeverLost ==
    \A m \in Msgs : (intent[m] # "none" /\ ~sup[m] /\ mv.m # m /\ backMark = NoMove
                     /\ backMove = NoMove) =>
        \/ sSeen[m] = (intent[m] = "read")
        \/ \E f \in Folders : \E p \in pend[f] : p.m = m /\ p.s = (intent[m] = "read")

\* ADR 0050: nothing is marked or moved on the server by a UID under
\* another UIDVALIDITY (it would be another message).
ADR0050_NoPushToAnotherValidity == ~badPush

\* AU-21: undoing a move puts the message back as it was here, read or
\* not, with its file.
AU21_UndoPutsBackReadState ==
    [][backMove = NoMove /\ backMove' # NoMove =>
         (Mk(backMove'.was) \in readMk') = backMove'.read]_vars

\* AU-21: an undo of a move the server couldn't take leaves this computer
\* as the server has it: not back here when it isn't back there.
AU21_AnUndoIsWholeOrNotAtAll ==
    [][backMove # NoMove /\ backMove' = NoMove =>
         \/ sLoc'[backMove.was.m] = backMove.was.f
         \/ backMove.was \notin files']_vars

\* With the server in reach and syncs running, marks waiting reach it.
Fairness ==
    /\ \A f \in Folders : WF_vars(SyncRead(f))
    /\ WF_vars(SyncApply) /\ WF_vars(MoveHere) /\ WF_vars(MoveBack)
    /\ WF_vars(MarkBack) /\ WF_vars(GoOnline)

LiveSpec == Spec /\ Fairness

ADR0050_WaitingMarksReachTheServer ==
    <>[]online => \A f \in Folders : [](pend[f] # {} => <>(pend[f] = {}))

=============================================================================
