# 0068. One computer per person; their agents are personas on it

Status: **Accepted, built** (2026-10-28). The owner: "Each user gets their
own computer they can send directions into, maybe they have multiple
agents personas in their desktop UI but each one routes to the human's
computer." This replaces ADR 0066's "each agent has a computer of its own".

## Context
- ADR 0066 gave every agent a computer of its own. A person with a
  reconciler, a researcher and a month-end closer had three computers:
  - three disks to keep and sleep;
  - three meters running;
  - three places to look for what was done;
  - and the agents couldn't build on each other's work.
- People think of "my computer", where their agents do things for them,
  much as their laptop is one computer running many apps.

## Decision
1. **Each person has one computer** (id `computer`). It's made the first
   time they add an agent, and kept, slept and woken as ADR 0065 §11 and
   ADR 0066 say. What's metered is a person's computer hours.
2. **Their agents are personas on it.**
   - A persona is a name, a role and a catalog agent to run: "Reconciler,
     matches invoices, runs the desktop agent".
   - It is kept in the computer's record, so it goes where the computer
     goes: asleep in the store, and to another host that wakes it.
   - A person keeps up to 20.
   - Ids and names are each the person's once.
   - A persona may run only an agent IT made available to that person
     (`agents_for`, as before).
   - Removing a persona leaves what it made on the computer.
3. **A task names the computer and the persona** (`TaskRequest.computer`,
   `TaskRequest.persona`).
   - glassd checks that the persona is on that computer, the caller's,
     and runs that persona's agent.
   - `TaskView.persona` says whose task it is, so the front shows each
     persona's conversation apart.
4. **Personas share the computer, on purpose.**
   - Several of one person's personas may work at once. Each task starts
     in its persona's folder (`/work/<persona>`), and all of the computer
     is theirs to share: the researcher can read what the reconciler
     wrote, as one person's apps share their home.
   - The screen is the computer's, each task headed by its persona's
     name (`── task glass-… ── Reconciler`).
   - Taking over, sleep and wake are the computer's: they change it for
     every persona. Idle means no persona used it.
   - A person who wants agents kept apart from each other's files gives
     them different computers. That isn't built: no one asked yet.
5. **The front's API keeps its shape.** An agent in the web app, the CLI
   and Teams is a persona.
   - Its `computer` is the person's computer.
   - `/agents/{id}/screen`, take-over, sleep and wake act on that
     computer.
   - `POST /agents` adds a persona (`persona.make`).

## What "desktop UI" means here
- The agents web app (`apps/agents`) is the desktop UI that exists, and is
  what this changes.
- The Glasshouse OS desktop routing its agents to the person's cloud
  computer is a later integration. It signs in as the CLI does (ADR 0067)
  and uses the same API.

## Consequences
- Records written before this change, with a computer per agent, still
  load. They have no personas, so the front doesn't show them.
- glassd's record changes take the computer's `moving` guard, so a
  persona added during a sleep isn't lost when the sleep writes its record.
- `specs/tla/AgentComputers.tla` models one computer and the tasks run in
  it. Personas are tasks' owners, not new states: the model's Tasks are a
  person's agents' tasks.
