# 0013. One crate per app's actions, built against a lib-layer contract

Extends the layering of [0001](0001-rust-workspace-layering.md).

## Context
The PRD makes Glasshouse a general OS: files, mail, calendar, CRM, IT
self-service, help desk and every app after them declare actions to the OS
(NV-1, NV-3), and users will soon ask for apps to be built (GA-1 … GA-11).
All first-party actions lived in one svc crate, `glass-actions`, next to the
registry and the `TaskContext` they're written against. Every team would
edit the same crate, each edit rebuilt all apps, and `glassctl policy check`
kept its own copy of the app list, which had already drifted from glassd's
(it missed the system and help desk apps).

## Decision
- **`glass-actions` moves to the lib layer** and holds only the contract:
  `ActionProvider`, `ActionDef`, `Preview`, `Change`, `TaskContext`, the
  registry, `parse_input`, `attribute` and a `testing::context` fixture.
- **Each app is its own crate** in `crates/apps/<app>` (`glass-app-files`,
  `glass-app-mail`, `glass-app-system`, `glass-app-helpdesk`). An app crate
  depends on lib crates only, never on another app or on the runtime.
- **`glass-first-party`** (svc) is the one list of apps the OS ships. It
  builds the registry from a `Setup` (what an app needs from the daemon: the
  restartable services, the help desk outbox, the system interface), and
  `glassd` and `glassctl policy check` both call it.
- The layers are now lib → apps → svc → bin; `tools/check-layers.py`
  enforces it.

## Consequences
- A team owns an app crate. Editing one rebuilds that crate and the binaries
  that link it (about 1.5 s for a debug build of the workspace), not the
  other apps or the runtime.
- An app can't reach into the runtime or another app's internals; anything
  shared goes into the contract, where every app sees the change.
- The runtime's pipeline tests take the app crates as dev-dependencies, so
  they test real providers without the runtime depending on any app.
- Third-party apps (marketplace, GA) will declare actions over a protocol
  rather than link in, but they'll use the same `ActionDef` and `Preview`
  shapes.
