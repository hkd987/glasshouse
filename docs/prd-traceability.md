# PRD traceability

Status of every **P0** requirement in the PRD ("Agent-Safe Enterprise Desktop
OS — PRD"), as of milestone M0. **Done** means implemented and covered by a
test named here. **Partial** says what's missing. Update this file in the same
change that moves a requirement.

Tests: *unit* = crate unit tests, *pipeline* =
`crates/svc/runtime/tests/pipeline.rs`, *e2e* = `tests/e2e` in the FreeBSD VM.

## Platform and containment

| Id | Status | Where | Verified by |
|---|---|---|---|
| PA-1 No grants → no files, network, processes | **Done** | `platform/sandbox/jail.rs` | e2e `containment` (19/19 probe checks in a real jail) |
| PA-2 Grants per task, expire at end | **Done** | `runtime/supervise.rs`, `broker/desk.rs` | e2e `stop`, `crash recovery`; pipeline `stopping_tears_down…` |
| PA-3 Escapes are P0 bugs, bounty | Process | — | — |
| PA-4 On-behalf-of tokens downstream | Not started | — | needs identity federation |
| PA-5 Upstream FreeBSD + our layers | **Done** | `os/freebsd`, ADR 0002 | VM boots pinned 15.1-RELEASE |
| PA-6 FDE, secure boot, TPM keys by default | Not started | — | image build work |

## Delegation and approvals

| Id | Status | Where | Verified by |
|---|---|---|---|
| DA-1 Command bar with context | Partial | `glassctl run` (CLI stand-in) | — shell not started |
| DA-2 Handoff card | **Done** (data + CLI) | `runtime.rs::prepare`, `glass-api::HandoffCard` | pipeline `plan_shows_the_handoff_card…` |
| DA-3 Edit scope; policy limits locked | Partial | folders + `--block/--ask` restrictions | pipeline `restrictions_tighten…`; inline UI not started |
| DA-10 IT tiers per agent/group; users stricter only | **Done** | `glass-policy` | unit `engine::*`, pipeline, e2e `scope` |
| DA-11 Approve once or for the task, never forever | **Done** | `broker/desk.rs` | unit `desk::*`, pipeline `sends_wait_for…` |
| DA-12 Batched asks, one approval | **Done** | `mail.send` batches, `Preview::items` | pipeline, e2e `invoices` |
| DA-13 Unanswered asks hold; timeout pauses | **Done** | `execute.rs::ask` | pipeline `approvals_time_out…` |

## Agent window

The tile, expanded view and their states exist as data (`glass-api`) and in
`glassctl` (`tasks`, `watch`, `show`). The compositor, GUI tiles and screen
thumbnails are not started.

| Id | Status | Notes |
|---|---|---|
| AW-1 Stack, newest at bottom, needs-you on top | Partial | ordering in `glassctl`; GUI not started |
| AW-2 Live thumbnail ≤ 5 fps | Not started | needs screen agents |
| AW-3 Three-line feed | **Done** (data) | `TaskView::feed` |
| AW-4 Name, mode, caption, progress | **Done** (data) | `report_progress` tool |
| AW-5 No focus stealing, draggable | Not started | shell |
| AW-10 Expanded view < 200 ms | Partial | `task.get` returns it; GUI not started |
| AW-11 Pause, Stop, Take over | Partial | all three work; "Stop asks once if unsaved" not done |
| AW-12 Now bar | **Done** (data) | `TaskDetail::now` |
| AW-13 Activity and Changes tabs | **Done** (data) | `TaskDetail::{activity, changes}` |
| AW-14 Can't touch panel | **Done** (data) | `EffectivePolicy::cant_touch` |
| AW-15 Timeline, roll back to a marker | **Done** | pipeline `rolling_back_to_a_marker…` |
| AW-16 Shrink back to tile | Not started | shell |
| AW-20 Take over pauses at a safe point within 1 s | Partial | actions agents pause at the next action boundary; display/input hand-off needs screen agents |
| AW-21 Hand back says what changed | **Done** | pipeline + e2e `takeover` |
| AW-22 Take over always available, even during an ask | **Done** | e2e `takeover` |

## Navigation

| Id | Status | Notes |
|---|---|---|
| NV-1 Action registry (name, inputs, side effects, default tier) | **Done** | `glass-actions`; typed *outputs* not yet declared |
| NV-2 Registry speaks MCP | **Done** | ADR 0004 |
| NV-3 Full first-party coverage | Partial | files, mail (mock store); calendar, browser, settings, docs, sheets not started |
| NV-10 Toolkit requires role + label | Not started | shell |
| NV-11 A11y tree through the broker | Not started | |
| NV-20 Screen agents on their own display | Not started | |

## Data protection

| Id | Status | Notes |
|---|---|---|
| DP-1 Gateway is the only model path | Partial | jails have no network at all, so no direct model access; gateway not started |
| DP-2 … DP-5 Allowlist, redaction, injection screening, call logging | Not started | model gateway |
| DP-10 Labels from sources, stored with files | Partial | folder rules + `glasshouse.label` xattrs enforced (e2e `labels`); Purview/Drive import not started |

## Audit, attribution, undo

| Id | Status | Notes |
|---|---|---|
| AU-1 Each action logged with user, agent, task, action, target, decision, approval, snapshot | **Done** | model calls field waits for the gateway |
| AU-2 Append-only, hash-chained, streamed to SIEM | Partial | chain + verify done (unit tamper tests); SIEM streaming and OpenBSM not started |
| AU-3 Rebuild any task end to end | **Done** | `audit.task`, `glassctl audit` |
| AU-10 "by [agent] for [user]" metadata | **Done** | xattr on files, header on mail (verified in VM) |
| AU-20 Snapshots before Notify steps, every 5 min, kept 30 days | Partial | before Notify and first change done; 5-minute cadence and retention not started |
| AU-21 Undo all, lists what it couldn't undo | **Done** | pipeline + e2e `invoices` |

## Admin, desktop, providers

| Area | Status | Notes |
|---|---|---|
| AD-1 … AD-5, AD-10 … AD-12 (console, SCIM, fleet) | Not started | `policy.toml` + `glassd.toml` are the local stand-ins |
| DK-1 … DK-3 (familiar desktop) | Not started | shell |
| MP-1 … MP-9 (model providers) | Not started | gateway |
| HX-1 Claude Code plugin: skills + MCP server | Partial | MCP server and `glass-mcp` bridge done; skills/plugin package not started |
| HX-2 … HX-7 | Not started | needs a Node runtime in the jail and per-harness egress |
