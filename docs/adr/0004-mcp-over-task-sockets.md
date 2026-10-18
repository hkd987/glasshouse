# 0004. Agents speak MCP over a per-task Unix socket

## Context
NV-2 requires the action registry to speak MCP so existing tools work; HX-1
and HX-6 require Claude Code, Codex and other MCP clients to drive the OS.
Agents must not be able to forge which task they act for.

## Decision
- Each task gets its own socket, bound by glassd inside that task's jail and
  owned by the task's user (0600). The socket *is* the task identity: no
  tokens to steal or replay.
- The socket speaks MCP (initialize, tools/list, tools/call). App actions are
  tools; `report_progress` updates the tile.
- Every socket in the system uses one framing: JSON-RPC 2.0, one message per
  line (MCP's stdio framing). `glass-mcp` bridges stdio to the socket byte for
  byte, so `claude mcp add glasshouse -- glass-mcp` works inside a jail.
- Policy refusals come back as tool results with `isError`, so models read
  them and adapt; only malformed calls are protocol errors.

## Consequences
- No proprietary agent SDK; `glass-agent-sdk` is optional sugar.
- Tool names use `_` (`files_write`) because model APIs reject dots; action
  names keep dots (`files.write`) for policy patterns.
