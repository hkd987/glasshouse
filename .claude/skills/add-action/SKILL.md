---
name: add-action
description: Add or change an app action (a typed operation an app declares to the OS, NV-1), such as calendar.hold or crm.invoice.create, end to end - provider, preview, tiers, policy, tests. Use when giving agents a new capability.
---

# Adding an app action

Agents can only affect the world through actions. Every action goes through
the pipeline in `crates/svc/runtime/src/execute.rs`; the provider never
decides permissions.

## Steps

1. **Pick the name and side effect.** `app.verb` (`files.write`,
   `calendar.hold`). Side effect: `None` (read only), `LocalReversible`
   (snapshots can undo it), `Irreversible` (leaves the machine: send, post,
   pay). Irreversible actions default to `Tier::Ask`.
2. **Provider.** In the app's crate, `crates/apps/<app>/` (`glass-app-<app>`),
   implement `ActionProvider`. A new app gets a new crate: copy
   `crates/apps/helpdesk` (the smallest), add it to
   `[workspace.dependencies]`, and depend only on lib crates
   (`glass-actions`, `glass-api`, `glass-platform`, `glass-types`); apps
   never depend on each other or on the runtime (`tools/check-layers.py`).
   - `actions()`: `ActionDef` with a JSON Schema (`additionalProperties:
     false`) and a plain-language title ("Send emails"; it appears on
     handoff cards).
   - `preview()`: **no side effects**. Fill `summary` (verb + target),
     `targets`, `items` (one line per affected thing, for batched approvals,
     DA-12), `reads`/`writes` (host paths, for label checks) and
     `snapshot_roots` (existing dirs to snapshot before a reversible change).
   - `invoke()`: do it. Resolve paths only with `TaskContext::resolve*` and
     touch files only through `ConfinedDir`. Hand ownership to `ctx.owner`,
     call `attribute()` on changed files (AU-10), return `Change`s with
     `reversible` set honestly.
   - Parse input with `glass_actions::parse_input` into
     `#[serde(deny_unknown_fields)]` structs.
3. **Register** a new provider in `crates/svc/first-party/src/lib.rs`
   (`registry()`), the one list `glassd` and `glassctl policy check` share.
   Anything it needs from the daemon (paths, policy lists, a system
   interface to fake in tests) goes on `Setup`.
4. **Policy.** Add a tier to `crates/lib/policy/default-policy.toml` if the
   app default isn't right for a new org, and a `[names]` entry for any
   pattern users will see.
5. **Tests.**
   - Provider unit tests in the app crate, built on
     `glass_actions::testing::context`: preview matches invoke; read-only
     grants refuse; bad input rejected.
   - A pipeline test (`crates/svc/runtime/tests/pipeline.rs`) for its tier
     behavior, especially Ask and undo.
   - If it's reversible, prove `glassctl undo` restores it.
6. **Docs.** Update `docs/prd-traceability.md` (NV-3 coverage) and the
   design map in `docs/design/README.md` if the shell shows it.

## Checks before you're done

`make test lint`, then `make e2e-vm` (its `perf` scenario holds actions to
their latency budgets, `docs/dev/performance.md`); use the `manual-test`
skill to show the action in a tile and in `glassctl audit`.
