# 0067. Each person reaches only their own agents' computers

Status: **Accepted, built** (2026-10-28), but for Teams (below). The owner: "We also need
to make sure people can only access their computers and not others
security and auth is important." Settles ADR 0066's open "Identity on a
host".

## Context
- On a laptop, glassd knows who is calling by the uid the kernel reports
  for its control socket's peer. That is never something in the request.
  A person reaches only their own tasks, approvals, events and agent
  computers (ADR 0066), and another person's computer is as absent as one
  that doesn't exist. Tests prove it.
- On an agent host in the buyer's AWS (ADR 0065 §9), people don't have
  accounts. They sign in at the load balancer (OIDC with Entra ID, or
  Identity Center through Cognito), and the front (`glass-agents`) calls
  glassd for them. Until now the front acted as the one account it runs
  as, so on a shared host everyone would have shared one identity, and
  each other's computers.

## Decision
1. **Every person gets an account of their own on every host.**
   - glassd keeps a registry of the people who signed in. A person's key
     is their sign-in subject (`sub`), which is stable across hosts and
     sessions.
   - Each person gets a uid from a reserved range, kept on the host. It
     has no password and no login; it exists only to own their things.
   - Their tasks, approvals, events and computers are keyed by that uid,
     as a laptop's person's are, so everything already enforced and
     tested holds between people unchanged:
     - their computers' folders are 0700 and theirs;
     - agents run as their uid outside the computer and root inside;
     - nobody else sees their tasks or answers their approvals.
   - Their computers in the store are keyed by their subject, not their
     uid, so any host can wake them.
2. **The front may say who it acts for; glassd checks it.**
   - A connection to glassd that names a person (`session.as`) is
     accepted only from the front's own account, by the uid the kernel
     reports. Any other caller is refused and the refusal is recorded.
   - glassd also checks the proof of who the person is:
     - **Signed in at the load balancer:** the load balancer's signed
       assertion (`x-amzn-oidc-data`). glassd checks:
       - ES256 with AWS's published key for its `kid`;
       - its `signer` is exactly this stack's load balancer's ARN;
       - it hasn't expired.

       The person is its `sub`; their name and email come from it too.
     - **The `glass` CLI:** a token glassd issued to that person while
       they were signed in. glassd keeps only its hash; tokens expire
       (30 days), and the person or IT revokes them.
   - So a compromised front can act only for the people whose sign-ins
     pass through it at that moment, never for anyone it names.
   - Every session opened is on the record, and so is every one refused.
3. **The front keeps people apart.**
   - Each HTTP request opens its own glassd session as its person, from
     that request's proof. Nothing is shared between people's requests.
   - A request with no proof, or a proof glassd refuses, gets 401.
   - Long polls subscribe to glassd's events as their person, so each
     sees only their own.
4. **Teams, by an explicit link, every step checked by glassd.**
   - Entra's user-info claims carry no Entra object id, so the load
     balancer's assertion can't say who someone is in Teams.
   - A Teams call reaches glassd with Microsoft's own proof. The front
     passes on the Bot Connector's signed token and the activity, and
     glassd checks them as the front did (RS256 against Microsoft's
     keys, this app, this tenant, the `serviceUrl`): only then does it
     believe the Entra object id in it.
   - The app installed for someone (a signed activity) gets glassd to
     issue a one-time link for that Entra id. The front sends it to their
     chat. Opened in the browser by someone signed in at the load
     balancer, it ties the two together, and glassd records it.
   - From then on, that person's questions go to their Teams chat, and
     only a signed answer from that Entra id counts.
   - A compromised front can't link an Entra id it has no signed
     activity for, nor answer as someone in Teams.
5. **Development stays simple.**
   - On a laptop or a development host, the front acts for the account
     that runs it, as before (no `session.as`).
   - `make agents-dev` can play several people with development
     assertions. glassd accepts those only when its config says
     `dev_people = true`, which a shipped config never sets.

## As built
- glassd's `[people]` names the front's account (`glass-agents`), the
  load balancer's ARN and the egress relay. The registry
  (`people.json`, 0600) keeps people, the hashes of their tokens, and
  one-time login codes. People's uids start at 200000.
- `session.as` comes from the front's uid only. Any other account is
  refused, with good claims too, and the refusal is recorded. The
  front's own account gets nothing without a session.
- The front opens one glassd session per request, as that request's
  person (a task-local connection). Its live updates follow that
  person's events only, with a time cursor, so nothing of anyone else's
  reaches them.
- One record line per person and kind of proof every ten minutes, since
  sessions are per request. Every refusal is recorded, every token
  issued, every revocation.
- A computer's key in the store is its owner's sign-in subject, hashed
  (`p` + 24 hex), so any host wakes it for them (ADR 0068).
- **`glass login`** (the owner asked how the CLI and desktop get a token):
  1. `glass` waits on this computer's loopback address and opens the
     browser at the front's `/login/cli`, which the load balancer signs
     the person into.
  2. The page asks "Let glass on <computer> use your agents?".
  3. Allowing it gets glassd to issue a one-time code (2 minutes). The
     browser takes it to `glass` on `127.0.0.1`.
  4. `glass` trades it once (`POST /api/v1/tokens`, the code as the
     bearer, which the stack's API rule lets through) for a token. The
     token is kept in `~/.config/glasshouse/agents.json` (0600) for 30
     days.

  The token itself never passes through the browser. `glass logout`
  revokes all of the person's tokens. The Glasshouse desktop signs in the
  same way, keeping its token in the person's keyring. Trading the
  desktop's own sign-in token for one is a later refinement.
- Tested: `tests/local-aws/people.sh` (36 checks, two people behind the
  stand-in load balancer), runtime `people::tests` and front
  `adr0067_*`.
- Found by testing two people: computers lived inside glassd's state
  folder, which is closed to everyone but root. Every computer whose
  owner wasn't root, a laptop user's too, couldn't reach its own disk.
  Computers now live beside the state folder, and the sandbox refuses to
  start where a folder above them is closed to their owners.

## What it doesn't do yet
- **Teams on a shared host.** The front refuses Teams' calls in
  load-balancer mode until glassd checks the Bot Connector's token itself
  and the one-time link (decision 4) ties an Entra id to a person.
- **Delegates** (a manager's assistant answering for them) and IT's view
  of everyone's agents (as root sees all on a laptop) are later, each with
  its own record.
- **The registry is per host.** A person who signs in on another host
  gets an account there; their computers follow them through the store,
  keyed by subject.

## Consequences
- glassd verifies ES256: P-256 ECDSA, which glass-crypto already has. It
  fetches AWS's keys through the egress relay, as allowed by the stack.
- The control socket's identity rule ("the caller by kernel uid, never
  anything in the request") gains one exception, and only this one: the
  front, known by its uid, may name a person, and only with a proof glassd
  checks itself.
