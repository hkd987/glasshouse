# PRD traceability

Status of every **P0** requirement in the PRD ("Agent-Safe Enterprise Desktop
OS — PRD"), plus engineering notes on P1 work that shapes today's design.
**Done** means implemented and covered by a test named here. **Partial** says
what's missing. Update this file in the same change that moves a
requirement; the `prd-sync` skill re-reads the PRD and diffs it.

PRD revision last synced: 2026-10-18, third sync (Glasshouse is for all the
work people do on a computer: "Everyday work" categories, a general desktop
agent (AE-3) and an IT self-service agent (AE-4), both P0; Pilot 1 ships
invoice reconciliation, with vendor email covered by AE-3; six new mockups). V1 is **US-only** and **x86 (Intel/AMD)
only**: we build and test `x86_64-unknown-freebsd` and nothing else.

Tests: *unit* = crate unit tests, *pipeline* =
`crates/svc/runtime/tests/pipeline.rs`, *e2e* = `tests/e2e` in the FreeBSD VM.

## Platform, containment and identity

| Id | Status | Where | Verified by |
|---|---|---|---|
| PA-1 No grants → no files, network, processes | **Done** | `platform/sandbox/jail.rs` | e2e `containment` (19/19 probe checks in a real jail) |
| PA-2 Grants per task, expire at end | **Done** | `runtime/supervise.rs`, `broker/desk.rs` | e2e `stop`, crash recovery; pipeline `stopping_tears_down…` |
| PA-3 Escapes are P0 bugs, bounty | Process | — | — |
| PA-4 On-behalf-of tokens downstream | Not started | — | needs identity federation |
| PA-5 Upstream FreeBSD + our layers | **Done** | `os/freebsd`, ADR 0002 | VM boots pinned 15.1-RELEASE |
| PA-6 FDE, secure boot, TPM keys by default | Not started | — | image build work |
| PA-7 Each agent its own directory identity with a human owner | Not started | catalog has id + version only | ADR 0008 (proposed) |
| AD-14 Agents in Microsoft Agent 365 registry | Not started | | |
| AD-15 Policy from Intune and Entra too | Not started | `policy.toml` is the local stand-in | |

## Endpoint protection

| Id | Status | Notes |
|---|---|---|
| EP-1 Read-only, signed, verified base; image updates via boot environments | Not started | Today the image is stock 15.1 plus our overlay (ADR 0002 notes the change) |
| EP-2 Only signed software, veriexec in user and agent jails | Not started | Agents already run only programs from read-only `agent_dirs`; veriexec manifests are the next step |
| EP-3 Scan incoming files | Not started | |
| EP-6 Ransomware recovery from clean snapshots, snapshots tamper-proof | Partial | Task snapshots live on datasets agents can't reach and users can't destroy without `zfs allow`; remote rollback from the console not built |
| EP-7 Alerts and telemetry in OCSF to the customer's XDR | Not started | The audit record needs an OCSF mapping (also AU-2) |
| EP-9 Removable-device control | Not started | Agent jails see no devices beyond the devfs jail ruleset |
| EP-11 … EP-13 Signing keys, reproducible builds, isolated update infra | Not started | Pinned toolchain and `Cargo.lock` are the start of reproducible builds |

## Delegation and approvals

| Id | Status | Where | Verified by |
|---|---|---|---|
| DA-1 Command bar with context | Partial | `glassctl run` (CLI stand-in) | shell not started |
| DA-2 Handoff card | **Done** (data + CLI) | `runtime.rs::prepare`, `HandoffCard` (plan, version, touches, asks, can't do, data and model) | pipeline `plan_shows_the_handoff_card…`, `feed_lines_carry_verb_and_target` |
| DA-3 Edit scope; policy limits locked | Partial | folders + `--block/--ask` restrictions | pipeline `restrictions_tighten…`; inline UI not started |
| DA-10 IT tiers per agent/group; users stricter only | **Done** | `glass-policy` | unit `engine::*`, pipeline, e2e `scope` |
| DA-11 Approve once or for the task, never forever | **Done** | `broker/desk.rs` | unit `desk::*`, pipeline `sends_wait_for…` |
| DA-12 Batched asks, one approval | **Done** | `mail.send` batches, `Preview::items` | pipeline, e2e `invoices` |
| DA-13 Unanswered asks hold; timeout pauses | **Done** | `execute.rs::ask` | pipeline `approvals_time_out…` |

## Agent window

The tile, expanded view and their states exist as data (`glass-api`) and in
`glassctl`. [docs/design](design/README.md) maps every mockup element to its
field. The compositor, GUI tiles and screen thumbnails are not started.

| Id | Status | Notes |
|---|---|---|
| AW-1 Stack, newest at bottom, needs-you on top | Partial | ordering in `glassctl`; GUI not started |
| AW-2 Live thumbnail ≤ 5 fps | Not started | needs screen agents |
| AW-3 Three-line feed, verb + target | **Done** (data) | `FeedLine { verb, target, text }` |
| AW-4 Name, mode, caption, progress | **Done** (data) | `report_progress` tool |
| AW-5 No focus stealing, draggable | Not started | shell |
| AW-10 Expanded view < 200 ms | Partial | `task.get` returns it; GUI not started |
| AW-11 Pause, Stop, Take over | Partial | all three work; "Stop asks once if unsaved" not done |
| AW-12 Now bar | **Done** (data) | `TaskDetail::now` |
| AW-13 Activity and Changes tabs | **Done** (data) | `TaskDetail::{activity, changes}` |
| AW-14 Can't touch panel | **Done** (data) | IT's Never rules by name, plus limits |
| AW-15 Timeline, roll back to a marker | **Done** | pipeline `rolling_back_to_a_marker…` |
| AW-16 Shrink back to tile | Not started | shell |
| AW-20 Take over pauses at a safe point within 1 s | Partial | actions agents pause at the next action boundary; display/input hand-off needs screen agents |
| AW-21 Hand back says what changed | **Done** | pipeline + e2e `takeover` |
| AW-22 Take over always available, even during an ask | **Done** | e2e `takeover` |
| Tile states: Failed with Retry, take over, undo all | **Done** (data + CLI) | `receipt.outcome()` on Failed and Stopped tiles; `task.retry`. Pipeline `an_ended_task_says_what_is_safe…`, e2e `changes`. Take over of a failed actions agent needs a design call (docs/design) |

## Navigation

| Id | Status | Notes |
|---|---|---|
| NV-1 Action registry (name, inputs, side effects, default tier) | **Done** | `glass-actions`; typed *outputs* not yet declared |
| NV-2 Registry speaks MCP | **Done** | ADR 0004 |
| NV-3 Full first-party coverage: files, mail, calendar, browser, settings | Partial | files, mail (mock store); calendar, browser, settings not started. No document or spreadsheet editors of our own: agents use Microsoft 365 and Google Workspace through their APIs |
| NV-10 Toolkit requires role + label | Not started | shell |
| NV-11 A11y tree through the broker | Not started | |
| NV-20 Screen agents on their own display | Not started | |

## Data protection and models

| Id | Status | Notes |
|---|---|---|
| DP-1 Gateway is the only model path | Partial | jails have no network at all, so no direct model access; gateway not started |
| DP-2 … DP-5 Allowlist, redaction, injection screening, call logging | Not started | model gateway |
| DP-10 Labels from sources, stored with files | Partial | folder rules + `glasshouse.label` xattrs enforced (e2e `labels`); Purview/Drive import not started |
| MP-1 … MP-9 Model providers (Bedrock default) | Not started | gateway; handoff card already carries a `model` line |
| HX-1 Claude Code plugin: skills + MCP server | Partial | MCP server and `glass-mcp` bridge done; plugin package not started |
| HX-2 … HX-7 | Not started | needs a Node runtime in the jail and per-harness egress |

## Audit, attribution, undo

| Id | Status | Notes |
|---|---|---|
| AU-1 Each action logged with user, agent, task, action, target, decision, approval, snapshot | **Done** | model calls field waits for the gateway |
| AU-2 Append-only, hash-chained, streamed in OCSF/CEF to the SIEM | Partial | chain + verify done (unit tamper tests); OCSF/CEF export, streaming and OpenBSM not started |
| AU-3 Rebuild any task end to end | **Done** | `audit.task`, `glassctl audit` |
| AU-10 "by [agent] for [user]" metadata | **Done** | xattr on files, header on mail (verified in the VM) |
| AU-20 Snapshots before Notify steps, every 5 min, kept 30 days | Partial | before Notify and first change done; cadence and retention not started |
| AU-21 Undo all, lists what it couldn't undo | **Done** | pipeline + e2e `invoices`; single changes too (Changes mockup): pipeline `undoing_single_changes…`, e2e `changes` |

## Admin, licensing, catalog

| Id | Status | Notes |
|---|---|---|
| AD-1 … AD-5, AD-10 … AD-12 Console, SCIM, fleet | Not started | `policy.toml` + `glassd.toml` are the local stand-ins |
| AD-16 Communications archiving and supervision | Not started | agent-sent mail is already attributed and audited, which this builds on |
| LA-1 … LA-3, LA-10 … LA-12, LA-20 Licensing and activation | Not started | Design constraint: a lapsed license pauses agents only; desktop, data and security updates keep working (LA-3) |
| DV-1 … DV-3, DV-5, DV-10, DV-11 Developer program, private catalog | Not started | DV-3 (declared privileges the OS enforces) shapes the agent manifest: ADR 0008 (proposed) |

## Core OS and workplace

| Id | Status | Notes |
|---|---|---|
| CF-1 … CF-3 Sign-in with Entra/Okta, passwordless, desktop SSO | Not started | |
| CF-10 802.1X, SCEP, PAC | Not started | |
| CF-15 … CF-17 Our Chromium build, patch SLAs, compatibility suite | Not started | |
| CF-20 Teams, Zoom, Meet in the browser | Not started | |
| CF-30 Sleep, wake, battery meet PM-1 … PM-10 | See below | |
| CF-32 Multiple monitors, docks, scaling | Not started | |
| CF-40 OneDrive, SharePoint, Drive sync | Not started | |
| CF-50 Published support policy, 48 h critical fixes | Process | |
| CF-60 Users can always see their own agents' logs | Partial | `audit.task` is open to the task's user; recordings don't exist yet |
| CF-61, CF-62 Retention set by IT; no individual-level reporting | Not started | |
| CF-63, CF-64 Recordings encrypted and labeled; legal hold | Not started | |
| CF-80 … CF-82 Built-in remote help desk | Not started | |

## Power management

| Id | Status | Notes |
|---|---|---|
| PM-1 Modern standby, < 2 s resume | Not started | hardware certification; FreeBSD 15.1 modern standby |
| PM-4 1,000 sleep/wake cycles per certified device | Not started | release gate on real hardware |
| PM-5 Agents checkpoint and pause before sleep, resume on wake | **Done** (local agents) | `Runtime::suspend/wake` via `rc.d/glassd` `suspend`/`resume` keywords, which `/etc/rc.suspend` and `/etc/rc.resume` run. Pipeline `agents_pause_for_sleep…`, e2e sleep step. Remote runtimes (PA-9) are P1; tested through the rc.d path, not a real ACPI suspend |
| PM-10 Certify only after PM-1 … PM-6 | Process | |

## Developer workspace

| Id | Status | Notes |
|---|---|---|
| DW-1, DW-3, DW-10, DW-11, DW-13, DW-20, DW-21, DW-23 | Not started | A bhyve Linux VM plus developer jails, audited from the host. Reuses the jail, broker and audit pieces; the runtime's sandbox interface was built for a second backend |

## Branding and notices

| Id | Status | Notes |
|---|---|---|
| BR-1 No FreeBSD where users look; system identity says Glasshouse | Partial | Overlay layer done: os-release, motd, boot menu brand, logo and title, our splash, muted boot, login screen art and banner, SSH version, login tips, our own strings (`tools/branding/art.py`). e2e branding check; boot screens recorded with `bootshots.py`. The UEFI loader's banner, kernel identity, base tools and man pages need our own base build, and the kernel's reported name needs a decision (ADR 0011) |
| BR-2 License notices in Settings → Legal notices and docs | Partial | `tools/legal/notices.py` generates `NOTICES.txt` for every shipped crate plus the base `COPYRIGHT`, staged into the image (e2e checks it). The Settings page waits for the shell; per-component base notices wait for the base build |
| BR-3 Source offer for GPL components | Not started | No GPL code ships today. The base system's CDDL code (OpenZFS, DTrace) carries a similar source obligation: legal review needed (ADR 0011) |
| BR-4 Full base details and SBOM to security teams under NDA, admins only | Partial | `make sbom` writes a CycloneDX SBOM to `build/`, never staged into the image. Admin export comes with the console (EP-10) |

## UI stack

| Id | Status | Notes |
|---|---|---|
| UI-1 … UI-6 Wayland compositor on Smithay, iced + AccessKit shell, web apps with manifests | Not started | ADR 0009. The control API already carries everything tiles, the expanded view, handoff and Changes show (docs/design) |

## Agent engine

| Id | Status | Notes |
|---|---|---|
| AE-1 Agents are versioned harness bundles; hard limits in the OS | Partial | The hard limits are enforced in `glassd` today, outside any agent. The bundle format and runner are proposed (ADR 0010) |
| AE-2 First-party workflow agents; Pilot 1 ships invoice reconciliation | Not started | `glass-demo-agent` is a scripted stand-in with no model. Vendor email moved to AE-3 |
| AE-3 General desktop agent, the command bar's default; hands off to specialists | Not started | Needs the gateway and bundles (ADR 0010). `glassctl run <agent>` names the agent today; routing by `agents.list` descriptions is the gap (docs/design, Command bar) |
| AE-4 IT self-service agent: approved fixes through a dedicated broker role, logged and undoable, never security settings; ticket when it can't fix | Partial | `system.*` actions (network, storage, performance checks; cache cleanup, undoable; service restart from IT's `[fixes]` list) and `helpdesk.ticket.create`, run by glassd (ADR 0012); policy allows them to `it-agent` only. Scripted `glass-it-agent`. Unit tests, pipeline `only_the_it_agent_reaches_the_system…`, e2e `it`. Missing: certificates, printers, app installs, a real help desk adapter, the model-backed agent |
| AE-10 … AE-12 Eval suites, release bar, live quality metrics | Not started | The e2e harness on ZFS snapshots is the base of the eval runner; take-overs, undos and declined approvals are already audit events per agent |
| AE-30 Re-run evals before a model switch | Not started | Needs the gateway and bundles |

## Pilot readiness

| Id | Status | Notes |
|---|---|---|
| GT-1 … GT-4, GT-10, GT-30 | Process | GT-3 (customer data stays in their boundary) and GT-21 (control plane on AWS) are architecture constraints for the console and activation services |
