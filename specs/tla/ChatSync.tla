------------------------------- MODULE ChatSync -------------------------------
(***************************************************************************)
(* A conversation's messages reaching every member's computer, what each   *)
(* computer counts as unread, and chat older than the organization keeps   *)
(* going from both (ADR 0030 C1, C4a; CF-91, CF-92, CF-61).                *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/apps/chat/src/store.rs   ChatHandle::sync_once (the logins,    *)
(*                                   the place, the console's answer, then *)
(*                                   each store absorbed and saved and the *)
(*                                   place moved, under the write lock),   *)
(*                                   member (a newcomer's key registered   *)
(*                                   and the place reset), Store::keep,    *)
(*                                   ChatHandle::trim and Store::trim      *)
(*                                   (what is past the rule goes, here and *)
(*                                   from what a sync just absorbed)       *)
(*   crates/apps/chat/src/lib.rs     send_body (a post kept here with the  *)
(*                                   console's sequence number), read (the *)
(*                                   newest view marks the conversation    *)
(*                                   seen, unless it is a `peek`: Chat     *)
(*                                   reading again in a window nobody is   *)
(*                                   looking at), list (unread)            *)
(*   apps/chat/src/main.ts           loadMessages (a peek when the window  *)
(*                                   is hidden or hasn't the keyboard),    *)
(*                                   lookedAgain (a view when it has)      *)
(*   crates/bin/glass-console/src/chat.rs  Chat::post, Chat::sync: every   *)
(*                                   message after `since`, and the        *)
(*                                   console's newest number as the place; *)
(*                                   past_keeping and forget (the          *)
(*                                   console's own copy, by the same       *)
(*                                   periods)                              *)
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
(* The rule (the period IT sets for a conversation of this kind, in its    *)
(* Chat release: ADR 0030, decision 4): messages are numbered in the       *)
(* order the console took them, which is the order they age in, so `past`  *)
(* messages are past the period: numbers 1..past. Time passing is `past`   *)
(* going up; with no period it never does, and the model is the one it     *)
(* was. The console and x go by the same period, each on its own clock     *)
(* and in its own time, so the console's mark, `gone`, never passes `past` *)
(* (TypeOK; it is what TrimConsole may do, not something found). Not       *)
(* here: IT making the period longer (`past` going back: what a computer   *)
(* already removed stays removed, and its place is past it), and a         *)
(* computer that hasn't heard of a shorter period yet (it keeps what it    *)
(* has a while longer, and isn't sent what the console already removed).   *)
(*                                                                         *)
(* Old is the set of the code's behaviours before ADR 0052 that a          *)
(* configuration keeps; {} is the code now.                               *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    MaxPosts,      \* messages posted, all told
    MaxSaveFails,  \* times keeping someone's store fails
    MaxPast,       \* messages that pass the period the organization
                   \*   keeps chat for; 0 is no period, the default
    Old            \* "pastFailedSave": the place moved on when a store
                   \*   couldn't be kept;
                   \* "placeByValue": a sync moved the place when it still
                   \*   read as it began, so a newcomer's reset to 0 was
                   \*   lost when the sync had read 0 too (begun at 0, or
                   \*   reading the place just after the reset);
                   \* "seenAtNewest": the newest view marked seen up to the
                   \*   newest message here, the person's own post included,
                   \*   even past what had been fetched;
                   \* "seenUnlooked": Chat reading the open conversation
                   \*   again at a change marked it seen though its window
                   \*   was behind another, and showed the person nothing;
                   \* "keptAgain": never the code, the mistake the rule's
                   \*   check at a sync is there for: a sync keeps whatever
                   \*   the console answers with, so a newcomer's fetch from
                   \*   the beginning puts back what the rule removed

People    == {"dana", "sam", "priya"}
Here      == {"dana", "sam"}         \* x's people
Starters  == {"dana"}                \* with a key on x from the start
Threads   == {"poll", "post"}

Idle == [at |-> "idle", gen |-> 0, logins |-> {}, since |-> 0, low |-> 0,
         upto |-> 0]

VARIABLES
    log,         \* the console's messages: log[s] posted seq s
    registered,  \* who has a key on x (store.registered)
    cursor,      \* x's place, on disk
    resets,      \* the place reset for a newcomer, in memory
    store,       \* store[p]: the seqs in p's store on x, as saved
    seen,        \* seen[p]: store.seen for the conversation
    shown,       \* ghost: others' messages p's Chat has shown p
    sync,        \* sync[t]: a sync_once under way
    fails,       \* stores that couldn't be kept, so far
    past,        \* messages 1..past are past the rule on x
    gone,        \* messages 1..gone are removed from the console
    trimmed      \* ghost: trimmed[p]: the rule has been applied to p's
                 \*   store up to this message

vars == <<log, registered, cursor, resets, store, seen, shown, sync, fails,
          past, gone, trimmed>>

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
    /\ past \in 0..Len(log)
    /\ gone \in 0..past
    /\ trimmed \in [Here -> 0..past]

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
    /\ past = 0
    /\ gone = 0
    /\ trimmed = [p \in Here |-> 0]

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
    /\ UNCHANGED <<registered, cursor, resets, seen, shown, sync, fails,
                   past, gone, trimmed>>

PostElsewhere ==
    /\ Len(log) < MaxPosts
    /\ log' = Append(log, "priya")
    /\ UNCHANGED <<registered, cursor, resets, store, seen, shown, sync, fails,
                   past, gone, trimmed>>

\* member(): sam opens Chat on x. His new key is registered, his store
\* saved, and the place reset to 0 so his conversations come from the
\* beginning; all under the write lock.
Register(p) ==
    /\ p \notin registered
    /\ registered' = registered \cup {p}
    /\ cursor' = 0
    /\ resets' = resets + 1
    /\ UNCHANGED <<log, store, seen, shown, sync, fails, past, gone, trimmed>>

\* chat.read from the Chat app, newest messages: the store and the place
\* are read (the place first, now), the conversation folded and shown,
\* then store.seen moved on under the write lock. Reading the place before
\* the store is the same as both at once: a sync between only adds to the
\* store, and a store loses nothing but what is past the rule.
View(p) ==
    LET newest == MaxOf(store[p])
        mark   == IF "seenAtNewest" \in Old THEN newest
                  ELSE Min(newest, cursor)
    IN /\ p \in registered
       /\ shown' = [shown EXCEPT ![p] = @ \cup {s \in store[p] : log[s] # p}]
       /\ seen' = [seen EXCEPT ![p] = Max(@, mark)]
       /\ UNCHANGED <<log, registered, cursor, resets, store, sync, fails,
                      past, gone, trimmed>>

\* chat.read with `peek`: Chat reads the open conversation again at a
\* change, in a window that is hidden or hasn't the keyboard. It draws the
\* messages and shows them to nobody, so nothing is seen; when the person
\* is back, Chat reads again and that is a View.
Peek(p) ==
    /\ p \in registered
    /\ seen' = IF "seenUnlooked" \in Old
               THEN [seen EXCEPT ![p] = Max(@, Min(MaxOf(store[p]), cursor))]
               ELSE seen
    /\ UNCHANGED <<log, registered, cursor, resets, store, shown, sync, fails,
                   past, gone, trimmed>>

-----------------------------------------------------------------------------
\* sync_once on x, as thread t.

\* The reset count (now), then the logins with a key here.
SyncStart(t) ==
    /\ sync[t].at = "idle"
    /\ registered # {}
    /\ sync' = [sync EXCEPT ![t] = [Idle EXCEPT !.at = "logins",
                                     !.gen = resets,
                                     !.logins = registered]]
    /\ UNCHANGED <<log, registered, cursor, resets, store, seen, shown, fails,
                   past, gone, trimmed>>

\* Then the place, as `since`, and the request goes.
SyncAsk(t) ==
    /\ sync[t].at = "logins"
    /\ sync' = [sync EXCEPT ![t].at = "asked", ![t].since = cursor]
    /\ UNCHANGED <<log, registered, cursor, resets, store, seen, shown, fails,
                   past, gone, trimmed>>

\* The console answers, now or after its wait: every message after `since`
\* in the conversation (everyone asked about is in it) that it still has,
\* and its newest number as the place. The console appends, and removes
\* from the old end only, so the answer is the messages after `low` up to
\* `upto`.
SyncAnswer(t) ==
    /\ sync[t].at = "asked"
    /\ sync' = [sync EXCEPT ![t].at = "answered",
                            ![t].low = Max(sync[t].since, gone),
                            ![t].upto = Len(log)]
    /\ UNCHANGED <<log, registered, cursor, resets, store, seen, shown, fails,
                   past, gone, trimmed>>

\* Under the write lock: each person asked about absorbs the answer, what
\* is past the rule by now goes from their store (Store::trim, so nothing
\* the rule removed is kept again), and the store is saved, or isn't
\* (`failed`); then the place moves.
SyncAbsorb(t) ==
    LET r   == sync[t]
        got == {s \in Seqs : r.low < s /\ s <= r.upto}
        kept(failed) == failed = {} \/ "pastFailedSave" \in Old
        saved(p, failed) == p \in r.logins /\ p \notin failed
    IN /\ r.at = "answered"
       /\ \E failed \in SUBSET r.logins :
            /\ fails + Cardinality(failed) <= MaxSaveFails
            /\ fails' = fails + Cardinality(failed)
            /\ store' = [p \in Here |->
                           IF ~saved(p, failed) THEN store[p]
                           ELSE IF "keptAgain" \in Old THEN store[p] \cup got
                           ELSE (store[p] \cup got) \ (1..past)]
            /\ trimmed' = [p \in Here |->
                             IF saved(p, failed) /\ "keptAgain" \notin Old
                             THEN past ELSE trimmed[p]]
            /\ cursor' =
                 IF "placeByValue" \in Old
                 THEN IF cursor = r.since /\ kept(failed) THEN r.upto ELSE cursor
                 ELSE IF /\ resets = r.gen /\ kept(failed)
                         /\ (cursor = r.since \/ r.upto > cursor)
                      THEN r.upto ELSE cursor
       /\ sync' = [sync EXCEPT ![t] = Idle]
       /\ UNCHANGED <<log, registered, resets, seen, shown, past, gone>>

-----------------------------------------------------------------------------
\* The organization's rule for how long chat is kept (CF-61).

\* A day passes: the oldest message the rule still kept on x is past it.
Ages ==
    /\ past < Min(Len(log), MaxPast)
    /\ past' = past + 1
    /\ UNCHANGED <<log, registered, cursor, resets, store, seen, shown, sync,
                   fails, gone, trimmed>>

\* ChatHandle::trim, when IT's periods arrive and every hour: under the
\* write lock, what is past the rule goes from the store of everyone with
\* a key here, and each store that lost something is saved, or isn't
\* (`failed`) and is as it was until the next pass.
TrimHere ==
    LET losing == {p \in registered : store[p] \cap (1..past) # {}}
    IN /\ \E failed \in SUBSET losing :
            /\ fails + Cardinality(failed) <= MaxSaveFails
            /\ fails' = fails + Cardinality(failed)
            /\ store' = [p \in Here |->
                           IF p \in registered /\ p \notin failed
                           THEN store[p] \ (1..past) ELSE store[p]]
            /\ trimmed' = [p \in Here |->
                             IF p \in registered /\ p \notin failed
                             THEN past ELSE trimmed[p]]
       /\ UNCHANGED <<log, registered, cursor, resets, seen, shown, sync,
                      past, gone>>

\* Console::trim_chat, as the console starts, once a day and when IT
\* changes the periods: its own copy loses what is past them. Numbers
\* aren't given out again: the next post still takes the one after the
\* last (Len(log) + 1).
TrimConsole ==
    /\ gone < past
    /\ gone' \in (gone + 1)..past
    /\ UNCHANGED <<log, registered, cursor, resets, store, seen, shown, sync,
                   fails, past, trimmed>>

-----------------------------------------------------------------------------
Next ==
    \/ PostElsewhere
    \/ \E p \in Here : PostHere(p) \/ Register(p) \/ View(p) \/ Peek(p)
    \/ \E t \in Threads :
         SyncStart(t) \/ SyncAsk(t) \/ SyncAnswer(t) \/ SyncAbsorb(t)
    \/ Ages \/ TrimHere \/ TrimConsole

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* What Chat promises.

\* No message is lost: the place never passes a message that someone with
\* a key here hasn't kept, since nothing before the place is fetched
\* again, unless the message is past the organization's rule. With syncs
\* answered, every computer then holds every message the rule keeps, each
\* once (Store::keep keeps a header once).
CF91_NoMessageLost ==
    \A p \in registered : \A s \in (past + 1)..cursor : s \in store[p]

\* What the rule removed from someone's store stays removed: no sync puts
\* it back, though the console may still hold it (its own pass is once a
\* day), an answer may have left before the message aged, and a newcomer's
\* fetch starts from the beginning.
CF61_WhatTheRuleRemovedStaysRemoved ==
    \A p \in Here : \A s \in store[p] : s > trimmed[p]

\* Unread is what Chat hasn't shown: someone else's message this computer
\* holds and never showed the person counts as unread (after store.seen).
CF92_UnreadIsWhatWasntShown ==
    \A p \in Here : \A s \in store[p] :
        (log[s] # p /\ s \notin shown[p]) => s > seen[p]

\* Everything Chat showed, this computer held, and holds still unless the
\* rule removed it.
ShownWasKept ==
    \A p \in Here : shown[p] \subseteq store[p] \cup (1..trimmed[p])
=============================================================================
