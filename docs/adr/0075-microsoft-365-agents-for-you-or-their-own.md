# 0075. Microsoft 365: an agent works as its person or as itself, and IT says whom it may reach

Status: **Proposed, designed, not built** (2026-10-28). The owner: "We will
want the calendar, mail, chat to be able to work with Microsoft products
and connect in, where people can give the agent its own calendar, mail,
chat and they can lock down who the agent can chat with, and maybe who it
can schedule meetings with…. It's an interesting concept here or some
might want the agent to act under the users authority and others might
want the agent to be its own entity." Answers ADR 0065's open question 5.

## Context
- **What exists:**
  - **Mail** works over IMAP and SMTP (`ImapLink`). Policy can name
    `service = "microsoft-365"` and a `tenant`, but nothing connects to
    it, and mail falls back to the local store.
  - **Calendar** is local files only. There is no `CalendarLink`.
  - **Chat** is our own sealed chat, through the console (`ChatLink`).
  - **Teams** carries approval cards through our bot (ADR 0065 §13),
    tested against stand-ins. No tenant has been tried.
  - There are no Microsoft Graph calls.
- **Who an agent may reach is limited only by tiers.** `mail.send` and
  `calendar.invite` ask first by default.
  - No rule names recipients, attendees, groups or domains. The
    "outside the team" mail test checks only that a send asks.
  - Nothing limits who may write to an agent.
- **What's built for who acted:**
  - Every action records "by Reconciler for Dana" (`AgentPrincipal`,
    AU-10).
  - Mail's `From:` comes from the directory, never from the request.
  - Agents never hold mail credentials; the broker does (ADR 0037).
- **The PRD already wants both ways:**
  - **For the person:** delegated sign-in, each person's own (IN-5), and
    on-behalf-of tokens downstream (PA-4).
  - **As itself:** each agent its own directory identity with a human
    owner, reviewed quarterly and retired after 90 days unused (PA-7),
    and registered in Microsoft Agent 365 (AD-14).
  - **The provider:** IT picks Glasshouse or Microsoft 365 per service
    (IN-1), with providers mixed (IN-4), and agents' actions stay the same
    whichever is chosen (IN-2).
  - Also planned: archiving (AD-16), a review before sending to chosen
    people (AD-17), and which domains may book (SC-4).
- **Microsoft Entra Agent ID** (Microsoft's documentation of August 2026):
  - **An agent identity** is a service principal made from a blueprint
    (which holds the credentials), with a sponsor who answers for it, and
    governed by Conditional Access.
  - **It runs in one of three ways:**
    - on behalf of a signed-in person (OBO): the person is the subject
      and the agent the actor;
    - service to service: the agent as itself;
    - as an "agent's user account" with its own mailbox, calendar and
      Teams presence.
  - The agent's user account is available only in the Frontier preview
    program.
- **Exchange Online can scope an app** to the mailboxes it names (RBAC for
  Applications), and a mailbox can accept mail only from senders it names
  (`AcceptMessagesOnlyFrom`).

## Decision
1. **IT picks the provider per service (IN-1, IN-4):**
   ```toml
   [microsoft]
   tenant = "acme.onmicrosoft.com"

   [mail]
   service = "microsoft-365"     # or "imap", or Glasshouse Mail (ADR 0037)

   [calendar]
   service = "microsoft-365"     # or "glasshouse"

   [chat]
   service = "teams"             # or "glasshouse"
   ```
   - Agents' actions keep their names and tiers whatever the provider
     (IN-2): `mail.send` is `mail.send`.
   - New links in `glass-gateway-providers`:
     - Graph mail (a `MailLink`);
     - Graph calendar (a new `CalendarLink`, which the local calendar
       implements too);
     - Teams chat (a `ChatLink`).
   - IT consents once, for the org, in the console (IN-3).
2. **An agent works one of two ways, set by IT per agent template.** A
   group may forbid either way. A person can't widen it.
   - **For you (delegated; PA-4, IN-5):**
     - The person signs in to Microsoft once. The agent's mail goes from
       their mailbox, its invitations from their calendar, its messages
       as them.
     - Each is marked as the agent's work:
       - a line in the message;
       - our `X-Glasshouse-Attribution` header;
       - with Agent ID registered, Entra's own logs show the agent as the
         actor for the person.
     - It reaches what the person can reach, and no more.
   - **Its own (PA-7, AD-14):**
     - The agent is an Entra agent identity, made from our blueprint (one
       blueprint, published for every customer's tenant). Its owner and
       sponsor is the person; IT can add others.
     - It has a mailbox, a calendar and a place in Teams of its own:
       - **with Microsoft's agent user account** where the tenant has it
         (in preview today);
       - **otherwise, a mailbox IT makes for it** (a shared mailbox),
         reached as an app scoped by Exchange's RBAC for Applications to
         that mailbox alone. Its Teams presence is then our bot, as for
         approvals (ADR 0065 §13).
     - What it sends is from its own address and says whom it works for:
       "Reconciler, working for Dana Alvarez".
     - **Its lifetime (PA-7):**
       - its owner reviews it each quarter;
       - it is disabled after 90 days unused;
       - it is removed with its persona.
3. **Whom an agent may reach, ours to enforce** in the broker, for every
   provider and both ways:
   ```toml
   [[group]]
   name = "finance"
   [group.reach]
   mail    = ["internal", "partners.example.com"]
   invite  = ["internal"]
   chat    = ["group:finance", "manager"]
   outside = "ask"                # or "never"
   ```
   - **What a list holds:**
     - people's addresses;
     - `group:<name>`, a directory group;
     - domains;
     - `internal`, the org's own domains;
     - `manager`, the person's manager.
   - **Each recipient and attendee is checked on its own.**
     - Those on the list go at the action's tier.
     - Anyone else is `outside`: an approval naming them, or Never.
   - The most restrictive of org, group and agent wins, as for
     `max_label`. SC-4's domains that may book use the same list.
   - **A refusal is a result the agent can read** ("Finance's agents
     email only internal addresses and partners.example.com"), and it is
     on the record.
   - We recommend IT set the same limits in Exchange and Teams, so a
     second wall holds if ours is ever wrong.
4. **Who may reach an agent with a mailbox or chat of its own.**
   - Anyone who can write to an agent can try to steer it with what they
     write.
   - So the broker gives the agent only messages from senders on its
     `from` list (by default, the org's own people).
   - Others are held for its owner to let through or delete, and never
     put in front of the model unseen.
   - Text that came from outside the org is marked as such when the agent
     reads it.
   - We recommend `AcceptMessagesOnlyFrom` on its mailbox to match.
5. **Where the credentials live. Never where an agent or the desktop can
   read them.**
   - **A person's delegated sign-in** (their refresh token) is in their
     secrets dataset:
     - root-owned and 0700;
     - encrypted with the person's own key (ADR 0073);
     - kept and moved with their home.

     It isn't in their home, because their desktop and their agents run
     as them.
   - **glassd uses it for each call,** as it does mail credentials today.
     - On today's agent hosts, before ADR 0073, it is in glassd's own
       root-only state for that person.
   - **Revoking the person's sessions in Entra** ends it, wherever it is.
   - **An agent identity's credential** (its blueprint's certificate) is
     the org's, in AWS Secrets Manager, read by glassd. It is never in
     anyone's dataset.
6. **On the record:**
   - each call as now, saying which way it acted ("for Dana" or "as
     Reconciler"), through which provider, to whom, and the decision;
   - each message held, and each released.

   Message bodies aren't in our record. Archiving is Exchange's own
   journal (AD-16).

## Slices
1. **Whom agents may reach (§3, §4)** in the broker, with tests. This
   works with any provider, so it can be built now, on the local and
   IMAP mail and the local calendar.
2. **Graph mail, for you:** the person's sign-in, the token in glassd's
   root-only state, and sends with attribution. Tested against a Graph
   stand-in, as Teams was.
3. **Graph calendar:** `CalendarLink`, and invitations under the reach
   lists.
4. **Its own:** a mailbox IT makes, reached as an app scoped to it, with
   the inbound hold.
5. **Teams chat** as a `ChatLink`.
6. **Agent ID:** our blueprint, agent identities made per agent, and the
   owner's reviews (AD-14, PA-7).

## Prerequisites
- **An Entra tenant to test against.** None has been tried: the Teams
  relay and this both need one.
- **The admin console in the AWS stack**, for IT's provider, identity
  and reach settings (as in ADR 0074).

## Open questions (owner)
1. The default for a new agent template: for you (proposed: what the
   person can do, nothing more), or its own?
2. Messages held from outside senders: to the agent's owner only
   (proposed), or also to IT?
