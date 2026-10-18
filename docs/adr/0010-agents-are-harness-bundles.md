# 0010. Agents are versioned harness bundles; hard limits stay in the OS

Status: **Proposed**. Extends ADR 0008 (signed manifests). PRD AE-1,
AE-10 … AE-12, AE-20 … AE-25, AE-30.

## Context
The PRD now defines what a first-party agent is (AE-1): a versioned bundle
of eight parts (system prompt, soft guardrails, memory, tool guidance,
subagents, skills, scripts, plugins) plus its manifest of declared actions
and permissions. Hard limits (tiers, the Never list, data labels, egress)
live in the OS brokers, outside the bundle, where no agent and no
improvement loop can change them. Agents ship only if they clear their
workflow's evaluation suite (AE-10, AE-11), get better through a
Harness²-style loop that edits the bundle, never model weights (AE-20 …
AE-25), and are re-run against their suites before a model switch (AE-30).

Today `glass-demo-agent` is a scripted Rust program: no model, no bundle.

## Decision (proposed)
- **One agent runner, many bundles.** First-party agents are bundles run by
  a single Glasshouse agent runner (a `crates/bin` program speaking MCP on
  the task socket, as every agent does). The runner loads the bundle, calls
  models only through the model gateway (DP-1) and calls actions only
  through the task socket. A new agent is a new bundle, not a new binary.
- **Bundle layout**, in the repo under `agents/<id>/` and signed as one
  package (ADR 0008):

  ```text
  manifest.toml     id, version, mode, declared actions and labels (ADR 0008),
                    models it is evaluated on (AE-30)
  prompt.md         system prompt
  guardrails.md     soft guardrails (advice to the model; not enforcement)
  tools/            guidance per action
  skills/  subagents/  scripts/  plugins/
  memory/           seed memory only; learned lessons live per org (AE-24)
  evals/            the workflow's suite: tasks, fixtures, graders (AE-10)
  ```

- **Hard limits stay where they are.** Tiers, Never rules, labels and scope
  are enforced by `glassd` (`glass-policy`, `glass-broker`) on every call,
  whatever the bundle says. A bundle can make an agent better at its job;
  it can't widen what it may do. That's the property AE-20's loop relies on.
- **Evaluation runs inside our containment.** A suite run is a task on a
  disposable ZFS clone of a fixture machine or workspace, with every
  action that would leave the machine (`SideEffect::Irreversible`, such as
  `mail.send`) answered by a stub provider (AE-21). Harness²'s own code runs
  its executors with network and sandboxing off; we adopt the method only.
- **Metrics come from the audit log (AE-12).** Take-overs, undos, declined
  approvals and finished tasks are already audit events per agent and
  version; user edits to agent output need post-task change tracking.
- **Release bar (AE-11)** is a CI gate on the bundle: suite accuracy at or
  above the workflow's target (95% for Pilot 1's invoice reconciliation, decided 2026-10-18), zero policy violations, no
  tracked metric regressing beyond tolerance.

## Consequences
- `CatalogEntry` gains the bundle path and its version; `model` comes from
  the manifest, as the handoff card already shows it.
- The `tests/e2e` harness and the VM's ZFS snapshots are the start of the
  eval runner; the stub provider is a registry mode, not a fork of each
  provider.
- Lessons (AE-24) carry data labels and live in the customer's boundary,
  so they're stored per org on the device or the org's account, never in
  the bundle we ship.
- Open: the Pilot 1 workflows (invoice reconciliation and vendor email, per
  AE-2) and each one's accuracy bar are still to be confirmed with design
  partners.
