# 0008. Agents ship signed manifests that declare their privileges

Status: **Proposed** (needed before the developer program, DV-1 … DV-3)

## Context
Today an agent is a catalog entry in `glassd.toml` (how to run it) plus an
`[[agent]]` block in policy (what IT allows). The October PRD adds three
requirements that meet in the same place:

- DV-3: every listing declares its privileges (actions, data labels, network
  destinations, default tiers), and the OS enforces the declaration, so an
  item can never exceed it.
- PA-7: every agent is its own directory identity with a named human owner,
  reviewed quarterly and retired when unused.
- EP-2 and DV-1: only signed software runs; every agent, adapter and skill is
  signed by a verified publisher.

## Decision (proposed)
An agent is a signed package containing a manifest:

```toml
id = "invoice-agent"
version = "2.3.0"
publisher = "Example Corp"           # verified publisher, signing identity
mode = "screen"                       # or "actions"
program = "bin/invoice-agent"
[declares]
actions = ["ap.invoice.*", "files.read", "mail.draft", "mail.send"]
max_label = "internal"
egress = []
default_tiers = { "mail.send" = "ask" }
```

What a task may do is the intersection, never the union:

    effective = manifest declaration ∩ IT policy (org, group, agent) ∩ task scope

- The publisher's declaration is a ceiling no policy can raise.
- IT policy keeps the agent-level `[[agent]]` rules it has today, plus the
  directory identity and owner (PA-7).
- The runtime's decision path (`glass-policy` → `glass-broker::decide`)
  gains one more stricter-only layer; nothing else changes.
- Anything outside the declaration is blocked and counted as drift (EP-5):
  repeated drift pauses the task and flags it.
- The catalog (`glassd.toml [[agent]]` today) becomes the set of installed,
  signature-checked packages; veriexec (EP-2) pins the program hashes.

## Consequences
- A third-party agent can be reviewed from its manifest alone (DV-2, DV-4).
- Moves catalog metadata (`version`, `model`) out of `glassd.toml`.
- Needs a package format and signing chain (EP-11, EP-12) first.
