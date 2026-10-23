# 0046. The OS writes the code for repeated work, from what a task did

Status: **Accepted, built** (2026-10-22). The owner: "Building the
deterministic writing of code for repeated actions needs to happen." PRD
NV-21 ("a user or admin records a flow once; the OS turns it into replay
code, and the agent takes over only when replay fails"), DA-4 (routines).
Builds on ADR 0044 (the script runner, routines) and ADR 0041 (the task
socket's tools).

## Context
- In ADR 0044 a routine's script is written by the model. That costs a
  call, comes out differently each time it's asked for, and can be wrong.
  Haiku's was good, but the person was approving a model's guess at the
  flow, not the flow.
- The task socket already sees every call an agent makes, with its input
  and what it returned. For actions (moving, drafting, filing) that is
  the flow, recorded as it happened. What the model decided shows in the
  calls: which items it touched.
- Agents batch: `mail.move` takes many ids in one call.

## Decision
1. **Every task's calls are recorded.** The runtime keeps each completed
   call in order, with its input and result. Model calls, an app's calls
   and saving routines are left out.
   - Kept in memory, at most 400 calls and 2 MB. Past that the task can't
     be replayed.
   - When the task ends, it is compiled once and the calls are let go.
2. **A compiler turns them into code** (`glass-replay`): deterministic,
   with no model, the same calls always the same script. The rules:
   - **Constants.** A value never in an earlier result is a constant.
   - **References.** A value taken from an earlier result is a
     reference:
     - an item of a list: `item.id`, `"Invoices/" + item.name`;
     - otherwise, an id-like field: `r3.data.id`.
     A value two items share (every message's folder) names neither, so
     it stays a constant.
   - **Lists.** A list in an input made of a listing's items, the same
     way, is that listing mapped. The call is skipped when it comes out
     empty.
   - **Loops.** Blocks of calls made once per item, up to 4 calls a block,
     are a loop over the list.
   - **Rules for subsets.** When only some items were touched, the
     simplest rule that picks exactly those is tried, in a fixed order:
     - a field equal to one value;
     - a field not equal to one value (two items left out sharing it);
     - a field containing one word (two or more examples).
     Ids never make a rule. One example makes one only on a field other
     items share values of. With no rule, the same items again.
   - **Checks and answer.** Every call is checked: one that fails stops
     the script, and the agent takes over (NV-21). The answer is what
     the calls that changed something said.
   - **Refused.** A task that only read: its answer was the model's.
   - **Safe from what it reads.** Anything from mail or files enters the
     code only as a JSON string or a quoted field name.
3. **Repeats are noticed.** When a task finishes, its code is kept with
   its request, per person and root only, for 30 days (at most 50).
   - The same request is the same agent, the same words (case, spacing
     and a final full stop aside) and the same folders.
   - When it was done before with the same steps, the task says so and
     how to save it.
   - The same steps compares shapes, not code. Calls that change
     something must match exactly, with their loops and rules. Reads match
     by tool and their non-numeric inputs: Haiku listed the Inbox once
     without a limit and once with `limit: 500`.
   - A routine's own run isn't noticed.
4. **The person saves it, or the agent does.**
   - `glassctl routine draft <task>` shows the code and its steps.
   - `glassctl routine save <task> --name … [--schedule …]` keeps it. It's
     the person's own act, so no ask, and it is audited (`RoutineSaved`).
   - The agent, asked for something regularly, does it once, then saves
     with `routines.save {from_this_task: true}`. The runtime puts the
     compiled script in the call before its preview, so what the person
     approves is the code this computer wrote.
   - The model writes a script only for an answer that has to be worked
     out (ADR 0044 item 4).
5. **Runs are as in ADR 0044.** A task with the saved scope runs the
   script with no model, under the same tiers, audit and undo. If a call
   fails, the agent takes over on the cheapest model.

## Consequences
- **Live on Claude Haiku 5.5** (2026-10-22), "Move the newsletters from
  Vendor News in my Inbox to Archive":
  - Done once for $0.0006. The code: list the Inbox, keep the messages
    whose `from` is Vendor News, move their ids in one call.
  - Next week, the same request was noticed as the same steps.
  - Saved, it ran the week after with no model call, $0, archiving the
    new newsletters.
  - Asked "every Friday at 5pm", Haiku did that week's and saved with
    `from_this_task`. The approval showed the code this computer wrote.
- A rule inferred from one run can be a coincidence. The approval and
  `routine draft` show it in words ("whose from is …"), and a run that
  goes wrong is undoable like any task.
- Text the model wrote into an input (a draft's body) is a constant and
  the same every run. The steps show it.
- Compiling a long trace takes a while: 400 calls over a 500-message
  listing, about 0.3 s optimized, most of it trying rules. It happens at
  the task's end, with the task's lock let go (holding it showed in the
  VM as undo preview over its budget).
- Kept flows hold the inputs of people's calls: root only in
  `/var/db/glasshouse/flows`. The probe's new check makes 29.

## Not built yet
- Rules on numbers ("amount over 1,000") and dates.
- Loops nested in loops, and blocks longer than 4 calls.
- Offering the save from the desktop when a repeat is noticed: today the
  task's activity says it.
- Screen flows (NV-21's legacy apps): actions only.
