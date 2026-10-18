# Working on Glasshouse

Proprietary FreeBSD-based desktop OS where AI agents run contained, visible,
audited and undoable. Read `docs/architecture.md` and `docs/repo-layout.md`
before structural changes; check `docs/adr/` before revisiting a decision.

## Project skills: use them

The workflows this project repeats are written down as skills in
`.claude/skills/`. Load the matching one before starting; they hold the
details this file leaves out.

| Skill | Use it when |
|---|---|
| `test-env` | Bringing up, deploying to, resetting or debugging the FreeBSD dev VM |
| `manual-test` | **Any** hands-on testing or demo. The user expects screenshots (`tools/screenshot/termshot.py`) of what you ran, sent as you go |
| `containment-review` | Before committing anything that touches the sandbox, broker, policy, actions, task socket, undo or audit |
| `add-action` | Giving agents a new capability (an app action) |
| `add-agent` | Building or onboarding an agent |
| `prd-sync` | The user says the PRD or mockups changed, or before planning against a requirement |

## Commands

- `make test lint` before every commit (lint = fmt check, clippy `-D warnings`
  for host and FreeBSD, `tools/check-layers.py`).
- `make freebsd vm-deploy` (~12 s) to try a change in the VM;
  `make e2e-vm` (~2 min) after any change to agent-facing behavior.
- `make dev-host` runs glassd locally with the insecure host sandbox.
- `make check` while editing; never `--release` in the loop
  (`docs/dev/build-performance.md`).
- Anything a person waits on has a latency budget in `glass-e2e perf`
  (`docs/dev/performance.md`).

## Sources of truth

- PRD: Claude Docs doc `30714c94-4e70-4873-837f-731979413c58`; snapshot in
  `docs/prd/`, status in `docs/prd-traceability.md`.
- Mockups: <https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC>; renders and
  the element → API map in `docs/design/`.

## Rules

- Layering: `crates/lib` → `crates/apps` → `crates/svc` → `crates/bin`
  (ADR 0013); apps never depend on each other, nor binaries on each other. Every dependency goes through
  `[workspace.dependencies]`, permissively licensed (`deny.toml`).
- No new `unsafe` outside `glass-platform`'s FFI modules; prefer base-system
  commands (ADR 0007).
- One target: `x86_64-unknown-freebsd` (V1 is x86-only).
- Policy refusals are tool results agents can read, not protocol errors.
- Users never see "FreeBSD" (BR-1): not in tiles, activity lines, errors,
  CLI output or anything the image shows. Admin logs and audit records may
  name the base system. `tests/e2e/check-branding.sh` checks the image.
- Every change to what an agent can do needs a test at the right level, and
  a probe check if it adds a sandbox surface.
- Reference PRD ids (`AU-21`) in code comments and test names, and update
  `docs/prd-traceability.md` when a requirement's status changes.
