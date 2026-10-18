# Repository layout and conventions

This codebase will grow to hundreds of crates and several languages. These
rules keep it navigable. `make lint` (what changed) and `make lint-all`
(everything) enforce the mechanical ones.

## Where things go

```text
crates/lib/<name>    reusable libraries: types, protocols, platform abstraction,
                     the action contract (glass-actions)
crates/apps/<name>   one crate per app's actions (glass-app-files, -mail, ...)
crates/svc/<name>    service implementations hosted by daemons
crates/bin/<name>    executables: daemons, CLIs, agents, tools
tests/<suite>        end-to-end suites that drive a running system
os/freebsd/          everything that becomes the OS image: overlay, image and VM tooling
shell/               the desktop shell UI (ADR 0009): its own Cargo workspace, so the
                     core never compiles the UI toolkit; a control-API client (shell/README.md)
tools/               developer tooling (checks, dev runners, screenshots, legal notices)
docs/                architecture, decisions (adr/), developer guides (dev/)
```

Future top-level areas, when they start: the compositor (M2, next to the
shell in `shell/`), `apps/` (first-party TypeScript web apps; their OS actions stay
in `crates/apps`), `agents/` (agent
harness bundles, ADR 0010), `console/` (admin console), `gateway/` if the
model gateway outgrows its crates (`crates/svc/gateway`,
`crates/svc/gateway-providers`, ADR 0018).

## Layering

| Layer | May depend on (inside the workspace) |
|---|---|
| `crates/lib` | `crates/lib` |
| `crates/apps` | `crates/lib` (never another app) |
| `crates/svc` | `crates/lib`, `crates/apps`, `crates/svc` |
| `crates/bin` | `crates/lib`, `crates/apps`, `crates/svc` (never another binary) |
| `tests/*` | `crates/lib`, `crates/apps`, `crates/svc` |

Consequences worth knowing:

- Wire contracts live in lib crates (`glass-api`, `glass-rpc`), so clients
  like `glassctl` and the shell never link service internals.
- Anything OS-specific lives in `glass-platform` behind a small interface,
  with a development backend, so services stay testable on any host.
- Apps never see each other or the runtime: an app crate builds against
  the `glass-actions` contract only, so a team can own one app, and an edit
  there rebuilds that crate and the binaries, nothing else (ADR 0013).
  `glass-first-party` (svc) is the one list of apps the OS ships;
  `glassd` and `glassctl policy check` both build their registry from it.

## Crate conventions

- Name crates `glass-<thing>`; binaries keep their command name (`glassd`).
- Every manifest inherits `version`, `edition`, `license-file`, `publish` and
  `[lints]` from the workspace, and takes every dependency through
  `[workspace.dependencies]`. Versions are pinned in one place.
- Library crates expose a small surface from `lib.rs` with a module doc that
  says what the crate owns and what it doesn't.
- `unsafe` is denied workspace-wide. The only exceptions are the FFI modules
  in `glass-platform` (`users`, `xattr`, `framebuffer`, `input`), each block with a `SAFETY` note.
  Prefer a base-system command or a vetted crate over new `unsafe`.
- Tie behavior to PRD ids in comments and test names where it implements one
  (`// AU-21`), so traceability survives refactors.

## Errors and logs

- Libraries return typed errors (`thiserror`). Binaries use `anyhow` at the
  edges.
- Policy refusals are results the agent can read, not protocol errors.
- `tracing` for daemon logs; `GLASS_LOG` sets the filter.

## Adding things

- **An app action**: implement `ActionProvider` in the app's crate under
  `crates/apps/` (or start one: copy `crates/apps/helpdesk`, the smallest),
  with a side-effect-free `preview`, and list a new app in
  `glass-first-party`. Give it a default tier and side effect; policy
  decides the rest. The `add-action` skill has the steps.
- **An agent**: a program that speaks MCP on `GLASS_AGENT_SOCKET`. Put it in
  `crates/bin`, add it to `os/freebsd/dist.manifest`, the catalog in the
  shipped `glassd.toml`, and an `[[agent]]` entry in policy.
- **A decision**: write an ADR in `docs/adr/` when a choice is hard to
  reverse or would surprise a new engineer.
