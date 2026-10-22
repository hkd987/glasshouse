# 0044. Start on the cheapest model; let repeats become scripts

Status: **Accepted, built** (2026-10-22). The owner asked for
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

**Built after the owner's go-ahead** ("Yes to all the suggestions",
2026-10-22):

4. **Exact work goes to code, not to a bigger model.** The desktop
   agent offers the model `run_script` from the first step. The model
   writes a short JavaScript script, and the agent runs it.
   - **What a script can do.** `call(tool, input)` makes one of the
     model's own tool calls on the task socket, under the same rules,
     and returns `{ok, text, data}`.
   - **Helpers** for what models get wrong by hand: `today()` (the
     computer's date), `daysBetween`, `addDays`, `weekday`, `round`,
     `print`.
   - **The instructions** say to use it for exact figures.
   - **Measured.** Haiku 5.5 got the exact-figures task right three
     times in three, for about $0.001 each: 20,147.60. By hand it said
     20,610.40, and Opus 5.5 by hand cost $0.046.
6. **One runtime**: the `glass-script` library, which the desktop agent
   runs in its own process, in its jail.
   - **The engine is Boa**, JavaScript written in Rust, MIT or
     Unlicense, with no default features. It adds 134 crates and about
     30 s to a clean build, and nothing to the edit loop.
   - **Considered:**
     - Rhai: 16 s and 30 crates, but `smartstring` is MPL-2.0, outside
       `deny.toml`'s list. That is the owner's legal call.
     - Starlark: 59 s and 181 crates.
     - QuickJS: C, against the no-C-build-scripts rule.
     - A JSON plan format, TypeScript on the browser engine, Python or
       shell, as before.
   - **Its only reach is `call`, over the task socket.**
     - Every `import` is refused: Boa's default loader would read
       modules from the current folder, so the idle loader is set.
     - Boa has no files, network, processes or timers, and the host
       adds none.
     - Loops stop at 100,000 iterations, calls at 256 deep, and tool
       calls at 100 a script.
     - A broken socket stops the script even inside a `catch`.
     - Memory is the jail's limit. A script stuck inside one built-in,
       such as a runaway regular expression, runs until the task is
       stopped; its tile shows "Running a script" meanwhile.
     - Whole numbers cross as integers, as tools' schemas ask.
     - A script may `return` its result or `await` its calls, as models
       write them: either runs as an async function's body. Haiku's
       first script in a live remix awaited a call and failed to parse.
     - Tests prove each of these (`glass-script`). The e2e suite runs a
       script in the agent's jail, its call decided and audited, and a
       script that imports `/etc/passwd` fails there.
   - **No new probe check.** The runner adds no reach to the agent's
     process; the jail and the probe's 27 checks bound it as before.
7. **Cost ordering where we set the models** (2026-10-22):
   - An owner who connects their own Anthropic account (ADR 0039) gets
     Haiku and Sonnet beside Opus, with `by_cost` on, unless policy set
     models of its own. The desktop agent starts on Haiku. Agents that
     don't choose still get Opus.
   - `glass-e2e real-model-policy … --ladder` writes the same ladder for
     `tools/dev/real-desktop.sh`, `real-remix.sh` and
     `fake-model.sh --ladder`.

**Built after the go-ahead, continued** (2026-10-22):

5. **Automations become scripts** (DA-4, NV-21), as `glass-app-routines`.
   - **Written once.** Asked for something regularly ("every weekday at
     9:00, tell me which invoices are unpaid"), the desktop agent writes
     a script against the task's own tools, tries it with `run_script`,
     and saves it with `routines.save`. The model is whichever the
     ladder gives; Haiku wrote a good one. Its script used the listing's
     data, counted money in integer cents and returned a line per
     invoice. Saving it cost $0.0019.
   - **What the person approves.** `routines.save` is Ask. Its review
     shows the request, the schedule, what it reads and changes, the
     tools the script calls and the script itself, every line. A new
     version is another save the person approves.
   - **Kept like the apps people make** (ADR 0032). Every version is a
     file under glassd's state, root only, never changed. The person's
     list is a file in their home, written as them, so task undo takes
     a routine away. `routines.delete` (Notify) does too.
   - **The same scope.** A routine keeps the grants of the task that
     saved it, never paths from the input (DA-4).
   - **Run as a task, in the same jail.** It runs on `glassctl routine
     run <id>` or on its schedule. A schedule is `daily 18:30`,
     `weekdays 09:00`, `mon,thu 08:30` or `hourly`, in the computer's
     local time; glassd checks once a minute and starts it as its owner.
     A minute the computer slept through doesn't run late. A run is a
     task of the agent that saved it, under policy as it is then, with
     `GLASS_ROUTINE` naming it. The agent reads it over its socket
     (`routines.read`, audited) and runs the script. The live run made
     no model call and gave the same total.
   - **When it breaks, the agent takes over** (NV-21). A script that
     fails, or throws on a result it rejects, hands the request to the
     model with what went wrong, on the cheapest model when by cost.
     The model says the routine needs fixing and may save a fixed
     version, which the person approves.
   - **Policy.** The default policy gives the desktop agent `routines.*`.
     A routine can do only what that agent may, with the scope it was
     saved with, and the person approves every save.
   - **Containment.**
     - A routine's id or its owner's login never becomes a path. A
       person may edit their list by hand, but `../..` picks nothing.
     - A kept version must name its owner, id and number, so glassd,
       reading as root, never runs a file put there under another name.
     - The probe's new check: an agent can't open glassd's routines
       (`/var/db/glasshouse/routines`, made at start for it). That makes
       28 checks.
   - **Not built.**
     - A script can't call a model for a judgment step: scripts get
       the tools the model gets, and `model.complete` isn't one.
     - The approval doesn't show each call's tier.
     - Triggers other than time.
     - The shell's screen for routines.
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
- Item 5's judgment steps, tiers in its review, and triggers other
  than time.
- Choosing the starting model per kind of task from evals (SK-2 scores
  per model) instead of always the cheapest.
- A catalog setting for an agent's starting model. Single-call agents
  (the IT agent) can already name their model in the catalog.
