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
   countable (AW-4). Treat `is_error` tool results as information: a declined
   or blocked step is skipped, never retried in a loop. Exit 0 when done,
   non-zero with a one-line reason on stderr when it can't continue (that
   line becomes the Failed tile's reason).
2. **Packaging.** Add the binary to `os/freebsd/dist.manifest` under
   `usr/local/libexec/glasshouse/` (the directory mounted read-only into every
   jail) and to `SHIPPED` in the `Makefile`.
3. **Catalog.** An `[[agent]]` entry in
   `os/freebsd/overlay/usr/local/etc/glasshouse/glassd.toml`: `id`, `name`,
   `version`, `description`, `mode`, `program`, `plan` (3–7 plain steps for
   the handoff card), `model` if it uses one.
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

## External harnesses (Claude Code, Codex)

They connect through `glass-mcp` (stdio ↔ task socket):
`claude mcp add glasshouse -- glass-mcp`. They need their runtime (Node) in
the jail and egress to their provider only (HX-2, HX-7), which isn't built
yet; don't widen the jail's network to get there.
