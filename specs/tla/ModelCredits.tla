---------------------------- MODULE ModelCredits ----------------------------
(***************************************************************************)
(* An org's model credits (ADR 0039, ADR 0040): payments relayed from      *)
(* Stripe raise the org's OpenRouter key limit once each, and computers'   *)
(* calls on credits spend it through the admin console.                    *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/bin/glass-console/src/server.rs   billing_event, add_credits   *)
(*                (one grant at a time, on a copy kept only once saved),   *)
(*                complete_model (the key read, a permit of 64, the call,  *)
(*                then usage recorded)                                     *)
(*   crates/bin/glass-console/src/credits.rs  Credits::grant (a session    *)
(*                granted once; the key's limit set to every grant's sum,  *)
(*                then saved), Usage::record                               *)
(*   crates/bin/glass-billing (ADR 0040)      the relay: a payment pushed  *)
(*                until the console answers 200                            *)
(*   crates/svc/gateway-providers/src/credits.rs, console.rs  the          *)
(*                computer's side: 402 and 5xx fail over (MP-8)            *)
(*   crates/svc/gateway/src/gateway.rs        failover to the next model   *)
(*                                                                         *)
(* OpenRouter is modelled as its documentation says: a call starts while   *)
(* the key's usage is below its limit, and is charged what it used when it *)
(* ends, even when the caller stopped waiting. Each call costs one unit; a *)
(* payment buys two; our operators can grant one.                          *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    Sessions,     \* Stripe checkouts paid
    Calls,        \* computers' calls on credits
    MaxInFlight,  \* the console's permits (64 in the code)
    MaxLost,      \* answers lost on the way: the relay's 200s and calls' replies
    MaxSaveFails  \* the console's credits.json failing to save

Paid == 2      \* units a payment buys
OperatorGrant == 1

VARIABLES
    grants,     \* credits.json: the grants, in order (sessions, or "op")
    orLimit,    \* OpenRouter: the key's limit
    orUsage,    \* OpenRouter: what the key has spent
    relay,      \* each payment: "queued", "delivered"
    gp,         \* the grant under way (add_credits holds `granting`)
    call,       \* each call: where it is
    recorded,   \* the console's model-usage.json, in units
    elsewhere,  \* calls the next model on the route answered after credits failed
    billed,     \* each call: times it was billed, anywhere
    lost, saveFails,
    opAsked,    \* times our operator added credits
    opFailed    \* and was told it failed

vars == <<grants, orLimit, orUsage, relay, gp, call, recorded, elsewhere,
          billed, lost, saveFails, opAsked, opFailed>>

NoGrant == [at |-> "idle", g |-> "none", from |-> "none"]

Amount(g) == IF g = "op" THEN OperatorGrant ELSE Paid
RECURSIVE SumOf(_)
SumOf(s) == IF s = <<>> THEN 0 ELSE Amount(Head(s)) + SumOf(Tail(s))
Range(s) == {s[i] : i \in 1..Len(s)}
InFlight == Cardinality({c \in Calls : call[c] \in {"permit", "openrouter"}})
\* Everything bought or given: payments the relay sent, and ours.
Bought == Paid * Cardinality(Sessions) + (IF opAsked > 0 THEN OperatorGrant ELSE 0)

Init ==
    /\ grants = <<>> /\ orLimit = 0 /\ orUsage = 0
    /\ relay = [s \in Sessions |-> "queued"]
    /\ gp = NoGrant
    /\ call = [c \in Calls |-> "computer"]
    /\ recorded = 0 /\ elsewhere = 0
    /\ billed = [c \in Calls |-> 0]
    /\ lost = 0 /\ saveFails = 0 /\ opAsked = 0 /\ opFailed = FALSE

-----------------------------------------------------------------------------
(* Grants: the relay pushes a payment until the console answers 200; our   *)
(* operators add one. add_credits takes `granting`, copies the credits,    *)
(* and Credits::grant answers at once for a session already granted, or   *)
(* sets the key's limit to every grant's sum and saves; only a saved copy *)
(* replaces the console's.                                                 *)

\* billing_event or /ops/credits, under `granting`: a session already in
\* credits.json answers at once; otherwise the key's limit is set to every
\* grant's sum with this one.
GrantStart(g, from) ==
    /\ gp.at = "idle"
    /\ IF g # "op" /\ g \in Range(grants)
       THEN \* Already granted: 200 again, which may be lost too.
            /\ \/ relay' = [relay EXCEPT ![g] = "delivered"] /\ lost' = lost
               \/ lost < MaxLost /\ lost' = lost + 1 /\ relay' = relay
            /\ UNCHANGED <<gp, orLimit>>
       ELSE /\ orLimit' = SumOf(grants) + Amount(g)
            /\ gp' = [at |-> "limit_set", g |-> g, from |-> from]
            /\ UNCHANGED <<relay, lost>>
    /\ UNCHANGED <<grants, orUsage, call, recorded, elsewhere, billed, saveFails, opFailed>>

RelayPush(s) ==
    /\ relay[s] = "queued"
    /\ GrantStart(s, "relay")
    /\ UNCHANGED opAsked

\* Our operator adds credits, and again after an error says it failed.
OperatorAdds ==
    /\ opAsked < 2 /\ "op" \notin Range(grants)
    /\ opAsked = 1 => opFailed
    /\ GrantStart("op", "operator")
    /\ opAsked' = opAsked + 1

\* Credits::save, then the answer: 200 (which the relay may not hear).
GrantSaved ==
    /\ gp.at = "limit_set"
    /\ grants' = Append(grants, gp.g)
    /\ IF gp.from = "relay"
       THEN \/ relay' = [relay EXCEPT ![gp.g] = "delivered"] /\ UNCHANGED lost
            \/ lost < MaxLost /\ lost' = lost + 1 /\ UNCHANGED relay
       ELSE UNCHANGED <<relay, lost>>
    /\ gp' = NoGrant
    /\ UNCHANGED <<orLimit, orUsage, call, recorded, elsewhere, billed, saveFails, opAsked, opFailed>>

\* The save fails: 500 (the relay pushes again) or the operator's error, and
\* the copy is dropped. The key's limit stays raised.
GrantSaveFails ==
    /\ gp.at = "limit_set" /\ saveFails < MaxSaveFails
    /\ saveFails' = saveFails + 1
    /\ opFailed' = (opFailed \/ gp.g = "op")
    /\ gp' = NoGrant
    /\ UNCHANGED <<grants, orLimit, orUsage, relay, call, recorded, elsewhere, billed, lost, opAsked>>

-----------------------------------------------------------------------------
(* A computer's call on credits: the console checks the key is there and   *)
(* takes a permit; OpenRouter starts it while the key has room, and        *)
(* charges it when it ends; the console records it on success. A refusal  *)
(* (402), an error or a lost answer fails over to the next model on the    *)
(* route, which bills its own provider.                                    *)

ConsoleTakes(c) ==
    /\ call[c] = "computer"
    /\ IF Len(grants) > 0 /\ InFlight < MaxInFlight
       THEN call' = [call EXCEPT ![c] = "permit"]
       \* No credits (402), or 64 calls at once (503): the next model.
       ELSE /\ call' = [call EXCEPT ![c] = "failed_over"]
    /\ UNCHANGED <<grants, orLimit, orUsage, relay, gp, recorded, elsewhere, billed, lost, saveFails, opAsked, opFailed>>

OpenRouterStarts(c) ==
    /\ call[c] = "permit"
    /\ IF orUsage < orLimit
       THEN call' = [call EXCEPT ![c] = "openrouter"]
       ELSE call' = [call EXCEPT ![c] = "failed_over"]   \* 402: used up
    /\ UNCHANGED <<grants, orLimit, orUsage, relay, gp, recorded, elsewhere, billed, lost, saveFails, opAsked, opFailed>>

\* It ends: an answer (cut off or not, charged and recorded), a provider
\* error (not charged), or charged with the console's or the computer's
\* wait over (timed out, the connection dropped): the computer fails over.
OpenRouterEnds(c, how) ==
    /\ call[c] = "openrouter"
    /\ how = "lost" => lost < MaxLost
    /\ CASE how = "answered" ->
              /\ orUsage' = orUsage + 1 /\ recorded' = recorded + 1
              /\ billed' = [billed EXCEPT ![c] = @ + 1]
              /\ call' = [call EXCEPT ![c] = "done"]
              /\ UNCHANGED lost
         [] how = "error" ->
              /\ call' = [call EXCEPT ![c] = "failed_over"]
              /\ UNCHANGED <<orUsage, recorded, billed, lost>>
         [] how = "lost" ->
              /\ orUsage' = orUsage + 1 /\ lost' = lost + 1
              /\ billed' = [billed EXCEPT ![c] = @ + 1]
              /\ call' = [call EXCEPT ![c] = "failed_over"]
              /\ UNCHANGED recorded
    /\ UNCHANGED <<grants, orLimit, relay, gp, elsewhere, saveFails, opAsked, opFailed>>

\* MP-8: the next model on the route takes the call, on its own bill.
NextModel(c) ==
    /\ call[c] = "failed_over"
    /\ call' = [call EXCEPT ![c] = "done"]
    /\ elsewhere' = elsewhere + 1
    /\ billed' = [billed EXCEPT ![c] = @ + 1]
    /\ UNCHANGED <<grants, orLimit, orUsage, relay, gp, recorded, lost, saveFails, opAsked, opFailed>>

Next ==
    \/ \E s \in Sessions : RelayPush(s)
    \/ OperatorAdds \/ GrantSaved \/ GrantSaveFails
    \/ \E c \in Calls :
         \/ ConsoleTakes(c) \/ OpenRouterStarts(c) \/ NextModel(c)
         \/ \E how \in {"answered", "error", "lost"} : OpenRouterEnds(c, how)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* What the design promises.                                               *)

TypeOK ==
    /\ \A s \in Sessions : relay[s] \in {"queued", "delivered"}
    /\ gp.at \in {"idle", "limit_set"}

\* ADR 0040: a payment is granted once, however often the relay pushes it
\* or its answer is lost; the key's limit is never more than was bought.
ADR0040_EachPaymentGrantedOnce ==
    \A i, j \in 1..Len(grants) : i # j /\ grants[i] # "op" => grants[i] # grants[j]
ADR0039_TheLimitIsNoMoreThanWasBought ==
    orLimit <= Bought

\* ADR 0040: what the relay counts as delivered is in credits.json.
ADR0040_DeliveredIsKept ==
    \A s \in Sessions : relay[s] = "delivered" => s \in Range(grants)

\* Once every payment is delivered and nothing is under way, the key's
\* limit is what credits.json granted: a save that failed after the limit
\* was set is made good by the retry, the relay's for a payment, the
\* operator's for ours (until then the limit counts it, which stays within
\* what was bought).
ADR0039_TheLimitIsTheGrants ==
    (/\ gp.at = "idle"
     /\ \A s \in Sessions : relay[s] = "delivered"
     /\ ~(opFailed /\ "op" \notin Range(grants)))
        => orLimit = SumOf(grants)

\* ADR 0039: no computer can spend past what was bought by more than the
\* calls already under way when the limit was reached: at most the
\* console's permits, each a call's worth.
ADR0039_SpendWithinCredits ==
    orUsage <= Bought + MaxInFlight

\* Not promised, and not in the .cfg: spend within the limit. Calls under
\* way when the limit is reached still finish, and are charged.
SpendWithinTheLimit == orUsage <= orLimit

\* The console's own figure never says more was spent than was.
ADR0039_TheConsoleNeverOverstatesSpend == recorded <= orUsage

\* MP-8, ADR 0039: a call is billed twice only when credits charged it and
\* the answer was lost on the way, so the next model took it too.
BilledTwiceOnlyAfterALostAnswer ==
    \A c \in Calls : billed[c] <= 1 \/ (billed[c] = 2 /\ lost > 0)
=============================================================================
