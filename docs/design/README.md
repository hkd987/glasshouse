# Design: the agent window mockups

The PRD's mockups live in a Claude Design canvas, **"Agent Window — OS
Mockups"**: <https://claude.ai/artifact/Y5fH2vFk3fWUvWumZTRATC>. The PNGs here
are renders of its five artboards, for reading without the canvas. They go
stale when the canvas changes; re-render them with the `prd-sync` skill
(`.claude/skills/prd-sync`), which uses `tools/design/render-dc.py`.

The shell (compositor and GUI) is not built yet. Everything a tile, the
expanded view and the handoff card show already comes from the control API
(`glass-api`), so the shell can be built directly against it. The tables
below say which field drives each element, and what's still missing.

Design tokens from the canvas: Geist and Geist Mono; dark tiles `#15181D` on a
light desktop; accent `#3D6BFF` for working and you're-driving; attention
`#F08A24` for needs-you, the only state that pulses; 44 px touch targets.

## Desktop · agent tiles

![Desktop with agent tiles](mockups/desktop-tiles.png)

| Element | Source |
|---|---|
| One tile per running agent, needs-you on top (AW-1) | `task.list` → `TaskView`; sort by `state == NeedsYou` |
| Name and mode chip ("Actions", "Screen") | `agent_name`, `mode` |
| Elapsed ("4 min") | now − `started_at` |
| Feed: verb column + target (AW-3) | `feed[].verb`, `feed[].target` (`text` is the whole line) |
| Caption and "12 / 40" (AW-4) | `caption`, `progress` (agents call `report_progress`) |
| Needs-you question, Review drafts / Approve and send | `needs: ApprovalView` (`summary`, `items`) → `approval.answer` |
| "Allow sends for the rest of this task" | `ApprovalAnswer::Approve { scope: Task }` |
| Screen thumbnail with labeled agent cursor (AW-2) | **Not built**: needs screen-control agents |
| Taskbar "3 agents" pill, command bar (DA-1) | `task.list` counts; command bar → `task.plan` |
| Live updates | `events.subscribe` → `event` notifications (`TaskUpdated`, `TaskRemoved`) |

## Expanded · watch, take over, rewind

![Expanded view](mockups/expanded.png)

| Element | Source |
|---|---|
| Header: name, "Working · 12 of 40", "on behalf of you · started 2:02 PM" | `TaskDetail.view` |
| Pause / Stop / Take over (AW-11, AW-20) | `task.pause`, `task.stop`, `task.take_over` (`task.hand_back` returns) |
| Now bar (AW-12) | `TaskDetail.now` |
| Activity tab with snapshot rows (AW-13) | `activity[]`, `kind == Snapshot` for the ◇ rows |
| Changes · 11 | `changes[]` (`reversible: false` flags sent mail and the like) |
| Can't touch (AW-14) | `cant_touch[]` |
| Timeline markers, "Roll back to 2:11 PM" (AW-15) | `snapshots[]` → `task.undo { to }` (`dry_run` first to show what changes) |
| Live screen with agent cursor, LIVE badge | **Not built**: screen-control agents |
| "Stop asks once if unsaved work" (AW-11) | **Not built** |

## Tile states

![Tile states](mockups/tile-states.png)

| Artboard state | `TileState` |
|---|---|
| Working · on screen / Working · actions | `Working` (`mode` picks thumbnail or feed) |
| Needs you | `NeedsYou` with `needs` set |
| You're driving · "You have control" · Hand back | `YouAreDriving` → `task.hand_back` |
| Done: receipt, Review changes, Undo all | `Done` with `receipt`; Undo all → `task.undo` |
| Collapsed | A shell display mode, not a task state |
| Failed (not yet mocked) | `Failed` with `failure` |
| Paused, Paused for sleep (PM-5) | `Paused`; caption "Paused for sleep" while the machine sleeps. **Needs a mock.** |

## Handoff · plan and scope

![Handoff card](mockups/handoff.png)

| Element | Source |
|---|---|
| Task text and context chips (DA-1) | `TaskRequest.prompt`, `scope.read/write` |
| Plan, 3–7 steps; "Invoice agent v2.3 · on screen" | `HandoffCard.plan`, `agent.version`, `agent.mode` |
| Can use | `touches[]` (path + access) |
| Will ask first | `asks_before[]` |
| Can't do · set by IT | `cant_touch[]`: IT's Never rules by name, plus data, network and scope limits |
| Data and model | `max_label`, `model` |
| Edit scope (DA-3) | re-plan with `scope.restrict` (stricter only) |
| Start | `task.start` |
| "Snapshots every 5 invoices" | **Partial**: snapshots before every change and Notify step; no cadence yet (AU-20) |
| Save as a routine (DA-4, P1) | **Not built** |

## Admin · agent policy

![Admin console policy page](mockups/admin.png)

The admin console is not started. Today its content lives in `policy.toml`
(`glassctl policy default` prints the shipped one):

| Element | Today |
|---|---|
| Approval tiers per action, "stricter, never looser" | `[org.tiers]`, `[group.tiers]`, `[agent.tiers]`; users only tighten (DA-10) |
| Plain-language action names | `[names]` |
| Allowed agents per group | `[[group]] agents` |
| Data labels and models | `max_label` rules; model routing waits for the gateway |
| Preview impact, publish to pilot ring (AD-3) | **Not built** |
| Live stats: running, waiting, blocked 24h, rollbacks 7d (AD-4) | **Not built**; the audit log has the data |
| Prompt fatigue nudge (DA-14, P1) | **Not built** |
