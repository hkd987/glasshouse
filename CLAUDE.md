# Working on Glasshouse

Proprietary FreeBSD-based desktop OS where AI agents run contained, visible,
audited and undoable. Read `docs/architecture.md` and `docs/repo-layout.md`
before structural changes; check `docs/adr/` before revisiting a decision.

## Commands

- `make test lint` before every commit (lint = fmt check, clippy `-D warnings`
  for host and FreeBSD, `tools/check-layers.py`).
- `make e2e-vm` after any change to containment, the broker, actions, undo or
  audit. Needs the VM: `make vm-up vm-wait` (see `docs/dev/testing.md`).
- `make dev-host` runs glassd locally with the insecure host sandbox.

## Rules

- Layering: `crates/lib` → `crates/svc` → `crates/bin`; binaries never
  depend on each other. Every dependency goes through
  `[workspace.dependencies]`.
- No new `unsafe` outside `glass-platform`'s FFI modules; prefer base-system
  commands (ADR 0007).
- Policy refusals are tool results agents can read, not protocol errors.
- Every change to what an agent can do needs a test at the right level, and a
  probe check if it adds a sandbox surface.
- Reference PRD ids (`AU-21`) in code comments and test names, and update
  `docs/prd-traceability.md` when a requirement's status changes.
- Third-party code must be permissively licensed (`deny.toml`).
