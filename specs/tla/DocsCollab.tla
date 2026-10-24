----------------------------- MODULE DocsCollab -----------------------------
(***************************************************************************)
(* One doc edited by two people on two computers at once (ADR 0035; DC-3,  *)
(* DC-4): their pages push and pull, each computer keeps the doc in        *)
(* memory and posts a snapshot of it now and then, the owner changes who   *)
(* edits, a computer loses the network and glassd restarts.                *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/apps/docs/src/lib.rs  with_doc, take_in and absorb (a          *)
(*        computer's copy brought up to date), load (a doc from its log:   *)
(*        its newest snapshot, then what that doesn't cover), push, pull,  *)
(*        maybe_snapshot (and doc.version), roles, open                    *)
(*   crates/apps/chat/src/{lib,store}.rs  the log on each computer:        *)
(*        send_body keeps a message of its own at once; sync_once keeps    *)
(*        everyone's, in order, then moves the cursor (Shared::through)    *)
(*   crates/bin/glass-console/src/chat.rs  one org-wide sequence numbers   *)
(*        every message                                                    *)
(*   apps/docs/src/live.ts  the page: pushes what it changed, keeps it     *)
(*        when that fails, one pull at a time, opens again after glassd    *)
(*        restarted                                                        *)
(*                                                                         *)
(* An edit is a token: Yjs updates merge, so a doc is the set of edits in  *)
(* it. A snapshot is the set its computer had and the sequence number it   *)
(* says it covers (upto). o owns the doc and edits on computer o; b edits  *)
(* on computer b until o says otherwise. One page on each; another window  *)
(* or an agent on the same computer works through the same copy, as a     *)
(* push or a snapshot does. Comments aren't modelled.                      *)
(*                                                                         *)
(* Old is the set of the code's behaviours before ADR 0052 that a          *)
(* configuration keeps; {} is the code now.                                *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Typists,      \* the computers whose page types
    MaxEdits,     \* edits typed on each of them
    MaxSnaps,     \* snapshots posted, by either
    MaxRoles,     \* times the owner changes whether b edits
    MaxRestarts,  \* times glassd restarts, on either computer
    MaxOffline,   \* times a computer loses the network
    Old           \* "pastGaps": a copy's and a page's cursor was the newest
                  \*   message read, while another computer's earlier one
                  \*   could still be on its way;
                  \* "byPosition": load skipped every edit before the newest
                  \*   snapshot in the log, not the ones it covers;
                  \* "finalRoles": load judged that snapshot's sender by who
                  \*   edits now, not when it was posted;
                  \* "rolesNow": pull judged each update by who edits now;
                  \* "keepRefused": a copy kept its own person's edit that
                  \*   the log refused (they had just stopped editing)

Computers == {"o", "b"}
Owner == "o"

VARIABLES
    log,          \* the console's messages, by sequence number
    have,         \* the sequence numbers each computer keeps
    synced,       \* each computer's sync cursor: all up to it is kept
    up,           \* each computer reaches the console
    cache,        \* each computer's copy: seq (all up to it taken in, in
                  \* order), ahead (its own posts taken in past seq), cont,
                  \* bob (whether b edits, as it knows) and refused
    page,         \* each computer's page
    typed, snaps, roleChanges, restarts, offs

vars == <<log, have, synced, up, cache, page, typed, snaps, roleChanges,
          restarts, offs>>

Rec(kind, by, eids, upto, bob) ==
    [kind |-> kind, by |-> by, eids |-> eids, upto |-> upto, bob |-> bob]
Max(S) == CHOOSE x \in S : \A y \in S : y <= x
Min(S) == CHOOSE x \in S : \A y \in S : x <= y
All == 1..Len(log)

-----------------------------------------------------------------------------
\* absorb: one message at a time, in order. The owner always edits; b
\* while the last Roles message says so. A copy starts with no editors.

Copy0 == [seq |-> 0, ahead |-> {}, cont |-> {}, bob |-> FALSE, refused |-> {}]

Edits(p, k) == p = Owner \/ k.bob

Absorb1(k, s) ==
    LET r == log[s] IN
    IF r.kind = "roles" THEN IF r.by = Owner THEN [k EXCEPT !.bob = r.bob] ELSE k
    ELSE IF Edits(r.by, k) THEN [k EXCEPT !.cont = @ \cup r.eids]
    ELSE [k EXCEPT !.refused = @ \cup {s}]

RECURSIVE Fold(_, _)
Fold(k, S) == IF S = {} THEN k ELSE LET s == Min(S) IN Fold(Absorb1(k, s), S \ {s})

\* What everyone should end up with: the whole log, in order.
Truth == Fold(Copy0, All)
Accepted == Truth.cont
TruthUpTo(u) == Fold(Copy0, 1..u).cont
Logged == UNION {log[s].eids : s \in {t \in All : log[t].kind = "upd"}}

-----------------------------------------------------------------------------
\* load: the newest snapshot its sender could post then, and after it
\* what it doesn't cover; roles always. Every update is judged, in order,
\* covered or not, for pull.

RoleAt(P, s) ==
    LET R == {t \in P : t < s /\ log[t].kind = "roles" /\ log[t].by = Owner}
    IN IF R = {} THEN FALSE ELSE log[Max(R)].bob

SnapOk(P, s) ==
    /\ log[s].kind = "snap"
    /\ \/ log[s].by = Owner
       \/ IF "finalRoles" \in Old THEN RoleAt(P, Max(P) + 1) ELSE RoleAt(P, s)

Base(P) == LET G == {s \in P : SnapOk(P, s)} IN IF G = {} THEN 0 ELSE Max(G)

Skip(P, s) ==
    LET b == Base(P) IN
    /\ b # 0
    /\ log[s].kind # "roles"
    /\ IF "byPosition" \in Old THEN s < b
       ELSE \/ log[s].kind = "upd" /\ s <= log[b].upto
            \/ log[s].kind = "snap" /\ s < b

\* Since ADR 0052 a copy's seq is the sync cursor, read before the log
\* (Shared::through): nothing up to it can still arrive. Its own posts
\* past it are ahead. Before, seq was the newest message.
LoadOf(P, through) ==
    LET k0 == [Copy0 EXCEPT !.bob = "finalRoles" \in Old /\ RoleAt(P, Max(P) + 1)]
        k == [Fold(k0, {s \in P : ~Skip(P, s)}) EXCEPT !.refused = Fold(k0, P).refused]
    IN IF "pastGaps" \in Old THEN [k EXCEPT !.seq = Max(P)]
       ELSE [k EXCEPT !.seq = through, !.ahead = {s \in P : s > through}]

Load(c) == LoadOf(have[c], synced[c])

\* with_doc: since(seq), absorbed. Messages arrive in order but for a
\* computer's own posts, kept at once (ahead). One from before those
\* arriving after them merges the same (Yjs), and is judged the same, unless
\* a Roles message is among them: then the copy is loaded again, in order.
\* So is a copy that refused an edit of its own person's (posted just after
\* they stopped editing): its Yjs document holds it.
Refreshed(c) ==
    LET k == cache[c]
        through == synced[c]
        read == {s \in have[c] : s > k.seq /\ s \notin k.ahead}
        late == k.ahead # {} /\ \E s \in read : s < Max(k.ahead)
        roles == \E s \in read \cup k.ahead : log[s].kind = "roles"
        k1 == Fold(k, read)
        k2 == IF "pastGaps" \in Old THEN [k1 EXCEPT !.seq = Max({k.seq} \cup read)]
              ELSE LET seq == Max({k.seq, through}) IN
                   [k1 EXCEPT !.seq = seq, !.ahead = {s \in k.ahead \cup read : s > seq}]
        stale == \E s \in k2.refused \ k.refused : log[s].by = c
        L == Load(c)
    IN IF \/ "pastGaps" \notin Old /\ late /\ roles
          \/ "keepRefused" \notin Old /\ stale
       THEN L
       ELSE k2

Init ==
    /\ log = << Rec("roles", Owner, {}, 0, TRUE) >>
    /\ have = [c \in Computers |-> {1}]
    /\ synced = [c \in Computers |-> 1]
    /\ up = [c \in Computers |-> TRUE]
    /\ cache = [c \in Computers |-> [seq |-> 1, ahead |-> {}, cont |-> {}, bob |-> TRUE,
                                     refused |-> {}]]
    /\ page = [c \in Computers |->
                [open |-> TRUE, srole |-> TRUE, cont |-> {}, cursor |-> 1,
                 pending |-> {}, stopped |-> FALSE, posted |-> {}]]
    /\ typed = [c \in Computers |-> 0]
    /\ snaps = 0 /\ roleChanges = 0 /\ restarts = 0 /\ offs = 0

-----------------------------------------------------------------------------
\* The console and the computers' logs.

\* send_body: the console numbers the message, the computer keeps it.
Post(c, r) ==
    /\ log' = Append(log, r)
    /\ have' = [have EXCEPT ![c] = @ \cup {Len(log) + 1}]

\* sync_once: everything since the cursor, kept, then the cursor moved.
Sync(c) ==
    /\ up[c] /\ synced[c] # Len(log)
    /\ have' = [have EXCEPT ![c] = @ \cup All]
    /\ synced' = [synced EXCEPT ![c] = Len(log)]
    /\ UNCHANGED <<log, up, cache, page, typed, snaps, roleChanges, restarts, offs>>

GoOffline(c) ==
    /\ up[c] /\ offs < MaxOffline
    /\ up' = [up EXCEPT ![c] = FALSE] /\ offs' = offs + 1
    /\ UNCHANGED <<log, have, synced, cache, page, typed, snaps, roleChanges, restarts>>

GoOnline(c) ==
    /\ ~up[c] /\ up' = [up EXCEPT ![c] = TRUE]
    /\ UNCHANGED <<log, have, synced, cache, page, typed, snaps, roleChanges, restarts, offs>>

-----------------------------------------------------------------------------
\* glassd's Docs.

\* push: the session's role, then with_doc, the role now, the update
\* applied to the copy and posted. Offline the post fails and the page
\* keeps it; refused, the page drops it and stops, saying why.
Push(c) ==
    /\ page[c].open /\ ~page[c].stopped /\ page[c].pending # {}
    /\ IF ~page[c].srole THEN
          /\ page' = [page EXCEPT ![c].pending = {}, ![c].stopped = TRUE]
          /\ UNCHANGED <<log, have, cache>>
       ELSE
          LET k == Refreshed(c) IN
          IF ~Edits(c, k) THEN
             /\ cache' = [cache EXCEPT ![c] = k]
             /\ page' = [page EXCEPT ![c].pending = {}, ![c].stopped = TRUE]
             /\ UNCHANGED <<log, have>>
          ELSE
             /\ cache' = [cache EXCEPT ![c] = [k EXCEPT !.cont = @ \cup page[c].pending]]
             /\ IF up[c]
                THEN /\ Post(c, Rec("upd", c, page[c].pending, 0, FALSE))
                     /\ page' = [page EXCEPT ![c].pending = {},
                                             ![c].posted = @ \cup {Len(log) + 1}]
                ELSE UNCHANGED <<log, have, page>>
    /\ UNCHANGED <<synced, up, typed, snaps, roleChanges, restarts, offs>>

\* maybe_snapshot after a push or a write (no with_doc between), or
\* doc.version after its with_doc (a Refresh first): the copy's state and
\* its cursor as upto.
Snapshot(c) ==
    /\ snaps < MaxSnaps /\ up[c]
    /\ Edits(c, cache[c])
    /\ Post(c, Rec("snap", c, cache[c].cont, cache[c].seq, FALSE))
    /\ snaps' = snaps + 1
    /\ UNCHANGED <<synced, up, cache, page, typed, roleChanges, restarts, offs>>

\* doc.roles: the owner makes b an editor or a viewer.
SetRoles ==
    /\ roleChanges < MaxRoles /\ up[Owner]
    /\ LET k == Refreshed(Owner) IN
       /\ cache' = [cache EXCEPT ![Owner] = k]
       /\ Post(Owner, Rec("roles", Owner, {}, 0, ~k.bob))
    /\ roleChanges' = roleChanges + 1
    /\ UNCHANGED <<synced, up, page, typed, snaps, restarts, offs>>

\* glassd stops and starts: copies and sessions are gone; the log on disk
\* stays. The copy is loaded from it again.
Restart(c) ==
    /\ restarts < MaxRestarts
    /\ cache' = [cache EXCEPT ![c] = Load(c)]
    /\ page' = [page EXCEPT ![c].open = FALSE]
    /\ restarts' = restarts + 1
    /\ UNCHANGED <<log, have, synced, up, typed, snaps, roleChanges, offs>>

-----------------------------------------------------------------------------
\* The page (live.ts).

Type(c) ==
    /\ c \in Typists
    /\ page[c].open /\ ~page[c].stopped /\ typed[c] < MaxEdits
    /\ LET e == <<c, typed[c] + 1>> IN
       page' = [page EXCEPT ![c].cont = @ \cup {e}, ![c].pending = @ \cup {e}]
    /\ typed' = [typed EXCEPT ![c] = @ + 1]
    /\ UNCHANGED <<log, have, synced, up, cache, snaps, roleChanges, restarts, offs>>

\* pull: with_doc, then what came after the page's cursor, each update as
\* the copy judged it, never one the page has: its own, or one it got
\* ahead of its cursor (another window's on this computer). Before ADR
\* 0052 it judged each by who edits now and moved the cursor to the newest
\* message. The bridge's wait for news is a pull that finds nothing.
Pull(c) ==
    /\ page[c].open
    /\ LET k == Refreshed(c)
           since == page[c].cursor
           fresh == {s \in have[c] : s > since}
           ok(s) == IF "rolesNow" \in Old THEN Edits(log[s].by, k) ELSE s \notin k.refused
           got == {s \in fresh : log[s].kind # "roles" /\ s \notin page[c].posted /\ ok(s)}
           cursor == IF "pastGaps" \in Old THEN Max({since} \cup fresh) ELSE Max({since, k.seq})
       IN /\ <<k, cursor>> # <<cache[c], since>> \/ got # {}
          /\ cache' = [cache EXCEPT ![c] = k]
          /\ page' = [page EXCEPT ![c].cont = @ \cup UNION {log[s].eids : s \in got},
                                  ![c].cursor = cursor,
                                  ![c].posted = @ \cup {s \in got : s > cursor}]
    /\ UNCHANGED <<log, have, synced, up, typed, snaps, roleChanges, restarts, offs>>

\* After a restart: doc.open again, its state merged, and what the copy
\* lacks queued to push.
Reopen(c) ==
    /\ ~page[c].open /\ ~page[c].stopped
    /\ LET k == Refreshed(c) IN
       /\ cache' = [cache EXCEPT ![c] = k]
       /\ page' = [page EXCEPT ![c].open = TRUE, ![c].srole = Edits(c, k),
                               ![c].cont = @ \cup k.cont, ![c].cursor = k.seq,
                               ![c].pending = page[c].cont \ k.cont,
                               ![c].posted = {}]
    /\ UNCHANGED <<log, have, synced, up, typed, snaps, roleChanges, restarts, offs>>

Next ==
    \/ SetRoles
    \/ \E c \in Computers :
        \/ Type(c) \/ Push(c) \/ Snapshot(c) \/ Sync(c)
        \/ Pull(c) \/ Restart(c) \/ Reopen(c)
        \/ GoOffline(c) \/ GoOnline(c)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* What the OS promises.

CaughtUp(c) == synced[c] = Len(log) /\ cache[c].seq = Len(log)

\* DC-3: a computer that opens the doc from its log, once it has the whole
\* log, has every edit the log accepted and nothing else. A snapshot never
\* drops one.
DC3_AFreshReaderHasEveryAcceptedEdit == LoadOf(All, Len(log)).cont = Accepted

\* DC-3: once the network heals and a computer has caught up, its copy is
\* everyone's: every accepted edit, and of its own nothing the log
\* refused (only what it couldn't post yet).
DC3_EveryComputerConverges ==
    \A c \in Computers : CaughtUp(c) =>
        /\ Accepted \subseteq cache[c].cont
        /\ \A e \in cache[c].cont \ Accepted : e[1] = c /\ e \notin Logged

\* DC-3: a page that has pulled everything shows every accepted edit, and
\* nothing of anyone else's that wasn't. (Its own refused edit stays
\* until it's opened again: Yjs can't take it back, and its next push is
\* refused, saying why.)
DC3_EveryPageConverges ==
    \A c \in Computers :
        (CaughtUp(c) /\ page[c].open /\ ~page[c].stopped /\ page[c].cursor = Len(log))
        => /\ Accepted \subseteq page[c].cont
           /\ \A e \in page[c].cont \ Accepted : e[1] = c

\* DC-4: no computer, copy or page, ever shows someone's edit that the
\* log refused because they weren't an editor then.
DC4_NobodyEditsPastTheirRole ==
    \A c \in Computers : \A e \in cache[c].cont \cup page[c].cont :
        e \in Accepted \/ e[1] = c

\* DC-3: a version (any snapshot) holds every edit up to what it says it
\* covers.
DC3_AVersionHoldsWhatItSays ==
    \A s \in All : (log[s].kind = "snap" /\ s \notin Truth.refused)
        => TruthUpTo(log[s].upto) \subseteq log[s].eids

=============================================================================
