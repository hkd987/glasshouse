---
name: add-agent
description: Add a new agent (a program that works for a user through the task's MCP socket) to Glasshouse - the program, its catalog entry, its policy and its packaging into the image. Use when building or onboarding an agent, including wrapping an external harness.
---

# Adding an agent

An agent is any program that speaks MCP on the socket in
`GLASS_AGENT_SOCKET`. It runs in a jail with no network and no user data; it
acts only through the tools `tools/list` returns. See ADR 0004 and, for where
this is going (signed manifests), ADR 0008.

## Steps

1. **Program.** For a Rust agent, a crate in `crates/bin/` using
   `glass-agent-sdk` (`Agent::from_env()`, `call`, `progress`). Call
   `report_progress` at each step with a plain caption and `done/total` when
   countable (AW-4). Its optional `title` renames the task once, if the name
   made from the prompt doesn't fit ("Reconcile October invoices"). Treat `is_error` tool results as information: a declined
   or blocked step is skipped, never retried in a loop. Exit 0 when done,
   non-zero with a one-line reason on stderr when it can't continue (that
   line becomes the Failed tile's reason).
2. **Packaging.** Add the binary to `os/freebsd/dist.manifest` under
   `usr/local/libexec/glasshouse/` (the directory mounted read-only into every
   jail) and to `SHIPPED` in the `Makefile`.
3. **Catalog.** An `[[agent]]` entry in
   `os/freebsd/overlay/usr/local/etc/glasshouse/glassd.toml`: `id`, `name`,
   `version`, `description`, `mode`, `program`, `plan` (3–7 plain steps for
   the handoff card), and `model`, the policy model id it prefers, if it
   uses one (see below).
   **What users may tailor** (TL-1, ADR 0014): `[[agent.customize]]` blocks
   (`key`, `title`, `kind` = text | number | flag | choice | instructions,
   `default`). The agent reads them with `Agent::setting(key)` (or the
   `GLASS_AGENT_SETTINGS` JSON); model-backed agents also get them in their
   instructions. Keep keys stable across versions: renaming one pauses
   every layer that set it (TL-13). Bump `version` when behavior changes.
4. **Policy.** An `[[agent]]` block with `allowed_actions` (as narrow as the
   job allows) in `crates/lib/policy/default-policy.toml` (only if every new
   org should have it), and add it to a group's `agents`.
5. **Dev host.** Add it to `tools/dev/host-run.sh` so `make dev-host` runs it.
6. **Tests.** An e2e scenario in `tests/e2e/src/scenarios.rs` that runs it
   start to finish as `demo`; check its tile states and audit trail.

## Model-backed agents

An agent never holds an API key or reaches a model itself: its jail has no
network. It calls `model_complete` on the task socket (ADR 0018), like any
tool:

```json
{"messages": [{"role": "user", "content": "Summarize these invoices: …"}],
 "max_tokens": 800, "system": "Reply in three bullet points."}
```

- **Leave `model` out.** Policy picks an approved model for the most
  sensitive data the task has read. Naming one outside the allowlist is
  refused.
- **Expect placeholders.** Secrets and configured PII leave the machine as
  `[redacted:aws-access-key#1]`, `[redacted:email#1]`; PII comes back in the
  reply, secrets don't. Never try to work around it.
- **Refusals are information**, like any `is_error` result: "Not sent: no
  model is approved for confidential data" means continue without a model
  for that step. Don't retry it in a loop.
- **Policy**: add `model.complete` to the agent's `allowed_actions`; the
  org's `[models]` section must approve a model for the data it handles
  (routes per label), and a group or the agent can narrow the list with
  `models = [...]`. The catalog's `model` is a preference the handoff card
  shows when policy allows it.
- **Tests**: `crates/svc/runtime/tests/models.rs` has a scripted provider
  and a model-backed `writer` agent to copy; the e2e `models` scenario runs
  under a test policy (`glass-e2e models-policy`).

## External harnesses (Claude Code, Codex)

They connect through `glass-mcp` (stdio ↔ task socket). Today they run as
**attended** agents (ADR 0015): a catalog entry with `attended = true` and no
program; glassd only binds the task socket in `/var/run/glasshouse/attended/`
and the user connects their own session (`glassctl take-over` on an ended
task, or `glassctl run claude-code …`, prints the command). Running them
inside a jail needs their runtime (Node) there and egress to their provider
only (HX-2, HX-7), which isn't built yet; don't widen the jail's network to
get there.
