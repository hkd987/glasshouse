---------------------------- MODULE SupportAccess ----------------------------
(***************************************************************************)
(* Who may do what in the admin console, request by request, while IT     *)
(* publishes and rolls back policies that add, narrow or take away a      *)
(* support role, and the console restarts with a new directory (CF-80,    *)
(* CF-81, ADR 0028 slice a).                                               *)
(*                                                                         *)
(* j is help desk staff: in the directory group a role names, while a     *)
(* published policy has the role. p is one of IT's admins. The role, in  *)
(* each version, is "none" (no role names j's group), "narrow" (it        *)
(* reaches AP's people: dana) or "wide" (everyone). One computer has two  *)
(* people on it, dana and the CEO.                                         *)
(*                                                                         *)
(* The code, each action one step under one lock:                          *)
(*  - crates/bin/glass-console/src/server.rs: POST /api/session (SignIn:  *)
(*    the check, then the token kept), DELETE (SignOut), and every other  *)
(*    request: the token's person, their access from the directory and    *)
(*    the roles the pilot ring runs (Decide: `access`, `api`, `session`), *)
(*    then the Support page (Respond: views::support).                    *)
(*  - access.rs Access::of; state.rs support_in_force (the pilot ring's   *)
(*    version's roles, parsed once per version: a version's text never    *)
(*    changes, so Content(pilot) is what it returns), publish, rollback  *)
(*    (a new version, the draft replaced).                                 *)
(*  - The directory is read at start and fixed while the console runs; a  *)
(*    new one comes with a restart, which also forgets every session     *)
(*    (main.rs, Console::new).                                             *)
(*  - Promote moves early and broad, never the pilot ring: left out.      *)
(*                                                                         *)
(* Old holds the code before ADR 0054, to show TLC needs each fix:        *)
(*  - "keepToken": a request that found its person no admin and in no    *)
(*    role said "sign in" but kept the session, which worked again once   *)
(*    IT gave the role back;                                               *)
(*  - "twoReads": the Support page read the roles in force again after    *)
(*    the request was decided, so a publish between mixed two policies    *)
(*    on one page;                                                         *)
(*  - "allUsers": the page named everyone on a computer it showed, not    *)
(*    only the people the role reaches.                                    *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  Old,          \* fixes switched off, as above
  MaxVersions,  \* policy versions, the first included
  MaxRestarts,  \* console restarts
  Tokens        \* session tokens (each made once)

People == {"j", "p"}
Groups == {"desk", "admins"}
Contents == {"none", "narrow", "wide"}
Routes == {"support", "admin", "session"}
OnComputer == {"dana", "ceo"}
NoOne == "-"
NoPage == [from |-> 0, tools |-> 0, users |-> {}]
Idle == [phase |-> "idle", v |-> 0, u |-> "-"]

\* Whom a role reaches: the people whose computers it may help with.
Reach(c) == CASE c = "none" -> {} [] c = "narrow" -> {"dana"} [] c = "wide" -> OnComputer

VARIABLES
  dir,        \* each person's directory groups, as this run of the console read them
  versions,   \* the role in each published version
  pilot,      \* the version the pilot ring runs
  draft,      \* the role in IT's draft
  sessions,   \* each token's person, NoOne once ended or before it's made
  made,       \* tokens made so far (a token is never made twice)
  signing,    \* a sign-in checked and not yet kept: its person
  req,        \* each token's request under way: decided, by which version
  page,       \* the last Support page each token got
  ended,      \* tokens a request found with no admin and no role
  revived,    \* a request on an ended token was let through
  restarts

vars == <<dir, versions, pilot, draft, sessions, made, signing, req, page, ended, revived, restarts>>

Content(v) == versions[v]

\* access.rs Access::of: an admin by the directory; a role when a group of
\* theirs is named by the role the pilot ring runs.
Admin(u) == "admins" \in dir[u]
Role(u, v) == "desk" \in dir[u] /\ Content(v) # "none"
Any(u, v) == Admin(u) \/ Role(u, v)

TypeOK ==
  /\ dir \in [People -> SUBSET Groups]
  /\ versions \in Seq(Contents)
  /\ pilot \in 1..Len(versions)
  /\ draft \in Contents
  /\ sessions \in [Tokens -> People \cup {NoOne}]
  /\ made \subseteq Tokens
  /\ signing \in [Tokens -> People \cup {NoOne}]
  /\ ended \subseteq Tokens
  /\ revived \in BOOLEAN

Init ==
  /\ dir = [u \in People |-> IF u = "j" THEN {"desk"} ELSE {"admins"}]
  /\ versions = <<"narrow">>
  /\ pilot = 1
  /\ draft = "narrow"
  /\ sessions = [t \in Tokens |-> NoOne]
  /\ made = {}
  /\ signing = [t \in Tokens |-> NoOne]
  /\ req = [t \in Tokens |-> Idle]
  /\ page = [t \in Tokens |-> NoPage]
  /\ ended = {}
  /\ revived = FALSE
  /\ restarts = 0

-----------------------------------------------------------------------------
(* IT.                                                                     *)

\* The Remote help page edits the draft's role (edits.rs Edit::SupportRole).
Edit(c) ==
  /\ c # draft
  /\ draft' = c
  /\ UNCHANGED <<dir, versions, pilot, sessions, made, signing, req, page, ended, revived, restarts>>

\* state.rs publish: the draft as the next version, to the pilot ring.
Publish ==
  /\ Len(versions) < MaxVersions
  /\ draft # Content(pilot)
  /\ versions' = Append(versions, draft)
  /\ pilot' = Len(versions) + 1
  /\ UNCHANGED <<dir, draft, sessions, made, signing, req, page, ended, revived, restarts>>

\* state.rs rollback: an earlier version's content as a new version,
\* everywhere at once, and in the draft.
Rollback(v) ==
  /\ Len(versions) < MaxVersions
  /\ v < pilot
  /\ versions' = Append(versions, Content(v))
  /\ pilot' = Len(versions) + 1
  /\ draft' = Content(v)
  /\ UNCHANGED <<dir, sessions, made, signing, req, page, ended, revived, restarts>>

\* main.rs: the console starts again with the directory as it is now
\* (someone moved into or out of a group); sessions live in memory only.
Restart ==
  /\ restarts < MaxRestarts
  /\ restarts' = restarts + 1
  /\ dir' \in [People -> SUBSET Groups]
  /\ sessions' = [t \in Tokens |-> NoOne]
  /\ signing' = [t \in Tokens |-> NoOne]
  /\ req' = [t \in Tokens |-> Idle]
  /\ UNCHANGED <<versions, pilot, draft, made, page, ended, revived>>

-----------------------------------------------------------------------------
(* Someone at the console.                                                 *)

\* POST /api/session: they may sign in if they're an admin or hold a role.
SignInCheck(u, t) ==
  /\ t \notin made
  /\ Any(u, pilot)
  /\ made' = made \cup {t}
  /\ signing' = [signing EXCEPT ![t] = u]
  /\ UNCHANGED <<dir, versions, pilot, draft, sessions, req, page, ended, revived, restarts>>

\* ...and the token kept, under the sessions' lock.
SignInKeep(t) ==
  /\ signing[t] # NoOne
  /\ sessions' = [sessions EXCEPT ![t] = signing[t]]
  /\ signing' = [signing EXCEPT ![t] = NoOne]
  /\ UNCHANGED <<dir, versions, pilot, draft, made, req, page, ended, revived, restarts>>

\* DELETE /api/session.
SignOut(t) ==
  /\ sessions[t] # NoOne
  /\ sessions' = [sessions EXCEPT ![t] = NoOne]
  /\ UNCHANGED <<dir, versions, pilot, draft, made, signing, req, page, ended, revived, restarts>>

\* Any request: the token's person and what they may do now. One with no
\* admin and no role is refused, and (since ADR 0054) its session ends.
Decide(t, route) ==
  /\ sessions[t] # NoOne
  /\ req[t] = Idle
  /\ LET u == sessions[t]
         allowed == CASE route = "admin" -> Admin(u)
                      [] route = "support" -> Role(u, pilot)
                      [] route = "session" -> Any(u, pilot)
     IN
     /\ revived' = (revived \/ (allowed /\ t \in ended))
     /\ IF Any(u, pilot)
          THEN /\ ended' = ended
               /\ sessions' = sessions
          ELSE /\ ended' = ended \cup {t}
               /\ sessions' = IF "keepToken" \in Old
                                THEN sessions
                                ELSE [sessions EXCEPT ![t] = NoOne]
     /\ req' = IF route = "support" /\ allowed
                 THEN [req EXCEPT ![t] = [phase |-> "decided", v |-> pilot, u |-> u]]
                 ELSE req
  /\ UNCHANGED <<dir, versions, pilot, draft, made, signing, page, restarts>>

\* views::support. Since ADR 0054 it reads the roles in force once, decides
\* again by them, and builds the whole page from them; before, the person's
\* tools and computers came from the access decided earlier and the tools'
\* roles from a second read.
Respond(t) ==
  /\ req[t].phase = "decided"
  /\ LET u == req[t].u
         from == IF "twoReads" \in Old THEN req[t].v ELSE pilot
         users == IF "allUsers" \in Old \/ Content(from) = "wide"
                    THEN OnComputer
                    ELSE OnComputer \cap Reach(Content(from))
     IN
     /\ page' = IF Role(u, from)
                  THEN [page EXCEPT ![t] = [from |-> from, tools |-> pilot, users |-> users]]
                  ELSE page
     /\ revived' = (revived \/ (Role(u, from) /\ t \in ended))
  /\ req' = [req EXCEPT ![t] = Idle]
  /\ UNCHANGED <<dir, versions, pilot, draft, sessions, made, signing, ended, restarts>>

Next ==
  \/ \E c \in Contents : Edit(c)
  \/ Publish
  \/ \E v \in 1..Len(versions) : Rollback(v)
  \/ Restart
  \/ \E u \in People, t \in Tokens : SignInCheck(u, t)
  \/ \E t \in Tokens : SignInKeep(t) \/ SignOut(t) \/ Respond(t)
  \/ \E t \in Tokens, r \in Routes : Decide(t, r)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* What the console promises.                                              *)

\* A session a request found with no admin and no role ended: it never lets
\* anything through again, though IT gives the role back. They sign in.
ADR0028_AnEndedSessionStaysEnded == ~revived

\* A Support page is one policy's: the person's tools and computers and
\* which roles hold each tool, all from the same version.
ADR0028_APageIsOnePolicys ==
  \A t \in Tokens : page[t] # NoPage => page[t].from = page[t].tools

\* It names, on a computer it shows, only the people the role reaches.
CF81_ThePageNamesOnlyWhoTheRoleReaches ==
  \A t \in Tokens : page[t] # NoPage => page[t].users \subseteq Reach(Content(page[t].from))

=============================================================================
