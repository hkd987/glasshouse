# 0015. Taking over an ended actions agent continues the task in the user's harness

Status: **Accepted**. PRD AW-20, AW-22, HX-1, HX-3.

## Context
Take over gives the user the agent's display and input (AW-20) and is
always available (AW-22). An actions agent has no display, and once it has
failed or been stopped there is nothing left to pause. The Calm mockup
still shows Take over on a failed tile. The user decided it should open
our harness or Claude Code for them, with the task.

Claude Code is meant to run inside an agent jail like any other agent
(HX-2), with egress only to its provider (HX-7). Neither the Node runtime
in jails nor per-harness egress exists yet.

## Decision
- **Attended agents.** A catalog entry with `attended = true` has no
  program: glassd creates no sandbox and starts nothing, only the task
  socket, at `/var/run/glasshouse/attended/<task>.sock`, owned by the
  task's user, mode 0600, in a root-owned directory. The user's own client
  connects with `glass-mcp <socket>`. Every call goes through the same
  pipeline (tiers, labels, snapshots, audit, undo), and the agent API still
  checks the caller's uid. Claude Code ships as the `claude-code` entry,
  with its own policy (`files.*`, `mail.*`, internal data at most, HX-7).
- **`task.continue`** on an ended task (the user's own) starts a new task
  for the configured harness (`harness = "claude-code"` in `glassd.toml`)
  with the same request and scope. The harness's instructions say which
  agent stopped and why, the steps it finished and what it changed, so it
  carries on rather than starting over. The new task records what it
  continues; its actions are attributed to the harness, for the user.
- **Hand back closes it** as Done with a receipt; Stop and Pause work as on
  any task; take over is refused (the user already drives it).
  `glassctl take-over` on an ended task does this and prints a
  session-only `claude --mcp-config` command, and the `glass-mcp` line for
  other MCP clients (HX-6).

## Consequences
- What Claude Code does through its own shell and file tools is outside
  Glasshouse until it runs in a jail (HX-2). It runs as the user in their
  terminal, so it can reach only what the user can. Only brokered actions
  are tiered, audited and undoable. The tile, approvals, timeline and undo
  cover what goes through the socket (HX-3).
- Our own harness runner (ADR 0010) becomes another attended or jailed
  agent behind the same `harness` setting.
- Leftover sockets from a crash are removed at startup. Jails can't see
  `/var/run/glasshouse` (the probe's control-socket check).
