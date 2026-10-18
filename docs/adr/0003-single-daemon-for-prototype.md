# 0003. One privileged daemon hosting services as libraries, for now

## Context
The PRD lists separate components: permission broker, secrets broker, model
gateway, snapshot and audit services, agent runtime. Separate processes give
privilege separation but cost IPC plumbing, startup ordering and operations
work before anything is proven.

## Decision
For the prototype, `glassd` hosts the runtime, broker, action providers and
audit writer in one process. Each is a separate crate with a narrow Rust
interface, so moving one behind a socket later changes wiring, not logic.

## Consequences
- The containment boundary that matters for the first gate (agent jail ↔
  host) is fully in place; the boundary between our own components is not.
- Split before handling real credentials: the secrets broker (MP-4) and the
  model gateway (DP-1) should be their own processes with their own users,
  and the audit log should move to a writer that `glassd` can append to but
  not rewrite (plus OpenBSM records, AU-2).
