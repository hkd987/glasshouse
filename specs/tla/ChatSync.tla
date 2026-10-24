------------------------------- MODULE ChatSync -------------------------------
(***************************************************************************)
(* A conversation's messages reaching every member's computer, and what    *)
(* each computer counts as unread (ADR 0030 C1, C4a; CF-91, CF-92).        *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/apps/chat/src/store.rs   ChatHandle::sync_once (the logins,    *)
(*                                   the place, the console's answer, then *)
(*                                   each store absorbed and saved and the *)
(*                                   place moved, under the write lock),   *)
(*                                   member (a newcomer's key registered   *)
(*                                   and the place reset), Store::keep     *)
(*   crates/apps/chat/src/lib.rs     send_body (a post kept here with the  *)
(*                                   console's sequence number), read (the *)
(*                                   newest view marks the conversation    *)
(*                                   seen), list (unread)                  *)
(*   crates/bin/glass-console/src/chat.rs  Chat::post, Chat::sync: every   *)
(*                                   message after `since`, and the        *)
(*                                   console's newest number as the place  *)
(*                                                                         *)
(* One conversation everyone is in, and one computer, x, shared by dana  *)
(* and sam; sam starts Chat during the run, so x registers his key and     *)
(* fetches from the beginning. priya posts from her own computer. Every    *)
(* computer runs the same code against the same console, so x stands for   *)
(* each of them: when each holds every message, each folds the same       *)
(* messages in sequence order (fold.rs) and shows the same conversation.   *)
(* x can run two syncs at once: glassd's long poll and the one a post      *)
(* makes when its epoch was old (send_body). Either can be answered at any *)
(* time, which covers the long poll's wait. Keeping a store can fail (a    *)
(* full disk). A post is one step: the console takes it and its computer   *)
(* keeps it; a sync landing between the two only adds the same message.    *)
(*                                                                         *)
(* Old is the set of the code's behaviours before ADR 0052 that a          *)
(* configuration keeps; {} is the code now.                               *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    MaxPosts,      \* messages posted, all told
    MaxSaveFails,  \* times keeping someone's store fails
    Old            \* "pastFailedSave": the place moved on when a store
                   \*   couldn't be kept;
                   \* "placeByValue": a sync moved the place when it still
                   \*   read as it began, so a newcomer's reset to 0 was
                   \*   lost when the sync had read 0 too (begun at 0, or
                   \*   reading the place just after the reset);
                   \* "seenAtNewest": the newest view marked seen up to the
                   \*   newest message here, the person's own post included,
                   \*   even past what had been fetched

People    == {"dana", "sam", "priya"}
Here      == {"dana", "sam"}         \* x's people
Starters  == {"dana"}                \* with a key on x from the start
Threads   == {"poll", "post"}

Idle == [at |-> "idle", gen |-> 0, logins |-> {}, since |-> 0, upto |-> 0]

VARIABLES
    log,         \* the console's messages: log[s] posted seq s
    registered,  \* who has a key on x (store.registered)
    cursor,      \* x's place, on disk
    resets,      \* the place reset for a newcomer, in memory
    store,       \* store[p]: the seqs in p's store on x, as saved
    seen,        \* seen[p]: store.seen for the conversation
    shown,       \* ghost: others' messages p's Chat has shown p
    sync,        \* sync[t]: a sync_once under way
    fails        \* stores that couldn't be kept, so far

vars == <<log, registered, cursor, resets, store, seen, shown, sync, fails>>

Max(a, b) == IF a >= b THEN a ELSE b
Min(a, b) == IF a <= b THEN a ELSE b
MaxOf(S) == IF S = {} THEN 0 ELSE CHOOSE s \in S : \A t \in S : t <= s
Seqs == 1..Len(log)

TypeOK ==
    /\ Len(log) <= MaxPosts
    /\ \A s \in Seqs : log[s] \in People
    /\ registered \subseteq Here
    /\ cursor \in 0..MaxPosts
    /\ store \in [Here -> SUBSET (1..MaxPosts)]
    /\ seen \in [Here -> 0..MaxPosts]
    /\ fails <= MaxSaveFails

Init ==
    /\ log = <<>>
    /\ registered = Starters
    /\ cursor = 0
    /\ resets = 0
    /\ store = [p \in Here |-> {}]
    /\ seen = [p \in Here |-> 0]
    /\ shown = [p \in Here |-> {}]
    /\ sync = [t \in Threads |-> Idle]
    /\ fails = 0

-----------------------------------------------------------------------------
\* People.

\* chat.post through send_body: the console takes it (Chat::post), and the
\* poster's computer keeps it with its number (Store::keep, then save).
\* priya's computer isn't x.
PostHere(p) ==
    /\ p \in registered
    /\ Len(log) < MaxPosts
    /\ log' = Append(log, p)
    /\ store' = [store EXCEPT ![p] = @ \cup {Len(log) + 1}]
    /\ UNCHANGED <<registered, cursor, resets, seen, shown, sync, fails>>

PostElsewhere ==
    /\ Len(log) < MaxPosts
    /\ log' = Append(log, "priya")
    /\ UNCHANGED <<registered, cursor, resets, store, seen, shown, sync, fails>>

\* member(): sam opens Chat on x. His new key is registered, his store
\* saved, and the place reset to 0 so his conversations come from the
\* beginning; all under the write lock.
Register(p) ==
    /\ p \notin registered
    /\ registered' = registered \cup {p}
    /\ cursor' = 0
    /\ resets' = resets + 1
    /\ UNCHANGED <<log, store, seen, shown, sync, fails>>

\* chat.read from the Chat app, newest messages: the store and the place
\* are read (the place first, now), the conversation folded and shown,
\* then store.seen moved on under the write lock. Reading the place before
\* the store is the same as both at once: a sync between only adds to the
\* store, and stores never lose a message.
View(p) ==
    LET newest == MaxOf(store[p])
        mark   == IF "seenAtNewest" \in Old THEN newest
                  ELSE Min(newest, cursor)
    IN /\ p \in registered
       /\ shown' = [shown EXCEPT ![p] = @ \cup {s \in store[p] : log[s] # p}]
       /\ seen' = [seen EXCEPT ![p] = Max(@, mark)]
       /\ UNCHANGED <<log, registered, cursor, resets, store, sync, fails>>

-----------------------------------------------------------------------------
\* sync_once on x, as thread t.

\* The reset count (now), then the logins with a key here.
SyncStart(t) ==
    /\ sync[t].at = "idle"
    /\ registered # {}
    /\ sync' = [sync EXCEPT ![t] = [Idle EXCEPT !.at = "logins",
                                     !.gen = resets,
                                     !.logins = registered]]
    /\ UNCHANGED <<log, registered, cursor, resets, store, seen, shown, fails>>

\* Then the place, as `since`, and the request goes.
SyncAsk(t) ==
    /\ sync[t].at = "logins"
    /\ sync' = [sync EXCEPT ![t].at = "asked", ![t].since = cursor]
    /\ UNCHANGED <<log, registered, cursor, resets, store, seen, shown, fails>>

\* The console answers, now or after its wait: every message after `since`
\* in the conversation (everyone asked about is in it), and its newest
\* number as the place. The console only appends, so the answer is the
\* messages after `since` up to `upto`.
SyncAnswer(t) ==
    /\ sync[t].at = "asked"
    /\ sync' = [sync EXCEPT ![t].at = "answered", ![t].upto = Len(log)]
    /\ UNCHANGED <<log, registered, cursor, resets, store, seen, shown, fails>>

\* Under the write lock: each person asked about absorbs the answer and
\* their store is saved, or isn't (`failed`); then the place moves.
SyncAbsorb(t) ==
    LET r   == sync[t]
        got == {s \in Seqs : r.since < s /\ s <= r.upto}
        kept(failed) == failed = {} \/ "pastFailedSave" \in Old
    IN /\ r.at = "answered"
       /\ \E failed \in SUBSET r.logins :
            /\ fails + Cardinality(failed) <= MaxSaveFails
            /\ fails' = fails + Cardinality(failed)
            /\ store' = [p \in Here |->
                           IF p \in r.logins /\ p \notin failed
                           THEN store[p] \cup got ELSE store[p]]
            /\ cursor' =
                 IF "placeByValue" \in Old
                 THEN IF cursor = r.since /\ kept(failed) THEN r.upto ELSE cursor
                 ELSE IF /\ resets = r.gen /\ kept(failed)
                         /\ (cursor = r.since \/ r.upto > cursor)
                      THEN r.upto ELSE cursor
       /\ sync' = [sync EXCEPT ![t] = Idle]
       /\ UNCHANGED <<log, registered, resets, seen, shown>>

-----------------------------------------------------------------------------
Next ==
    \/ PostElsewhere
    \/ \E p \in Here : PostHere(p) \/ Register(p) \/ View(p)
    \/ \E t \in Threads :
         SyncStart(t) \/ SyncAsk(t) \/ SyncAnswer(t) \/ SyncAbsorb(t)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* What Chat promises.

\* No message is lost: the place never passes a message that someone with
\* a key here hasn't kept, since nothing before the place is fetched
\* again. With syncs answered, every computer then holds every message,
\* each once (Store::keep keeps a header once).
CF91_NoMessageLost ==
    \A p \in registered : \A s \in 1..cursor : s \in store[p]

\* Unread is what Chat hasn't shown: someone else's message this computer
\* holds and never showed the person counts as unread (after store.seen).
CF92_UnreadIsWhatWasntShown ==
    \A p \in Here : \A s \in store[p] :
        (log[s] # p /\ s \notin shown[p]) => s > seen[p]

\* Everything Chat showed, this computer held.
ShownWasKept == \A p \in Here : shown[p] \subseteq store[p]
=============================================================================
