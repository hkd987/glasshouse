---
name: containment-review
description: Checklist for any change that touches what an agent can reach or what the OS records - the sandbox/jail config, the broker or policy evaluation, actions, the task socket, undo or the audit log. Use before committing such a change and when reviewing one.
---

# Containment review

Glasshouse's promise is that agents are contained, visible, attributable and
undoable. A change that weakens any of those is a P0 bug (PA-3), however
small it looks.

## Ask of every change

- **Reach.** Can an agent now reach a file, socket, device, process or
  network destination it couldn't before? Jail params live in
  `crates/lib/platform/src/sandbox/jail.rs`; mounts come from
  `RuntimeConfig::agent_dirs` and `JailConfig::base_mounts`.
- **Data leaving the machine.** Does anything an agent produced or read
  leave except through the model gateway's routing and redaction (labels
  decide where, ADR 0018)? Does glassd reach a new network destination?
  Outbound connections belong in `glass-gateway-providers`, to endpoints
  policy names. Do credentials stay out of logs, audit records and tool
  results?
- **Root acting for an agent.** Does broker-side code touch paths an agent
  influences? It must go through `TaskContext::resolve*` and `ConfinedDir`,
  never `std::fs` on a joined path.
- **Decisions.** Does every new side effect pass `decide()`, label checks and
  (if reversible) a snapshot first? Is an irreversible action below Ask?
  (`Policy::lint` warns.)
- **Identity.** Is the caller authorized by kernel uid (control socket) or by
  the task socket it arrived on, never by anything in the request?
- **Record.** Is the action audited with its decision and outcome, including
  refusals? Does an audit failure still fail closed?
- **Lifetime.** Does anything granted outlive the task (PA-2)? Teardown in
  `supervise.rs` must still remove it.
- **Undo.** Does `glassctl undo` still restore it, and list what it can't?

## Prove it

1. `make test lint`.
2. `make e2e-vm`: the containment probe must pass every check (28/28) and
   "nothing left behind" must pass.
3. New surface → new probe check in `crates/bin/glass-probe` (it must FAIL on
   the insecure host backend and PASS in a jail).
4. A design change → an ADR; a requirement's status change →
   `docs/prd-traceability.md`.
5. Show it with the `manual-test` skill.
