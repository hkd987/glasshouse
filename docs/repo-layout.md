# Repository layout and conventions

This codebase will grow to hundreds of crates and several languages. These
rules keep it navigable. `make lint` enforces the mechanical ones.

## Where things go

```text
crates/lib/<name>    reusable libraries: types, protocols, platform abstraction
crates/svc/<name>    service implementations hosted by daemons
crates/bin/<name>    executables: daemons, CLIs, agents, tools
tests/<suite>        end-to-end suites that drive a running system
os/freebsd/          everything that becomes the OS image: overlay, image and VM tooling
tools/               developer tooling (checks, dev runners)
docs/                architecture, decisions (adr/), developer guides (dev/)
```

Future top-level areas, when they start: `shell/` (compositor and UI toolkit),
`console/` (admin console), `gateway/` if the model gateway outgrows a crate.

## Layering

| Layer | May depend on (inside the workspace) |
|---|---|
| `crates/lib` | `crates/lib` |
| `crates/svc` | `crates/lib`, `crates/svc` |
| `crates/bin` | `crates/lib`, `crates/svc` (never another binary) |
| `tests/*` | `crates/lib`, `crates/svc` |

Two consequences worth knowing:

- Wire contracts live in lib crates (`glass-api`, `glass-rpc`), so clients
  like `glassctl` and the shell never link service internals.
- Anything OS-specific lives in `glass-platform` behind a small interface,
  with a development backend, so services stay testable on any host.

## Crate conventions

- Name crates `glass-<thing>`; binaries keep their command name (`glassd`).
- Every manifest inherits `version`, `edition`, `license-file`, `publish` and
  `[lints]` from the workspace, and takes every dependency through
  `[workspace.dependencies]`. Versions are pinned in one place.
- Library crates expose a small surface from `lib.rs` with a module doc that
  says what the crate owns and what it doesn't.
- `unsafe` is denied workspace-wide. The only exceptions are the FFI modules
  in `glass-platform` (`users`, `xattr`), each block with a `SAFETY` note.
  Prefer a base-system command or a vetted crate over new `unsafe`.
- Tie behavior to PRD ids in comments and test names where it implements one
  (`// AU-21`), so traceability survives refactors.

## Errors and logs

- Libraries return typed errors (`thiserror`). Binaries use `anyhow` at the
  edges.
- Policy refusals are results the agent can read, not protocol errors.
- `tracing` for daemon logs; `GLASS_LOG` sets the filter.

## Adding things

- **An app action**: implement `ActionProvider` in `glass-actions` (or a new
  svc crate for a big app), with a side-effect-free `preview`, then register
  it in `glassd`. Give it a default tier and side effect; policy decides the
  rest. Add the tier to the default policy if it needs one.
- **An agent**: a program that speaks MCP on `GLASS_AGENT_SOCKET`. Put it in
  `crates/bin`, add it to `os/freebsd/dist.manifest`, the catalog in the
  shipped `glassd.toml`, and an `[[agent]]` entry in policy.
- **A decision**: write an ADR in `docs/adr/` when a choice is hard to
  reverse or would surprise a new engineer.
