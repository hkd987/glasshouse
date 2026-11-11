---------------------------- MODULE ComputerLease ----------------------------
(***************************************************************************)
(* One agent computer and the hosts of a stack that share nothing but the  *)
(* store (ADR 0066): which host may wake it, save it and write its record, *)
(* while hosts are cut off from the store, their glassd stops and starts,  *)
(* and hosts go away for good with the computer awake.                     *)
(*                                                                         *)
(* AgentComputers.tla is the computer on one host (its tasks, its folder,  *)
(* a sleep and a wake step by step). This is the same sleep and wake seen  *)
(* from the store, by more than one host.                                  *)
(*                                                                         *)
(* The code:                                                               *)
(*  - crates/svc/computers/src/store/lease.rs: `claim` (WakeAsk: a host    *)
(*    that doesn't have the computer reads the newest lease and may take   *)
(*    the next term if nobody holds it), `reclaim` (ReclaimAsk: a host     *)
(*    that has it awake may take the next term only if the newest is its   *)
(*    own), `take` (WakeTake, ReclaimTake: the next term's object is made  *)
(*    only if it isn't there, `put_new`, S3's If-None-Match), `renew`      *)
(*    (Renew), `release` (SleepRelease).                                   *)
(*  - crates/svc/computers/src/sandbox.rs: `wake` and `wake_taken`         *)
(*    (WakeAsk, WakeTake, WakeRestore from the version the store's record  *)
(*    names, WakeDone once the lease is surely still this host's), `sleep` *)
(*    (SleepBegin, SleepSave, SleepRecord, SleepClear, SleepRelease),      *)
(*    `sure` and `still_ours` (SureAsk: a host that hasn't heard from the  *)
(*    store for TRUST_FOR asks again before it writes), `lose` (Gone),     *)
(*    `keep_leases` (Renew, KeepAsk, SetAside), `adopt` (Adopt),           *)
(*    `cleanup_stale` (Crash: what a wake left is cleared at the start).   *)
(*                                                                         *)
(* Time is two facts. `trusted[h]`: the store took h's claim or renewal    *)
(* less than TRUST_FOR ago. `fresh`: the newest lease was renewed less     *)
(* than LEASE_FOR ago. TRUST_FOR is well short of LEASE_FOR, so a lease    *)
(* runs out (Expire) only once its holder no longer trusts it; and a write *)
(* of the record follows its check by less than the difference, so         *)
(* SleepRecord is one step with the check.                                 *)
(*                                                                         *)
(* A change to a sleeping computer's record (an agent added) is, to the    *)
(* lease, a wake and a sleep with nothing between: `change_record` takes   *)
(* the lease, writes the record and releases it.                           *)
(*                                                                         *)
(* Old is the code before this model, and ways it could be wrong:          *)
(*  "noLease"        nothing said which host had the computer: a second    *)
(*                   host woke it from its last sleep while the first      *)
(*                   still had it, and the later save replaced the         *)
(*                   earlier one's work.                                   *)
(*  "localRecord"    a wake restored the version the host's own record     *)
(*                   named, from when it last had the computer (audit      *)
(*                   7.3), not the one the store's record names.           *)
(*  "reclaimIfFree"  a host with the computer awake, asking again, was     *)
(*                   given the lease whenever nobody held it: also after   *)
(*                   another host had taken it, worked and released it.    *)
(*  "noMargin"       TRUST_FOR as long as LEASE_FOR: a lease runs out      *)
(*                   while its holder still acts on it.                    *)
(* Each fails NothingSavedOverAnotherHostsWork.                            *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    Hosts,          \* the stack's hosts
    MaxSaves,       \* versions saved
    MaxTerms,       \* terms taken (a bound on the model, not in the code)
    MaxCrashes,     \* glassd stopping and starting, and hosts going away
    Old             \* see above

VARIABLES
    \* The store.
    lease,          \* the newest term: its number, its host, given up or not
    fresh,          \* it was renewed within the last LEASE_FOR
    recVer,         \* the version the computer's record names; 0: never slept
    nextVer,        \* versions saved so far
    \* Each host's disk.
    loc,            \* "none" (not here, or its record only), "waking",
                    \* "here", "gone" (the host is no more)
    base,           \* the version its copy came from, or was last saved as
    remVer,         \* the version its own record names; 0: it knows none
    \* Each host's glassd, in memory.
    pc,             \* where it is in a wake or a sleep
    after,          \* where it goes on once it has asked again
    asking,         \* the term it is about to make
    saved,          \* the version its save just wrote
    myTerm,         \* the term it holds, 0 for none
    trusted,        \* the store took word of it within TRUST_FOR
    lost,           \* it knows another host has the computer it has awake
    \* History.
    forked,         \* a record was written from a copy that didn't come
                    \* from the version it named
    crashes

vars == <<lease, fresh, recVer, nextVer, loc, base, remVer, pc, after, asking,
          saved, myTerm, trusted, lost, forked, crashes>>

Pcs == {"idle", "wakeTake", "restore", "wakeSure", "sleepSure", "recordSure",
        "clear", "release", "reclaim", "reclaimTake"}

TypeOK ==
    /\ lease \in [term : 1..(MaxTerms + 1), host : Hosts, released : BOOLEAN]
    /\ fresh \in BOOLEAN
    /\ recVer \in 0..MaxSaves /\ nextVer \in 0..MaxSaves
    /\ loc \in [Hosts -> {"none", "waking", "here", "gone"}]
    /\ base \in [Hosts -> 0..MaxSaves]
    /\ remVer \in [Hosts -> 0..MaxSaves]
    /\ pc \in [Hosts -> Pcs]
    /\ after \in [Hosts -> Pcs]
    /\ asking \in [Hosts -> 0..(MaxTerms + 1)]
    /\ saved \in [Hosts -> 0..MaxSaves]
    /\ myTerm \in [Hosts -> 0..(MaxTerms + 1)]
    /\ trusted \in [Hosts -> BOOLEAN]
    /\ lost \in [Hosts -> BOOLEAN]
    /\ forked \in BOOLEAN
    /\ crashes \in 0..MaxCrashes

(* `make`: the host that makes it takes its first term and puts its record *)
(* in the store. It never slept: there is nothing of it to wake.           *)
Init ==
    \E m \in Hosts :
        /\ lease = [term |-> 1, host |-> m, released |-> FALSE]
        /\ fresh = TRUE
        /\ recVer = 0 /\ nextVer = 0
        /\ loc = [h \in Hosts |-> IF h = m THEN "here" ELSE "none"]
        /\ base = [h \in Hosts |-> 0]
        /\ remVer = [h \in Hosts |-> 0]
        /\ pc = [h \in Hosts |-> "idle"]
        /\ after = [h \in Hosts |-> "idle"]
        /\ asking = [h \in Hosts |-> 0]
        /\ saved = [h \in Hosts |-> 0]
        /\ myTerm = [h \in Hosts |-> IF h = m THEN 1 ELSE 0]
        /\ trusted = [h \in Hosts |-> h = m]
        /\ lost = [h \in Hosts |-> FALSE]
        /\ forked = FALSE /\ crashes = 0

NoLease == "noLease" \in Old

(* A host is sure the computer is its own: it holds a term, and the store  *)
(* took word of that lately.                                               *)
Sure(h) == NoLease \/ (trusted[h] /\ myTerm[h] > 0)

(* `claim`: for a host that doesn't have it awake. Its own lease, left by  *)
(* a wake its glassd didn't live through, is no other host's.              *)
Takeable(h) ==
    NoLease \/ lease.host = h \/ lease.released \/ ~fresh

(* `reclaim`: for a host that has it awake. Anything but its own lease,    *)
(* still held, means another host has had the computer since.              *)
StillOwn(h) ==
    \/ NoLease
    \/ (lease.host = h /\ ~lease.released)
    \/ ("reclaimIfFree" \in Old /\ (lease.released \/ ~fresh))

(* `take`: the next term's object is made only if it isn't there.          *)
TakeWorks(h) == NoLease \/ lease.term + 1 = asking[h]

Taken(h) ==
    /\ lease' = [term |-> lease.term + 1, host |-> h, released |-> FALSE]
    /\ fresh' = TRUE
    /\ trusted' = [trusted EXCEPT ![h] = TRUE]
    /\ myTerm' = [myTerm EXCEPT ![h] = lease.term + 1]

(* `adopt`: a host that never had the computer lists it from its record in *)
(* the store, asleep, and keeps that as its own record of it.              *)
Adopt(h) ==
    /\ loc[h] = "none" /\ pc[h] = "idle" /\ remVer[h] = 0 /\ recVer > 0
    /\ remVer' = [remVer EXCEPT ![h] = recVer]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, loc, base, pc, after,
                   asking, saved, myTerm, trusted, lost, forked, crashes>>

(* Wake: a task or its person, on a host that knows it asleep.             *)
WakeAsk(h) ==
    /\ pc[h] = "idle" /\ loc[h] = "none" /\ remVer[h] > 0
    /\ Takeable(h)
    /\ asking' = [asking EXCEPT ![h] = lease.term + 1]
    /\ pc' = [pc EXCEPT ![h] = "wakeTake"]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, loc, base, remVer, after,
                   saved, myTerm, trusted, lost, forked, crashes>>

WakeTake(h) ==
    /\ pc[h] = "wakeTake"
    /\ asking' = [asking EXCEPT ![h] = 0]
    /\ IF TakeWorks(h)
          THEN /\ Taken(h)
               /\ pc' = [pc EXCEPT ![h] = "restore"]
               /\ loc' = [loc EXCEPT ![h] = "waking"]
          \* Another host made that term first: refused, "awake on
          \* another host".
          ELSE /\ pc' = [pc EXCEPT ![h] = "idle"]
               /\ UNCHANGED <<lease, fresh, trusted, myTerm, loc>>
    /\ UNCHANGED <<recVer, nextVer, base, remVer, after, saved, lost, forked,
                   crashes>>

(* All of it back in its folder, from the version the store's record names *)
(* now that this host holds the lease.                                     *)
WakeRestore(h) ==
    /\ pc[h] = "restore"
    /\ base' = [base EXCEPT ![h] =
                    IF "localRecord" \in Old THEN remVer[h] ELSE recVer]
    /\ pc' = [pc EXCEPT ![h] = "wakeSure"]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, loc, remVer, after, asking,
                   saved, myTerm, trusted, lost, forked, crashes>>

(* Awake here, once the lease is surely still this host's: a wake can take *)
(* longer than a lease lasts.                                              *)
WakeDone(h) ==
    /\ pc[h] = "wakeSure" /\ Sure(h)
    /\ loc' = [loc EXCEPT ![h] = "here"]
    /\ pc' = [pc EXCEPT ![h] = "idle"]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, base, remVer, after, asking,
                   saved, myTerm, trusted, lost, forked, crashes>>

(* Sleep: nothing using it.                                                *)
SleepBegin(h) ==
    /\ pc[h] = "idle" /\ loc[h] = "here" /\ ~lost[h]
    /\ nextVer < MaxSaves
    /\ pc' = [pc EXCEPT ![h] = "sleepSure"]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, loc, base, remVer, after,
                   asking, saved, myTerm, trusted, lost, forked, crashes>>

(* All of its folder to the store, as a new version no record names yet.   *)
SleepSave(h) ==
    /\ pc[h] = "sleepSure" /\ Sure(h)
    /\ nextVer' = nextVer + 1
    /\ saved' = [saved EXCEPT ![h] = nextVer + 1]
    /\ pc' = [pc EXCEPT ![h] = "recordSure"]
    /\ UNCHANGED <<lease, fresh, recVer, loc, base, remVer, after, asking,
                   myTerm, trusted, lost, forked, crashes>>

(* The record names the new version: what any host wakes from now on.      *)
SleepRecord(h) ==
    /\ pc[h] = "recordSure" /\ Sure(h)
    /\ forked' = (forked \/ base[h] # recVer)
    /\ recVer' = saved[h]
    /\ base' = [base EXCEPT ![h] = saved[h]]
    /\ pc' = [pc EXCEPT ![h] = "clear"]
    /\ UNCHANGED <<lease, fresh, nextVer, loc, remVer, after, asking, saved,
                   myTerm, trusted, lost, crashes>>

(* Its own record says asleep, and the folder goes.                        *)
SleepClear(h) ==
    /\ pc[h] = "clear"
    /\ loc' = [loc EXCEPT ![h] = "none"]
    /\ remVer' = [remVer EXCEPT ![h] = saved[h]]
    /\ pc' = [pc EXCEPT ![h] = "release"]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, base, after, asking, saved,
                   myTerm, trusted, lost, forked, crashes>>

(* Its term is marked given up. A host writes only its own term's object:  *)
(* if a newer term is there, this changes nothing that counts.             *)
SleepRelease(h) ==
    /\ pc[h] = "release"
    /\ lease' = IF lease.host = h /\ lease.term = myTerm[h]
                   THEN [lease EXCEPT !.released = TRUE]
                   ELSE lease
    /\ myTerm' = [myTerm EXCEPT ![h] = 0]
    /\ trusted' = [trusted EXCEPT ![h] = FALSE]
    /\ pc' = [pc EXCEPT ![h] = "idle"]
    /\ UNCHANGED <<fresh, recVer, nextVer, loc, base, remVer, after, asking,
                   saved, lost, forked, crashes>>

(* A host that isn't sure asks again before it goes on: in a sleep, at the *)
(* end of a wake, and every half minute for a computer awake on it.        *)
SureAsk(h) ==
    /\ pc[h] \in {"wakeSure", "sleepSure", "recordSure"} /\ ~Sure(h)
    /\ after' = [after EXCEPT ![h] = pc[h]]
    /\ pc' = [pc EXCEPT ![h] = "reclaim"]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, loc, base, remVer, asking,
                   saved, myTerm, trusted, lost, forked, crashes>>

KeepAsk(h) ==
    /\ pc[h] = "idle" /\ loc[h] = "here" /\ ~lost[h] /\ ~Sure(h)
    /\ after' = [after EXCEPT ![h] = "idle"]
    /\ pc' = [pc EXCEPT ![h] = "reclaim"]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, loc, base, remVer, asking,
                   saved, myTerm, trusted, lost, forked, crashes>>

(* `lose`, or a wake that fails: what this host has is not the computer.   *)
(* A copy awake here is marked lost: nothing of it is saved, no task       *)
(* starts in it. What a wake brought is cleared.                           *)
Gone(h) ==
    /\ myTerm' = [myTerm EXCEPT ![h] = 0]
    /\ asking' = [asking EXCEPT ![h] = 0]
    /\ pc' = [pc EXCEPT ![h] = "idle"]
    /\ IF loc[h] = "here"
          THEN lost' = [lost EXCEPT ![h] = TRUE] /\ UNCHANGED loc
          ELSE loc' = [loc EXCEPT ![h] = "none"] /\ UNCHANGED lost

ReclaimAsk(h) ==
    /\ pc[h] = "reclaim"
    /\ IF StillOwn(h)
          THEN /\ asking' = [asking EXCEPT ![h] = lease.term + 1]
               /\ pc' = [pc EXCEPT ![h] = "reclaimTake"]
               /\ UNCHANGED <<loc, lost, myTerm>>
          ELSE Gone(h)
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, base, remVer, after, saved,
                   trusted, forked, crashes>>

ReclaimTake(h) ==
    /\ pc[h] = "reclaimTake"
    /\ IF TakeWorks(h)
          THEN /\ Taken(h)
               /\ asking' = [asking EXCEPT ![h] = 0]
               /\ pc' = [pc EXCEPT ![h] = after[h]]
               /\ UNCHANGED <<loc, lost>>
          \* Another host took the term in between: it has the computer.
          ELSE Gone(h) /\ UNCHANGED <<lease, fresh, trusted>>
    /\ UNCHANGED <<recVer, nextVer, base, remVer, after, saved, forked,
                   crashes>>

(* `keep_leases`: a holder the store heard from lately says so again, for  *)
(* a computer awake on it or on its way.                                   *)
Renew(h) ==
    /\ trusted[h] /\ myTerm[h] > 0
    /\ lease.host = h /\ lease.term = myTerm[h] /\ ~lease.released
    /\ ~fresh
    /\ fresh' = TRUE
    /\ UNCHANGED <<lease, recVer, nextVer, loc, base, remVer, pc, after,
                   asking, saved, myTerm, trusted, lost, forked, crashes>>

(* A lost copy is set aside once nothing runs in it, and the computer is   *)
(* listed here as the store has it.                                        *)
SetAside(h) ==
    /\ pc[h] = "idle" /\ loc[h] = "here" /\ lost[h]
    /\ loc' = [loc EXCEPT ![h] = "none"]
    /\ lost' = [lost EXCEPT ![h] = FALSE]
    /\ remVer' = [remVer EXCEPT ![h] = recVer]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, base, pc, after, asking,
                   saved, myTerm, trusted, forked, crashes>>

(* Time passing. A host hasn't heard from the store for TRUST_FOR (it was  *)
(* cut off, or its renewals failed).                                       *)
Distrust(h) ==
    /\ trusted[h]
    /\ trusted' = [trusted EXCEPT ![h] = FALSE]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, loc, base, remVer, pc,
                   after, asking, saved, myTerm, lost, forked, crashes>>

(* The newest lease hasn't been renewed for LEASE_FOR: longer than its     *)
(* holder trusts it for.                                                   *)
Expire ==
    /\ fresh /\ ~lease.released
    /\ ("noMargin" \in Old \/ ~trusted[lease.host])
    /\ fresh' = FALSE
    /\ UNCHANGED <<lease, recVer, nextVer, loc, base, remVer, pc, after,
                   asking, saved, myTerm, trusted, lost, forked, crashes>>

(* glassd stops and starts again on a host: all it had in memory is gone.  *)
(* What a wake left in the folder is cleared at the start; a computer      *)
(* awake here is still here.                                               *)
Crash(h) ==
    /\ crashes < MaxCrashes /\ loc[h] # "gone"
    /\ crashes' = crashes + 1
    /\ pc' = [pc EXCEPT ![h] = "idle"]
    /\ after' = [after EXCEPT ![h] = "idle"]
    /\ asking' = [asking EXCEPT ![h] = 0]
    /\ myTerm' = [myTerm EXCEPT ![h] = 0]
    /\ trusted' = [trusted EXCEPT ![h] = FALSE]
    /\ lost' = [lost EXCEPT ![h] = FALSE]
    /\ loc' = [loc EXCEPT ![h] = IF @ = "waking" THEN "none" ELSE @]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, base, remVer, saved, forked>>

(* A host goes away for good, with whatever it had. One host stays.        *)
Die(h) ==
    /\ crashes < MaxCrashes /\ loc[h] # "gone"
    /\ \E g \in Hosts \ {h} : loc[g] # "gone"
    /\ crashes' = crashes + 1
    /\ loc' = [loc EXCEPT ![h] = "gone"]
    /\ pc' = [pc EXCEPT ![h] = "idle"]
    /\ after' = [after EXCEPT ![h] = "idle"]
    /\ asking' = [asking EXCEPT ![h] = 0]
    /\ myTerm' = [myTerm EXCEPT ![h] = 0]
    /\ trusted' = [trusted EXCEPT ![h] = FALSE]
    /\ lost' = [lost EXCEPT ![h] = FALSE]
    /\ UNCHANGED <<lease, fresh, recVer, nextVer, base, remVer, saved, forked>>

Next ==
    \/ \E h \in Hosts :
        \/ Adopt(h) \/ WakeAsk(h) \/ WakeTake(h) \/ WakeRestore(h) \/ WakeDone(h)
        \/ SleepBegin(h) \/ SleepSave(h) \/ SleepRecord(h) \/ SleepClear(h)
        \/ SleepRelease(h)
        \/ SureAsk(h) \/ KeepAsk(h) \/ ReclaimAsk(h) \/ ReclaimTake(h)
        \/ Renew(h) \/ SetAside(h) \/ Distrust(h) \/ Crash(h) \/ Die(h)
    \/ Expire

Spec == Init /\ [][Next]_vars

(* A bound on the model: terms run out.                                    *)
Bounded == lease.term <= MaxTerms

(* What the OS promises.                                                   *)

\* Only the computer's line of versions is ever in its record: a host
\* writes the record only from a copy that came from the version the
\* record named. No save replaces work another host did since.
NothingSavedOverAnotherHostsWork == ~forked

\* A host that is sure of its lease has it: the newest term is its own,
\* held, and not run out.
SureMeansHeld ==
    \A h \in Hosts :
        (trusted[h] /\ myTerm[h] > 0) =>
            (lease.host = h /\ lease.term = myTerm[h] /\ ~lease.released /\ fresh)

\* So no two hosts are sure of the one computer at once.
OneSureHostAtATime ==
    \A g, h \in Hosts :
        (g # h) => ~(trusted[g] /\ myTerm[g] > 0 /\ trusted[h] /\ myTerm[h] > 0)

\* Audit 7.3: the copy a host is sure of is the version the record names,
\* whatever that host's own record remembered.
AwakeIsWhatTheRecordNames ==
    \A h \in Hosts :
        (loc[h] = "here" /\ ~lost[h] /\ trusted[h] /\ myTerm[h] > 0) =>
            base[h] = recVer

=============================================================================
