# 0014. Tailoring is a stack of personal layers the OS checks and applies

Status: **Accepted** for personal layers on catalog agents (TL-1, TL-2,
TL-10, TL-13). Team layers, copies and evaluation gates are **Proposed**
below. PRD "Tailoring catalog items".

## Context
Users adapt catalog agents by asking ("also flag auto-renewal clauses").
The PRD wants those changes saved as thin layers on the published version
rather than copies, so publisher updates keep arriving (TL-2), and wants
the OS, not the agent, to guarantee that a layer can only narrow what the
agent may do (TL-10), passes the agent's release bar before it turns on
(TL-11), and is re-checked when the agent changes (TL-13).

## Decision
- **Customization points are declared by the publisher** in the catalog
  entry (`[[agent.customize]]`: key, title, kind, default), standing in for
  the signed manifest of ADR 0008. Kinds are text, number, flag, choice and
  instructions (the user's own words). Anything not declared is locked.
- **A layer is data, not code.** It holds values for declared points and
  extra limits (`restrict`, the same mechanism as the handoff card's
  limits). It can't name data, actions or programs, so it has no way to add
  a permission; limits are applied as a maximum over policy, so they can
  only narrow.
- **glassd owns layers** (`glass-tailoring`, stored under its state
  directory, root-only) and changes them only through `layer.set` and
  `layer.revert`, after checks: values match the points (TL-1), limits only
  tighten the user's effective policy (TL-10), and the release bar (TL-11),
  which reports "not available" until evaluation suites exist (AE-10).
- **Every change is a version; undo is switching versions** (TL-2). History
  is never edited. Saves, switches and re-checks are audit events
  (`LayerChanged`), and a task's start record names the layer version and
  the settings it ran with.
- **The agent gets settings, not a different agent**: as
  `GLASS_AGENT_SETTINGS` (JSON) and in its MCP instructions, alongside the
  user's words for an instructions point.
- **A new agent version re-checks every layer** at startup and before use
  (TL-13). A layer that no longer fits is paused with the reason, the
  published agent runs instead, and the handoff card and `glassctl tailor`
  say so.

## Proposed next
- Team layers (TL-3) stack between the published version and personal
  layers, shared through the same review as generated apps (GA-8).
- "Make my own version" (TL-4) copies the agent into a generated app and
  leaves the layer model.
- The release bar (TL-11) and re-tests (TL-13) run the agent's evaluation
  suite with the layer applied on a disposable snapshot (AE-21), once
  suites exist.
- IT controls (TL-12, TL-14): per-agent "display only" or "re-validate
  behavior changes", and a layer inventory, in policy and the console.

## Consequences
- Agents opt in by declaring points and reading settings; an agent that
  declares none can still be narrowed by a layer's limits.
- A setting renamed by the publisher pauses the layers that used it, which
  is safe but needs the owner to act; publishers should keep keys stable.
- Users are told about a paused layer when they next look (card,
  `glassctl tailor`); pushing a notification waits for the shell.
