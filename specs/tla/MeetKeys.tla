------------------------------ MODULE MeetKeys ------------------------------
(***************************************************************************)
(* A meeting's media keys while people join, leave and drop out of the     *)
(* call (CF-21, ADR 0030 §6): someone who left can't open what is said     *)
(* after, everyone in the call opens what everyone else sends, and the     *)
(* console keeps a session's keys no longer than the call.                 *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/lib/managed/src/meet/rooms.rs  Rooms::join, keys, rekey,       *)
(*                                         leave, admitted, connected      *)
(*   crates/bin/glass-console/src/server.rs  meet_join (rooms.join, then   *)
(*                                         the relay, then admitted),      *)
(*                                         meet_leave, the keys route and  *)
(*                                         meetings' sweep (reconcile)     *)
(*   crates/svc/media-relay/src/media.rs   a connection that closes or     *)
(*                                         drops leaves the room, and the  *)
(*                                         others' pages hear "peers"      *)
(*   crates/apps/meet/src/lib.rs           MeetProvider::join (ask for     *)
(*                                         keys, bring a key, join, retry  *)
(*                                         once), current_keys (rekey      *)
(*                                         when due, ask again if refused) *)
(*   apps/meet/src/main.ts                 handle "peers", awaitNewKey,    *)
(*                                         useKeys, leave                  *)
(*                                                                         *)
(* A key is a number; Sealed[k] is whose member keys it is sealed to: who  *)
(* can open it, wherever it travels (the relay is not trusted with media). *)
(* Each console request is one step under the rooms' lock; glassd's calls  *)
(* and the page's loop interleave with everyone else's. A page that drops  *)
(* out (its window closed, its computer asleep or off the network) never   *)
(* says so: the relay sees its connection go. Leaving closes the page's    *)
(* connection first, then tells the console.                               *)
(*                                                                         *)
(* Fixed = FALSE is the code before ADR 0052; TRUE is the code now: the    *)
(* console asks the relay who is still connected before it answers for     *)
(* keys, and on a and takes anyone it seated before asking who      *)
(* isn't as having left; a page asks for keys for as long as one is due.   *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    People,     \* one computer each
    MaxKeys,    \* media keys made, all told
    MaxJoins,   \* joins, all told
    MaxFails,   \* console requests that fail: out of reach
    Asks,       \* times a page asks for keys after a change (before the fix)
    Fixed       \* the code with ADR 0052's fixes

None == [mark |-> 0, live |-> {}, on |-> FALSE]

VARIABLES
    \* The console's rooms (one meeting's session).
    seat,       \* "none", "seated" (relay not done) or "admitted"
    adm,        \* Fixed: the clock when the relay took the seat
    clock,      \* Fixed: Rooms' clock
    cur, prev,  \* the session's media keys, 0 for none
    left,       \* who left holding the current key
    Sealed,     \* Sealed[k]: who key k is sealed to
    made,       \* keys made
    \* The relay.
    conn,       \* who is connected
    \* The pages, and glassd under them.
    page,       \* "out", "keys", "seat", "relay", "in", "leaving"
    bring,      \* the key a join brings, 0 for none
    here,       \* the join found the session's key sealed to it
    tries,      \* the join's attempts
    seatKey,    \* the key the seat came with
    held,       \* keys the page has
    seal,       \* the key the page seals with
    asking,     \* awaitNewKey's asks left (Fixed: Asks while one is due)
    stale,      \* the request in flight is an older loop's
    ask,        \* "idle", "answer", "rekey", "again"
    rekeyKey,   \* the new key the page's glassd brings
    recon,      \* Fixed: a request's reconcile: mark and who the relay had
    joins, fails,
    wasIn,      \* ghost: was in the call
    session,    \* ghost: sessions started
    keySess,    \* ghost: the session each key was current in
    goneSess,   \* ghost: the session they last left
    first       \* ghost: first[k], k started a session (sealed to its people)

vars == <<seat, adm, clock, cur, prev, left, Sealed, made, conn, page, bring,
          here, tries, seatKey, held, seal, asking, stale, ask, rekeyKey, recon,
          joins, fails, wasIn, session, keySess, goneSess, first>>

Keys == 1..MaxKeys
Present == {p \in People : seat[p] # "none"}
Holders == IF cur = 0 THEN {} ELSE Sealed[cur]
Due == cur # 0 /\ ((left \cap Holders) # {} \/ (Present \ Holders) # {})
InCall == {p \in People : page[p] = "in"}
Opens(p) == {k \in {cur, prev} : k # 0 /\ p \in Sealed[k]}

Init ==
    /\ seat = [p \in People |-> "none"] /\ adm = [p \in People |-> 0]
    /\ clock = 0 /\ cur = 0 /\ prev = 0 /\ left = {}
    /\ Sealed = [k \in Keys |-> {}] /\ made = 0
    /\ conn = {}
    /\ page = [p \in People |-> "out"] /\ bring = [p \in People |-> 0]
    /\ here = [p \in People |-> FALSE] /\ tries = [p \in People |-> 0]
    /\ seatKey = [p \in People |-> 0] /\ held = [p \in People |-> {}]
    /\ seal = [p \in People |-> 0] /\ asking = [p \in People |-> 0]
    /\ stale = [p \in People |-> FALSE]
    /\ ask = [p \in People |-> "idle"] /\ rekeyKey = [p \in People |-> 0]
    /\ recon = [p \in People |-> None]
    /\ joins = 0 /\ fails = 0
    /\ wasIn = [p \in People |-> FALSE]
    /\ session = 0 /\ keySess = [k \in Keys |-> 0]
    /\ goneSess = [p \in People |-> 0] /\ first = [k \in Keys |-> FALSE]

-----------------------------------------------------------------------------
(* The rooms' state changes, as one request's step makes them.             *)

\* Rooms: the last one out ends the session and its keys.
Ended(s) == IF \A p \in People : s[p] = "none" THEN 0 ELSE "keep"

\* Rooms::connected (Fixed): seats taken before `rc` asked the relay whose
\* computers the relay no longer has are left, as `leave` would.
Gone(rc) == {p \in People : seat[p] = "admitted" /\ adm[p] <= rc.mark
                            /\ p \notin rc.live}

\* The seats, `left` and keys after reconciling with `rc`.
AfterSeat(rc) == IF Fixed /\ rc.on
                 THEN [p \in People |-> IF p \in Gone(rc) THEN "none" ELSE seat[p]]
                 ELSE seat
AfterLeft(rc) == IF Fixed /\ rc.on THEN left \cup (Gone(rc) \cap Holders) ELSE left

\* Applies a reconcile, then `then` (an action on the reconciled rooms).
\* Written out by each caller: TLA+ has no sequencing, so callers compute
\* from AfterSeat/AfterLeft.
EndIfEmpty(s, l) ==
    IF \A p \in People : s[p] = "none"
    THEN /\ seat' = s /\ cur' = 0 /\ prev' = 0 /\ left' = {}
    ELSE /\ seat' = s /\ left' = l /\ UNCHANGED <<cur, prev>>

\* The relay's "peers" line to the pages in `told`: each starts asking
\* again (a new awaitNewKey); a request already out still comes back.
Told(told) ==
    /\ asking' = [q \in People |-> IF q \in told THEN Asks ELSE asking[q]]
    /\ stale' = [q \in People |-> IF q \in told
                                  THEN ask[q] # "idle" \/ recon[q].on
                                  ELSE stale[q]]

NewKey(to) ==
    /\ made < MaxKeys
    /\ made' = made + 1
    /\ Sealed' = [Sealed EXCEPT ![made + 1] = to]

Fail == fails < MaxFails /\ fails' = fails + 1

-----------------------------------------------------------------------------
(* Joining: MeetProvider::join, the console's meet_join, the relay.        *)

\* The person presses Join.
Join(p) ==
    /\ page[p] = "out" /\ joins < MaxJoins
    /\ ask[p] = "idle" /\ ~recon[p].on
    /\ page' = [page EXCEPT ![p] = "keys"]
    /\ joins' = joins + 1
    /\ tries' = [tries EXCEPT ![p] = 0]
    /\ held' = [held EXCEPT ![p] = {}] /\ seal' = [seal EXCEPT ![p] = 0]
    /\ recon' = [recon EXCEPT ![p] = None]
    /\ UNCHANGED <<seat, adm, clock, cur, prev, left, Sealed, made, conn,
                   bring, here, seatKey, asking, stale, ask, rekeyKey, fails,
                   wasIn, session, keySess, goneSess, first>>

\* glassd asks for the session's keys and makes one to bring: the first
\* (sealed to all the meeting's people), or, when the session's isn't
\* sealed to this computer, a new one sealed to who is present and itself.
JoinKeys(p) ==
    /\ page[p] = "keys"
    \* Fixed: the keys route asks the relay who it has first (taken as one
    \* step here; a page's own asks below take it as two).
    /\ LET rc == IF Fixed THEN [mark |-> clock, live |-> conn, on |-> TRUE] ELSE None
           s == AfterSeat(rc)
           l == AfterLeft(rc)
           present == {q \in People : s[q] # "none"}
           ended == \A q \in People : s[q] = "none"
           c == IF ended THEN 0 ELSE cur
           sealedHere == c # 0 /\ p \in Sealed[c]
       IN /\ EndIfEmpty(s, l)
          /\ here' = [here EXCEPT ![p] = sealedHere]
          /\ IF c = 0
             THEN /\ NewKey(People)
                  /\ bring' = [bring EXCEPT ![p] = made + 1]
             ELSE IF ~sealedHere
             THEN /\ NewKey(present \cup {p})
                  /\ bring' = [bring EXCEPT ![p] = made + 1]
             ELSE /\ bring' = [bring EXCEPT ![p] = 0]
                  /\ UNCHANGED <<made, Sealed>>
    /\ page' = [page EXCEPT ![p] = "seat"]
    /\ UNCHANGED <<adm, clock, conn, tries, seatKey, held, seal, asking,
                   stale, ask, rekeyKey, recon, joins, fails, wasIn, session,
                   keySess, goneSess, first>>

\* The console out of reach: "The meeting can't be reached right now."
JoinUnreachable(p) ==
    /\ page[p] \in {"keys", "seat"} /\ Fail
    /\ page' = [page EXCEPT ![p] = "out"]
    /\ recon' = [recon EXCEPT ![p] = None]
    /\ UNCHANGED <<seat, adm, clock, cur, prev, left, Sealed, made, conn,
                   bring, here, tries, seatKey, held, seal, asking, stale,
                   ask, rekeyKey, joins, wasIn, session, keySess, goneSess,
                   first>>

\* Rooms::join: a seat, with the session's keys. The first joiner's key
\* starts the session; a joiner the current key isn't sealed to brings one
\* sealed to exactly who is present and itself, or is refused.
JoinSeat(p) ==
    /\ page[p] = "seat"
    /\ LET k == bring[p]
           ok == IF cur = 0 THEN k # 0
                 ELSE IF p \notin Holders
                      THEN k # 0 /\ Sealed[k] = Present \cup {p}
                      ELSE TRUE
           replace == ok /\ k # 0 /\ (cur = 0 \/ p \notin Holders)
           c2 == IF replace THEN k ELSE cur
           p2 == IF replace /\ cur # 0 THEN cur ELSE prev
       IN IF ok
          THEN /\ cur' = c2 /\ prev' = p2
               /\ left' = IF replace /\ cur # 0 THEN {} ELSE left
               /\ session' = IF cur = 0 THEN session + 1 ELSE session
               /\ first' = IF cur = 0 THEN [first EXCEPT ![k] = TRUE] ELSE first
               /\ keySess' = IF replace
                             THEN [keySess EXCEPT ![k] = IF cur = 0 THEN session + 1
                                                         ELSE session]
                             ELSE keySess
               /\ seat' = [seat EXCEPT ![p] = "seated"]
               \* open_media: the first, current first, sealed to it.
               /\ seatKey' = [seatKey EXCEPT ![p] =
                                 IF p \in Sealed[c2] THEN c2 ELSE p2]
               /\ page' = [page EXCEPT ![p] = "relay"]
               /\ UNCHANGED tries
          ELSE /\ UNCHANGED <<cur, prev, left, seat, seatKey, session, keySess, first>>
               \* Someone joined or left while it made its key: once more.
               /\ IF tries[p] = 0 /\ ~here[p]
                  THEN /\ page' = [page EXCEPT ![p] = "keys"]
                       /\ tries' = [tries EXCEPT ![p] = 1]
                  ELSE /\ page' = [page EXCEPT ![p] = "out"]
                       /\ UNCHANGED tries
    /\ UNCHANGED <<adm, clock, Sealed, made, conn, bring, here, held, seal,
                   asking, stale, ask, rekeyKey, recon, joins, fails, wasIn,
                   goneSess>>

\* The relay takes the offer; the console marks the seat admitted
\* (Fixed); the page has its key and is in. The others hear "peers".
JoinRelay(p) ==
    /\ page[p] = "relay"
    /\ conn' = conn \cup {p}
    /\ IF Fixed /\ seat[p] # "none"
       THEN /\ seat' = [seat EXCEPT ![p] = "admitted"]
            /\ clock' = clock + 1
            /\ adm' = [adm EXCEPT ![p] = clock + 1]
       ELSE UNCHANGED <<seat, clock, adm>>
    /\ page' = [page EXCEPT ![p] = "in"]
    /\ held' = [held EXCEPT ![p] = {seatKey[p]}]
    /\ seal' = [seal EXCEPT ![p] = seatKey[p]]
    /\ wasIn' = [wasIn EXCEPT ![p] = TRUE]
    \* The others hear "peers"; Fixed: the page itself asks for keys once
    \* it is in, as a key made meanwhile wasn't in its seat.
    /\ Told({q \in InCall : q # p} \cup (IF Fixed THEN {p} ELSE {}))
    /\ UNCHANGED <<cur, prev, left, Sealed, made, bring, here, tries, seatKey,
                   ask, rekeyKey, recon, joins, fails, session, keySess,
                   goneSess, first>>

-----------------------------------------------------------------------------
(* Leaving, and dropping out.                                              *)

\* The window closes, the computer sleeps or loses the network: the relay
\* sees the connection go, and tells the others. Nobody tells the console.
Drop(p) ==
    /\ page[p] = "in"
    /\ conn' = conn \ {p}
    /\ goneSess' = [goneSess EXCEPT ![p] = session]
    /\ page' = [page EXCEPT ![p] = "out"]
    \* The others hear "peers". Its own request out, if any, still lands.
    /\ asking' = [q \in People |-> IF q \in InCall /\ q # p THEN Asks
                                   ELSE IF q = p THEN 0 ELSE asking[q]]
    /\ stale' = [q \in People |-> IF q \in InCall
                                  THEN ask[q] # "idle" \/ recon[q].on
                                  ELSE stale[q]]
    /\ UNCHANGED <<seat, adm, clock, cur, prev, left, Sealed, made, bring,
                   here, tries, seatKey, held, seal, ask, rekeyKey, recon,
                   joins, fails, wasIn, session, keySess, first>>

\* Leave: the page closes its connection (stop()), finishes the
\* transcript's last lines, then tells the console (meet.leave).
LeaveClose(p) ==
    /\ page[p] = "in"
    /\ conn' = conn \ {p}
    /\ goneSess' = [goneSess EXCEPT ![p] = session]
    /\ page' = [page EXCEPT ![p] = "leaving"]
    \* The others hear "peers". Its own request out, if any, still lands.
    /\ asking' = [q \in People |-> IF q \in InCall /\ q # p THEN Asks
                                   ELSE IF q = p THEN 0 ELSE asking[q]]
    /\ stale' = [q \in People |-> IF q \in InCall
                                  THEN ask[q] # "idle" \/ recon[q].on
                                  ELSE stale[q]]
    /\ UNCHANGED <<seat, adm, clock, cur, prev, left, Sealed, made, bring,
                   here, tries, seatKey, held, seal, ask, rekeyKey, recon,
                   joins, fails, wasIn, session, keySess, first>>

\* Rooms::leave: the seat goes; holding the current key, its key is due
\* to change. The console then frees the relay's seat (gone already).
LeaveTell(p) ==
    /\ page[p] = "leaving"
    /\ LET s == [seat EXCEPT ![p] = "none"]
           l == IF seat[p] # "none" /\ p \in Holders THEN left \cup {p} ELSE left
       IN EndIfEmpty(s, l)
    /\ page' = [page EXCEPT ![p] = "out"]
    /\ UNCHANGED <<adm, clock, Sealed, made, conn, bring, here, tries,
                   seatKey, held, seal, asking, stale, ask, rekeyKey, recon,
                   joins, fails, wasIn, session, keySess, goneSess, first>>

\* meet.leave never arrives: "the console ends the seat on its own".
LeaveUnreachable(p) ==
    /\ page[p] = "leaving" /\ Fail
    /\ page' = [page EXCEPT ![p] = "out"]
    /\ UNCHANGED <<seat, adm, clock, cur, prev, left, Sealed, made, conn,
                   bring, here, tries, seatKey, held, seal, asking, stale,
                   ask, rekeyKey, recon, joins, wasIn, session, keySess,
                   goneSess, first>>

-----------------------------------------------------------------------------
(* awaitNewKey: the page asks for keys after someone came or went.         *)

\* useKeys: every key it can open that it hasn't; a new current one seals
\* from now on. Then the loop stops if the key is new or none is due.
UseKeys(p, c, pv, due) ==
    LET opened == {k \in {c, pv} : k # 0 /\ p \in Sealed[k]}
        newer == c # 0 /\ c \in opened /\ c \notin held[p]
        in == page[p] = "in"
    IN /\ held' = [held EXCEPT ![p] = IF in THEN @ \cup opened ELSE @]
       /\ seal' = [seal EXCEPT ![p] = IF in /\ newer THEN c ELSE @]
       \* An older loop's answer: the newer loop asks on.
       /\ asking' = [asking EXCEPT ![p] =
                        IF ~in \/ stale[p] THEN @
                        ELSE IF opened # {} /\ (newer \/ ~due) THEN 0
                        ELSE IF Fixed THEN @ ELSE @ - 1]
       /\ stale' = [stale EXCEPT ![p] = FALSE]
       /\ ask' = [ask EXCEPT ![p] = "idle"]

\* Fixed: the keys route asks the relay who it has.
AskSnap(p) ==
    /\ Fixed
    /\ page[p] = "in" /\ asking[p] > 0 /\ ask[p] = "idle" /\ ~recon[p].on
    /\ recon' = [recon EXCEPT ![p] = [mark |-> clock, live |-> conn, on |-> TRUE]]
    /\ UNCHANGED <<seat, adm, clock, cur, prev, left, Sealed, made, conn,
                   page, bring, here, tries, seatKey, held, seal, asking,
                   stale, ask, rekeyKey, joins, fails, wasIn, session,
                   keySess, goneSess, first>>

\* current_keys: the answer; when a key is due and this computer is
\* present, it makes one sealed to who is present and itself.
AskAnswer(p) ==
    /\ ask[p] = "idle"
    /\ IF Fixed THEN recon[p].on ELSE page[p] = "in" /\ asking[p] > 0
    /\ LET s == AfterSeat(recon[p])
           l == AfterLeft(recon[p])
           present == {q \in People : s[q] # "none"}
           ended == \A q \in People : s[q] = "none"
           c == IF ended THEN 0 ELSE cur
           pv == IF ended THEN 0 ELSE prev
           holders == IF c = 0 THEN {} ELSE Sealed[c]
           due == c # 0 /\ ((l \cap holders) # {} \/ (present \ holders) # {})
       IN /\ EndIfEmpty(s, l)
          /\ IF due /\ p \in present
             THEN /\ NewKey(present \cup {p})
                  /\ rekeyKey' = [rekeyKey EXCEPT ![p] = made + 1]
                  /\ ask' = [ask EXCEPT ![p] = "rekey"]
                  /\ UNCHANGED <<held, seal, asking, stale>>
             ELSE /\ UseKeys(p, c, pv, due)
                  /\ UNCHANGED <<made, Sealed, rekeyKey>>
    /\ recon' = [recon EXCEPT ![p] = None]
    /\ UNCHANGED <<adm, clock, conn, page, bring, here, tries, seatKey, joins,
                   fails, wasIn, session, keySess, goneSess, first>>

\* Rooms::rekey: taken from someone present, while one is due, sealed to
\* exactly who is present; otherwise refused, and glassd asks again.
AskRekey(p) ==
    /\ ask[p] = "rekey"
    /\ LET k == rekeyKey[p]
           ok == p \in Present /\ Due /\ Sealed[k] = Present
       IN IF ok
          THEN /\ cur' = k /\ prev' = cur /\ left' = {}
               /\ keySess' = [keySess EXCEPT ![k] = session]
               /\ UseKeys(p, k, cur, FALSE)
          ELSE /\ ask' = [ask EXCEPT ![p] = "again"]
               /\ UNCHANGED <<cur, prev, left, held, seal, asking, stale, keySess>>
    /\ UNCHANGED <<seat, adm, clock, Sealed, made, conn, page, bring, here,
                   tries, seatKey, rekeyKey, recon, joins, fails, wasIn,
                   session, goneSess, first>>

\* The ask after a refused rekey: the answer as it is (the keys route,
\* its reconcile taken as one step here).
AskAgain(p) ==
    /\ ask[p] = "again"
    /\ LET rc == IF Fixed THEN [mark |-> clock, live |-> conn, on |-> TRUE] ELSE None
           s == AfterSeat(rc)
           l == AfterLeft(rc)
           present == {q \in People : s[q] # "none"}
           ended == \A q \in People : s[q] = "none"
           c == IF ended THEN 0 ELSE cur
           pv == IF ended THEN 0 ELSE prev
           holders == IF c = 0 THEN {} ELSE Sealed[c]
           due == c # 0 /\ ((l \cap holders) # {} \/ (present \ holders) # {})
       IN /\ EndIfEmpty(s, l)
          /\ UseKeys(p, c, pv, due)
    /\ UNCHANGED <<adm, clock, Sealed, made, conn, page, bring, here, tries,
                   seatKey, rekeyKey, recon, joins, fails, wasIn, session,
                   keySess, goneSess, first>>

\* meet.keys fails: out of reach. The loop counts it as an ask.
AskUnreachable(p) ==
    /\ ask[p] = "idle" /\ Fail
    /\ IF Fixed THEN recon[p].on ELSE page[p] = "in" /\ asking[p] > 0
    /\ asking' = [asking EXCEPT ![p] = IF page[p] # "in" \/ stale[p] \/ Fixed
                                        THEN @ ELSE @ - 1]
    /\ stale' = [stale EXCEPT ![p] = FALSE]
    /\ recon' = [recon EXCEPT ![p] = None]
    /\ UNCHANGED <<seat, adm, clock, cur, prev, left, Sealed, made, conn,
                   page, bring, here, tries, seatKey, held, seal, ask,
                   rekeyKey, joins, wasIn, session, keySess, goneSess, first>>

-----------------------------------------------------------------------------
(* Fixed: the console's sweep of meetings' sessions, every 30 s.           *)

Sweep ==
    /\ Fixed /\ (\E p \in People : seat[p] # "none")
    /\ LET rc == [mark |-> clock, live |-> conn, on |-> TRUE]
       IN EndIfEmpty(AfterSeat(rc), AfterLeft(rc))
    /\ UNCHANGED <<adm, clock, Sealed, made, conn, page, bring, here, tries,
                   seatKey, held, seal, asking, stale, ask, rekeyKey, recon,
                   joins, fails, wasIn, session, keySess, goneSess, first>>

-----------------------------------------------------------------------------
Next ==
    \/ \E p \in People :
        \/ Join(p) \/ JoinKeys(p) \/ JoinUnreachable(p)
        \/ JoinSeat(p) \/ JoinRelay(p)
        \/ Drop(p) \/ LeaveClose(p) \/ LeaveTell(p) \/ LeaveUnreachable(p)
        \/ AskSnap(p) \/ AskAnswer(p) \/ AskRekey(p) \/ AskAgain(p)
        \/ AskUnreachable(p)
    \/ Sweep

Spec == Init /\ [][Next]_vars

\* People are alike: TLC checks one of each arrangement.
Alike == Permutations(People)

-----------------------------------------------------------------------------
(* What the OS promises.                                                   *)

TypeOK ==
    /\ seat \in [People -> {"none", "seated", "admitted"}]
    /\ cur \in 0..MaxKeys /\ prev \in 0..MaxKeys /\ left \subseteq People
    /\ conn \subseteq People
    /\ page \in [People -> {"out", "keys", "seat", "relay", "in", "leaving"}]
    /\ \A p \in People : held[p] \subseteq Keys

\* Nothing is under way: every page is in the call or out of it, none is
\* asking for keys, the console has nothing half done, and its sweep
\* would change nothing.
Settled ==
    /\ \A p \in People : page[p] \in {"out", "in"} /\ asking[p] = 0
                         /\ ask[p] = "idle" /\ ~recon[p].on
    /\ Fixed => Gone([mark |-> clock, live |-> conn, on |-> TRUE]) = {}

\* Someone who was in the call and isn't now: they left or dropped out.
GoneFromCall == {p \in People : wasIn[p] /\ page[p] # "in"}

\* CF-21 (ADR 0030 §6): once things settle, nobody who left the call can
\* open what anyone in it now sends.
CF21_WhoLeftCantOpenWhatFollows ==
    Settled => \A g \in GoneFromCall, q \in InCall :
        \/ g \notin Sealed[seal[q]]
        \* A session's first key is sealed to the meeting's people (ADR
        \* 0030): the first key of a session that began after they left.
        \/ first[seal[q]] /\ keySess[seal[q]] > goneSess[g]

\* CF-21: once things settle, everyone in the call opens what everyone
\* else in it sends.
CF21_EveryoneHearsEveryone ==
    Settled => \A p, q \in InCall : seal[q] \in held[p]

\* CF-21: someone in the call has their seat in the console (the sweep or
\* a reconcile never takes the seat of someone the relay has, or is
\* taking), so new keys are sealed to them.
CF21_InTheCallMeansSeated ==
    \A p \in People : page[p] = "in" /\ p \in conn => seat[p] # "none"

\* CF-21 (ADR 0030: the console deletes a session's keys when it ends):
\* once settled with nobody in the call, the console keeps no key.
CF21_NoKeyOutlivesTheCall ==
    (Settled /\ \A p \in People : page[p] = "out") => cur = 0
=============================================================================
