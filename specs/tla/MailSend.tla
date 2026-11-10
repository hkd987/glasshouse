------------------------------ MODULE MailSend ------------------------------
(***************************************************************************)
(* Sending mail, with the mail server in reach or not (ADR 0037 "Sending", *)
(* DP-30): a message that can't leave waits in Drafts, and is sent once,   *)
(* never twice.                                                            *)
(*                                                                         *)
(* The code it follows:                                                    *)
(*   crates/apps/mail/src/lib.rs     MailProvider::send (the draft read,   *)
(*                                   submit, then file_sent), keep_unsent, *)
(*                                   draft                                 *)
(*   crates/apps/mail/src/handle.rs  MailHandle::take_drafts               *)
(*   apps/mail/src/main.ts           send, saveDraft: the person's Send of *)
(*                                   a draft, and Send or Save draft while *)
(*                                   editing one                           *)
(*                                                                         *)
(* Senders are calls that run at once: tasks, routines and the person's    *)
(* Mail app. mail.send is irreversible, so it doesn't take the person's    *)
(* turn (execute.rs), and two calls interleave. Sending one message is     *)
(* three steps: reading its draft (whose files go with it), submitting it, *)
(* then filing it in Sent and putting the draft away.                      *)
(*                                                                         *)
(* Fixed = FALSE is the code before ADR 0051; TRUE is the code now.        *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    Msgs,          \* what tasks write to send, each asked for once
    Senders,       \* calls running at once
    MaxDrafts,     \* drafts made, all told
    MaxOutages,    \* times the server goes out of reach
    LostReplies,   \* submissions the server took whose reply never came
    Fixed          \* the code with ADR 0051's fixes

Idle == [at |-> "idle", m |-> 0, d |-> 0]

VARIABLES
    online, outages, lost,
    drafts,     \* Drafts on this computer: [id, m] records
    made,       \* draft ids handed out
    took,       \* took[m]: times the mail server took m to send
    asked,      \* messages a task asked to send (ghost)
    pc,         \* each call's step, its message and the draft it names
    claimed     \* Fixed: drafts a call is sending or replacing

vars == <<online, outages, lost, drafts, made, took, asked, pc, claimed>>

Draft(d) == {r \in drafts : r.id = d}
MsgOf(d) == (CHOOSE r \in Draft(d) : TRUE).m
New(m) == [id |-> made + 1, m |-> m]

Init ==
    /\ online = TRUE /\ outages = 0 /\ lost = 0
    /\ drafts = {} /\ made = 0
    /\ took = [m \in Msgs |-> 0] /\ asked = {}
    /\ pc = [p \in Senders |-> Idle] /\ claimed = {}

-----------------------------------------------------------------------------
GoOffline ==
    /\ online /\ outages < MaxOutages
    /\ online' = FALSE /\ outages' = outages + 1
    /\ UNCHANGED <<lost, drafts, made, took, asked, pc, claimed>>

GoOnline ==
    /\ ~online /\ online' = TRUE
    /\ UNCHANGED <<outages, lost, drafts, made, took, asked, pc, claimed>>

\* mail.send of a message a task wrote, naming no draft.
Ask(p, m) ==
    /\ pc[p] = Idle /\ m \notin asked
    /\ asked' = asked \cup {m}
    /\ pc' = [pc EXCEPT ![p] = [at |-> "submit", m |-> m, d |-> 0]]
    /\ UNCHANGED <<online, outages, lost, drafts, made, took, claimed>>

\* mail.send of a draft by its id: a task's, or the person's Send on it
\* (and, fixed, their Send while editing it). The id may be from a listing
\* a while ago. Fixed: the call takes the draft first, and is refused
\* while another call has it or it names it twice ("being sent").
SendDraft(p, d) ==
    /\ pc[p] = Idle /\ d \in 1..made
    /\ IF Fixed THEN d \notin claimed /\ claimed' = claimed \cup {d}
                ELSE UNCHANGED claimed
    /\ pc' = [pc EXCEPT ![p] = [at |-> "read", m |-> 0, d |-> d]]
    /\ UNCHANGED <<online, outages, lost, drafts, made, took, asked>>

\* Before the fix, a draft's message was sent again naming no draft: the
\* person's Send while editing it, and a task told "kept in Drafts, send it
\* once mail is connected" (mail.send's schema had no `draft`). It went,
\* and the draft stayed. Fixed, both name it (and a task that writes the
\* message afresh anyway sends a message of its own).
Resend(p, d) ==
    /\ ~Fixed /\ pc[p] = Idle /\ Draft(d) # {}
    /\ pc' = [pc EXCEPT ![p] = [at |-> "submit", m |-> MsgOf(d), d |-> 0]]
    /\ UNCHANGED <<online, outages, lost, drafts, made, took, asked, claimed>>

\* It reads the draft; one that's gone fails the call, nothing sent.
Read(p) ==
    /\ pc[p].at = "read"
    /\ LET d == pc[p].d
       IN IF Draft(d) # {}
          THEN /\ pc' = [pc EXCEPT ![p] = [at |-> "submit", m |-> MsgOf(d), d |-> d]]
               /\ UNCHANGED claimed
          ELSE /\ pc' = [pc EXCEPT ![p] = Idle]
               /\ claimed' = claimed \ {d}
    /\ UNCHANGED <<online, outages, lost, drafts, made, took, asked>>

\* The person's Save draft while editing one (mail.draft, one step under
\* their turn). Before the fix it saved a new draft beside the old; now it
\* names the draft and replaces it, refused while a call sends it.
EditSave(d) ==
    /\ Draft(d) # {} /\ made < MaxDrafts
    /\ ~Fixed \/ d \notin claimed
    /\ drafts' = IF Fixed THEN (drafts \ Draft(d)) \cup {New(MsgOf(d))}
                 ELSE drafts \cup {New(MsgOf(d))}
    /\ made' = made + 1
    /\ UNCHANGED <<online, outages, lost, took, asked, pc, claimed>>

\* SMTP: in reach, the server takes it. Out of reach, or (LostReplies) it
\* took it and the reply was lost on the way back, the call keeps it in
\* Drafts (keep_unsent): before the fix as a new draft, even when it was
\* one; now in the draft it names. (With the text this call tried to send:
\* a draft the person went on writing before Send holds what they wrote.
\* The message here is the draft's, so that is the same step.)
Submit(p) ==
    /\ pc[p].at = "submit"
    /\ LET m == pc[p].m
           d == pc[p].d
           kept == /\ IF Fixed /\ d # 0
                       THEN UNCHANGED <<drafts, made>>
                       ELSE /\ made < MaxDrafts
                            /\ drafts' = drafts \cup {New(m)}
                            /\ made' = made + 1
                   /\ pc' = [pc EXCEPT ![p] = Idle]
                   /\ claimed' = claimed \ {d}
       IN \/ /\ online
             /\ took' = [took EXCEPT ![m] = @ + 1]
             /\ pc' = [pc EXCEPT ![p].at = "file"]
             /\ UNCHANGED <<lost, drafts, made, claimed>>
          \/ /\ ~online /\ kept
             /\ UNCHANGED <<lost, took>>
          \/ /\ online /\ lost < LostReplies /\ kept
             /\ lost' = lost + 1 /\ took' = [took EXCEPT ![m] = @ + 1]
    /\ UNCHANGED <<online, outages, asked>>

\* Filed in Sent (or not: that's said, and changes nothing here). Fixed:
\* the draft it was goes from Drafts.
File(p) ==
    /\ pc[p].at = "file"
    /\ drafts' = IF Fixed THEN drafts \ Draft(pc[p].d) ELSE drafts
    /\ claimed' = claimed \ {pc[p].d}
    /\ pc' = [pc EXCEPT ![p] = Idle]
    /\ UNCHANGED <<online, outages, lost, made, took, asked>>

Next ==
    \/ GoOffline \/ GoOnline
    \/ \E p \in Senders, m \in Msgs : Ask(p, m)
    \/ \E p \in Senders, d \in 1..MaxDrafts : SendDraft(p, d) \/ Resend(p, d)
    \/ \E d \in 1..MaxDrafts : EditSave(d)
    \/ \E p \in Senders : Read(p) \/ Submit(p) \/ File(p)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
TypeOK ==
    /\ took \in [Msgs -> Nat]
    /\ \A p \in Senders : pc[p].at \in {"idle", "read", "submit", "file"}
    /\ made <= MaxDrafts

\* What can't leave waits in Drafts: each message asked for has gone, or
\* is on its way, or is in Drafts.
DP30_WhatCantLeaveWaitsInDrafts ==
    \A m \in asked :
        \/ took[m] > 0
        \/ \E r \in drafts : r.m = m
        \/ \E p \in Senders : pc[p].m = m

\* ... once: one draft a message.
DP30_OneDraftAMessage ==
    \A r, s \in drafts : r.m = s.m => r = s

\* And it is sent once, never twice. SMTP can't promise it when the reply
\* to a message's last line is lost (LostReplies): the server may have it
\* or not, and the call can't tell. It says so (`LinkError::Unknown`: "may
\* have been sent ... check before sending it again", not an error an agent
\* would retry), keeps it in Drafts in case it didn't go, and the person
\* decides.
DP30_SentOnceNeverTwice ==
    \A m \in Msgs : took[m] <= 1 \/ lost > 0

=============================================================================
