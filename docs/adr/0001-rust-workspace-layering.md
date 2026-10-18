# 0001. One Rust workspace, layered lib → svc → bin

## Context
The PRD expects a founding team of about 15 engineers and memory-safe
languages for new daemons ("Rust for brokers and the gateway is the
default"). The codebase will be large and long-lived.

## Decision
- Rust, one Cargo workspace, toolchain pinned in `rust-toolchain.toml`.
- Crates live in `crates/lib`, `crates/svc`, `crates/bin` and `tests/`, with
  dependencies only flowing lib → svc → bin (`tools/check-layers.py`).
- Versions, lints, license and edition are set once in the workspace.

## Consequences
- Wire contracts (`glass-api`, `glass-rpc`) sit in lib, so clients never
  link service internals, and the shell can be written against the same API
  as `glassctl`.
- The UI toolkit language is still open (PRD open question); if it isn't
  Rust it gets its own top-level directory and talks to `glass-api`.
