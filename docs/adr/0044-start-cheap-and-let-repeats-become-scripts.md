# 0044. Start on the cheapest model; let repeats become scripts

Status: **Proposed, first slice built** (2026-10-22). The owner asked for
it: "pick the right model for a task to start with so we can try to get
things done the cheapest way possible … we should have our harness write
code so that it can then just call scripts that do the work in cases of
automations." PRD MP-8 (routing by task and cost), DA-4 (routines),
NV-21 (replay code; the agent takes over only when replay fails), AE-1
(scripts in a harness bundle), SK-2 (scores recorded per model). Builds
on ADR 0018 (the gateway), 0041 (tool use, the desktop agent) and 0043
(Anthropic's API).

## Context
- Measured 2026-10-22 on Anthropic's API: the desktop agent over a seeded
  workday (`tools/dev/seed-workday.py`). Eight tasks:
  - which invoices are unpaid and the total;
  - inbox triage;
  - the calendar for the rest of the day;
  - proposing a free slot and drafting the reply;
  - drafting a chase from an address in another email;
  - a send that policy forbids (the agent may only draft);
  - a folder it wasn't given;
  - a call prep across mail, calendar and invoices.

  | Model | Tasks right | Cost of all eight | Model calls |
  |---|---|---|---|
  | Claude Haiku 5.5 | 8 of 8 | $0.0065 | 27 |
  | Claude Sonnet 5.5 | 8 of 8 | $0.113 | 27 |
  | Claude Opus 5.5 | 8 of 8 | $0.220 | 27 |

  Same tools, same steps. The prompt cache keeps the instructions cheap,
  so the price per token is most of the difference: about 30 to 1.
- Where Haiku slips is precision:
  - counting days between dates (23 for 22);
  - a sum (892.05 for 893.05, once in two runs);
  - attendees (three for two);
  - a currency it assumed.

  Asked for "exact numbers" (days overdue per unpaid invoice, and the
  total weighted by days), it got one date wrong. Its total was 20,610.40
  where the answer is 20,147.60, and it didn't ask for a stronger model.
  Opus got it right, for $0.046 to Haiku's $0.001.
- Policy routes by data label: which models may see a call's data, in
  order, the rest taking over on an outage (MP-8). Agents may name a
  model; most don't. Nothing chose by task or cost.
- The work agents repeat (a weekly reminder, a Monday report) runs the
  model every time, though the steps are the same each time. NV-21 asks
  for replay code; DA-4 for routines.

## Decision
**Built:**
1. **Agents may ask for the cheapest or the strongest model, where IT
   lets them.**
   - `model.complete` takes `prefer: "cheapest" | "strongest"` when it
     names no model.
   - With `[models] by_cost = true`, the route policy computed for the
     call's data is put in order of policy's prices: output, then input.
     Unpriced models go last, in policy's order.
   - Failover walks the rest as before. The audit record's route says
     "cheapest first" or "strongest first".
   - Off by default: then the route's order stands, since IT may have
     listed a model only as an outage fallback.
   - Policy still decides which models a call's data may go to. This
     only orders them.
2. **The desktop agent starts every task on the cheapest model and
   climbs one step at a time.** Haiku hands over to Sonnet before Opus
   (the owner: "we don't have to jump from Haiku to Opus").
   - **One step up.** `model.complete` takes `above: <model>` with
     `prefer: "cheapest"`. The route then puts the cheapest model priced
     above that one first, then that one, then the cheaper ones. So a
     step up never goes down, and at the top the top stays first.
   - **The agent knows the top.** Each reply says whether a dearer
     approved model could take the next step (`stronger`), so the agent
     stops climbing there without spending a call to find out.
   - **When it climbs.** The rest of the task moves up a step when:
     - the model calls `use_a_stronger_model`, once per step up. It is
       offered from the first step, so the tools never change mid-task
       and the replayed thinking and the cache hold (ADR 0043). On
       Anthropic's API, Sonnet 5.5 and Opus 5.5 read Haiku 5.5's
       thinking.
     - a step is cut off at `max_tokens`. That step is asked again on
       the next model up.
   - **Seen working.** Live, asked to hand an exact-figures task up,
     Haiku did, and Sonnet answered it right: $0.020, where Opus alone
     was $0.046. Against a stand-in that cut off every first step, the
     agent climbed Haiku, Sonnet, Opus and stopped with the reason.
3. **Self-escalation is a convenience, not the safety net.** Haiku
   called `use_a_stronger_model` only when the request told it to. It
   didn't for exact figures it then got wrong. The safety nets are
   below.

**Proposed, not built (the owner's go-ahead first):**

4. **Exact work goes to code, not to a bigger model.** A step that needs
   exact dates, sums or counts writes a few lines of code that run on the
   tools' data, and the model reads the result. On Haiku that is right
   and costs a fraction of a cent, where Opus guessing well costs 46
   times as much.
5. **Automations become scripts** (DA-4, NV-21).
   - **Written once.** "Do this every Monday" has the strongest model
     write a short program against the task's own tools: the action
     registry's typed actions, NV-1. It has ordinary loops, conditions
     and arithmetic, and no model inside. A step that needs judgment
     ("is this email a complaint?") calls `model.complete` with
     `prefer: "cheapest"`.
   - **An item like a skill** (ADR 0034). It has an owner and versions.
     The person sees what it calls, at which tiers, before it's saved.
     Changes are proposals.
   - **Run as a task, in the same jail.** Same grants, tiers, asks,
     Never, labels, audit and undo. Its only reach is its tools on the
     task socket, so it can do nothing an agent couldn't.
   - **When it breaks, the agent takes over.** A failed call, or a result
     the script's own checks reject, stops the run. The agent carries on
     from there on the cheapest model (NV-21) and can propose a fixed
     version.
6. **One runtime for both**: `glass-script-runner`, an agent binary
   embedding a JavaScript engine written in Rust (a candidate is
   `boa_engine`; licences still to check).
   - It exposes only `tools.<name>(input)`, over the task socket. There
     are no files, network, clock or processes beyond what the tools
     give, and it runs in the agent jail like any agent.
   - Models write JavaScript well.
   - Considered:
     - a JSON plan format: safest to show, but loops and arithmetic make
       it a language anyway;
     - TypeScript on the OS's browser engine: heavier, for a headless
       run;
     - Python or shell: not in the base system (ADR 0007), and they reach
       files.
   - A new probe check proves the runner reaches nothing but its task
     socket.

## Consequences
- An org that turns `by_cost` on pays about a thirtieth for the desktop
  agent's everyday tasks. Answers that hang on exact figures need item 4
  before they rely on the cheapest model. Until then, IT can leave
  `by_cost` off, or approve only a stronger model for that data.
- IT keeps the say: which models a label's data may go to, and whether
  agents may choose among them by cost.
- Each step up restarts the prompt cache once, since caches are per
  model.
- Scripts make a routine's code steps free and the same every time.
  Each new routine then has a reviewed artifact, which is the point:
  the person approves what will run, not only what ran.

## Not built yet
- Items 4 to 6.
- Choosing the starting model per kind of task from evals (SK-2 scores
  per model) instead of always the cheapest.
- A catalog setting for an agent's starting model. Single-call agents
  (the IT agent) can already name their model in the catalog.
