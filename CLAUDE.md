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
| `cleanup` | Under 8 GB free, after merging an agent's branch, a build dies with "No space left", a VM stops answering, or before ending a session |

## Commands

- `make test lint` before every commit. `make lint` checks what changed since
  main (rustfmt, clippy `-D warnings` for host and FreeBSD, layering, art,
  shellcheck); `make lint-all` checks everything, as CI does.
- `make tla` (about two minutes) after changing a workflow `specs/tla` models
  (its README lists them: boot, policy, the audit log, containment, tasks
  and approvals, undo, chat, docs, mail, meetings and more): TLC checks the
  model, and a change in behavior changes the model with it (ADRs 0051 to
  0054).
- `make freebsd vm-deploy` (~12 s) to try a change in the VM;
  `make e2e-vm` (~1 min) after any change to agent-facing behavior.
- `make dev-host` runs glassd locally with the insecure host sandbox.
- `make check` while editing; never `--release` in the loop
  (`docs/dev/build-performance.md`).
- The desktop shell is its own workspace (`shell/README.md`): `make shell-test
  shell-lint`; `make shell-preview` renders its screens to PNG,
  `make shell-vm` puts the desktop on the VM's screen and `make greeter-vm`
  the sign-in screen before it (the user likes seeing them).
- The web apps are a TypeScript workspace (`apps/README.md`, ADR 0024):
  - `make apps-build` bundles them with esbuild into `apps/<app>/dist`.
  - `make apps-test`: types, unit tests, Playwright (headless Chromium) on dev-host glassd, launch and memory budgets.
  - `make apps-lint`: types, manifests, npm licenses; `make lint` runs it when `apps/` changes.
  - `make apps-dev` serves them at http://mail.localhost:7300 from `make dev-host`'s glassd.
- The admin console (`glass-console` + `apps/console`, ADR 0023; `apps/README.md`):
  - `make console-dev` serves it at http://127.0.0.1:7400 (`FIXTURE=500,50`); `GLASS_DEV_ENROLL=build/console/enrollment.json make dev-host` enrolls glassd.
  - `make console-test`: Rust tests, then Playwright on a 500-device fixture and an enrolled dev-host glassd (page and propagation budgets).
  - `make console-lint`: clippy, types, BR-1; `make lint` runs it when the console changes.
- Anything a person waits on has a latency budget in `glass-e2e perf`
  (`docs/dev/performance.md`).

## Saving time and tokens

- Run long, chatty commands through `tools/quiet` (`tools/quiet make test
  lint`, `tools/quiet make e2e-vm`): it prints the summary and, on failure,
  the failures; the whole output is in `build/logs/`. Read the log only
  when that isn't enough.
- Drive the VM's screen with `os/freebsd/scripts/desk.sh` (`signin`,
  `signout`, `click`, `key`, `keydown`/`keyup`, `point`, `app`, `shot`, `wait`,
  `log`, `fresh` for first-run setup): each step waits for the screen to have
  drawn, so there are no sleep loops to write.
- If you type the same multi-step sequence a second time, make it a
  script or a `desk.sh`/`vm.sh` command (and note it here or in the skill
  it belongs to) instead of a third time.
- Clean up as you go (`cleanup` skill, `tools/cleanup.sh`): disk is small,
  and a full disk pauses VMs and kills links.

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
