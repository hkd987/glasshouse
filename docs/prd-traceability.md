# PRD traceability

Status of every **P0** requirement in the PRD ("Agent-Safe Enterprise Desktop
OS — PRD"), plus engineering notes on P1 work that shapes today's design.
**Done** means implemented and covered by a test named here. **Partial** says
what's missing. Update this file in the same change that moves a
requirement; the `prd-sync` skill re-reads the PRD and diffs it.

PRD revision last synced: 2026-10-19, fifth sync (a Mac-like desktop:
menu bar, dock, window controls on the left, Ctrl Space command bar; the
"Desktop experience and design" section, UX-1 … UX-40, with UX-1, UX-2,
UX-10, UX-20 … UX-22 and UX-30 in P0; four new mockups: boot, sign in,
snapping and ask the agent, themes). V1 is **US-only** and **x86 (Intel/AMD)
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
| DA-1 Command bar with context | Partial | `glassctl run` (CLI stand-in); on the VM's screen Ctrl Space opens the shell's command bar, typing works and Recent shows real tasks. Missing: the desktop agent's answers and handing off from the bar | `glass-shell` `tests/live.rs` |
| DA-2 Handoff card | **Done** (data + CLI) | `runtime.rs::prepare`, `HandoffCard` (plan, version, touches, asks, can't do, data and model) | pipeline `plan_shows_the_handoff_card…`, `feed_lines_carry_verb_and_target` |
| DA-3 Edit scope; policy limits locked | Partial | folders + `--block/--ask` restrictions | pipeline `restrictions_tighten…`; inline UI not started |
| DA-10 IT tiers per agent/group; users stricter only | **Done** | `glass-policy` | unit `engine::*`, pipeline, e2e `scope` |
| DA-11 Approve once or for the task, never forever | **Done** | `broker/desk.rs` | unit `desk::*`, pipeline `sends_wait_for…` |
| DA-12 Batched asks, one approval | **Done** | `mail.send` batches, `Preview::items` | pipeline, e2e `invoices` |
| DA-13 Unanswered asks hold; timeout pauses | **Done** | `execute.rs::ask` | pipeline `approvals_time_out…` |

## Agent window

The tile, expanded view and their states exist as data (`glass-api`) and in
`glassctl`. [docs/design](design/README.md) maps every mockup element to its
field. The shell (`shell/`) draws the tiles and the expanded view from that
data, on the VM's screen through `glass-shell-fb`. The compositor and screen
thumbnails are not started.

| Id | Status | Notes |
|---|---|---|
| AW-1 Stack, newest at bottom, needs-you on top | Partial | ordering in `glassctl` and the shell's tile stack, live on the VM's screen; Approve on a needs-you tile answers its ask. Missing: the compositor, dismissing tiles |
| AW-2 Live thumbnail ≤ 5 fps | Not started | needs screen agents |
| AW-3 Three-line feed, verb + target | **Done** (data) | `FeedLine { verb, target, text }` |
| AW-4 Name, mode, caption, progress | **Done** (data) | `report_progress` tool |
| AW-5 No focus stealing, draggable | Not started | shell |
| AW-10 Expanded view < 200 ms | Partial | `task.get` answers in ~4 ms in the VM (e2e `perf`, docs/dev/performance.md). The shell opens a tile (or a needs-you tile's Review) into the expanded view from data in memory plus that one call, and fetches it again when the task changes (shell `aw10_*`, `aw13_the_open_task_changing_refetches_its_detail`, snapshot `expanded`). Missing: an open-to-drawn budget; a full 1440x900 frame takes ~0.2 s on the dev host in a debug build and 1.6–2 s in the emulated VM (ADR 0016) |
| AW-11 Pause, Stop, Take over | Partial | all three work; the shell's header shows the ones that apply to the tile's state (Resume while paused, Hand back while you drive, none once ended) and calls them (`aw11_controls_follow_the_tile_state`, `aw11_header_controls_call_their_methods`, `aw10_clicking_a_tile_opens_it_and_its_header_stops_the_task`). "Stop asks once if unsaved" not done |
| AW-12 Now bar | **Done** (data) | `TaskDetail::now`, drawn in the shell's expanded view. The left pane shows the feed, the ask's items or the receipt; a live screen needs screen agents |
| AW-13 Activity and Changes tabs | **Done** | `TaskDetail::{activity, changes}`; the shell's tabs, snapshot rows ◇ and changes that can't be undone flagged (`aw13_*`, snapshot `expanded`) |
| AW-14 Can't touch panel | **Done** | IT's Never rules by name, plus limits; the shell's Can't touch card (snapshot `expanded`) |
| AW-15 Timeline, roll back to a marker | **Done** | pipeline `rolling_back_to_a_marker…`; undo `rolling_back_to_a_marker_removes_drafts…`. The shell's timeline: pick a marker, Roll back shows the dry run's report, then rolls back and shows what it did (`aw15_*`) |
| AW-16 Shrink back to tile | **Done** | the shell's collapse button and Esc; the tiles keep updating behind the view (`aw16_esc_collapses_the_expanded_view`, `aw16_tiles_keep_updating_behind_the_expanded_view`, `aw10_clicking_a_tile_opens_it…`) |
| AW-20 Take over pauses at a safe point within 1 s | Partial | actions agents pause at the next action boundary, ~0.2 s in the VM (e2e `perf`); display/input hand-off needs screen agents |
| AW-21 Hand back says what changed | **Done** | pipeline + e2e `takeover` |
| AW-22 Take over always available, even during an ask | **Done** | e2e `takeover` |
| Tile states: Failed with Retry, take over, undo all | **Done** (data + CLI) | `receipt.outcome()` on Failed and Stopped tiles; `task.retry`. Take over on an ended actions agent continues in the user's harness (`task.continue`, ADR 0015). Pipeline `an_ended_task_says_what_is_safe…`, `continuation::…`; e2e `changes`, `harness` |

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
| HX-1 Claude Code plugin: skills + MCP server | Partial | MCP server and `glass-mcp` bridge done; Claude Code is an attended catalog agent the user connects with `glassctl take-over` / `glassctl run claude-code` (ADR 0015); plugin package (skills, hooks) not started |
| HX-2 … HX-7 | Not started | needs a Node runtime in the jail and per-harness egress |

## Audit, attribution, undo

| Id | Status | Notes |
|---|---|---|
| AU-1 Each action logged with user, agent, task, action, target, decision, approval, snapshot | **Done** | model calls field waits for the gateway |
| AU-2 Append-only, hash-chained, streamed in OCSF/CEF to the SIEM | Partial | chain + verify done (unit tamper tests); OCSF/CEF export, streaming and OpenBSM not started |
| AU-3 Rebuild any task end to end | **Done** | `audit.task`, `glassctl audit` |
| AU-10 "by [agent] for [user]" metadata | **Done** | xattr on files, header on mail (verified in the VM) |
| AU-20 Snapshots before Notify steps, every 5 min, kept 30 days | Partial | before Notify and first change done; cadence and retention not started (snapshots are never destroyed yet) |
| AU-21 Undo all, lists what it couldn't undo | **Done** | pipeline + e2e `invoices`; single changes too (Changes mockup): pipeline `undoing_single_changes…`, e2e `changes`. Only the task's own files, and a file changed since is left and listed (ADR 0017): `crates/svc/runtime/tests/undo.rs` |

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
| BR-2 License notices in Settings → Legal notices and docs | Partial | `NOTICES.txt` in the image: every shipped crate (`tools/legal/notices.py`), the base `COPYRIGHT`, and every imported base component's license texts from the release source (`tools/legal/base_notices.py`, 96 components). e2e `check-branding.sh` checks it. The Settings page waits for the shell |
| BR-3 Source offer for GPL components | Partial | `NOTICES.txt` opens with the source offer: the 17 base components under CDDL or GNU licenses (5 also offer a permissive choice) and the unmodified source release by SHA-256. Missing: the legal contact (`make image-release` refuses until it's set) and legal review of the list. None of our own code is copyleft |
| BR-4 Full base details and SBOM to security teams under NDA, admins only | Partial | `make sbom` writes a CycloneDX SBOM to `build/`, never staged into the image. Admin export comes with the console (EP-10) |

## UI stack

| Id | Status | Notes |
|---|---|---|
| UI-4 Shell in Rust with iced and AccessKit | Partial | `shell/` (its own workspace): boot, sign-in, desktop and command-bar screens match the mockups, from `glass_api` data. `glass-shell-fb` draws the desktop on the VM's screen (console framebuffer) with live tiles from `events.subscribe` (`make shell-vm`). Missing: the compositor (ADR 0016), input, AccessKit |
| UI-1 … UI-3 Wayland compositor on Smithay, isolation, agent displays | Not started | ADR 0016 (accepted): framebuffer output in the VM, verified with `glass-shell-fb`; DRM on hardware |
| UI-5, UI-6 Web apps and native apps with manifests | Not started | ADR 0009 |

## Desktop experience

The desktop is Mac-like (DK-1, revised): menu bar, dock, window controls on
the left, agent tiles at the top right, command bar on Ctrl Space. The
element-by-element map is in [docs/design](design/README.md).

| Id | Status | Notes |
|---|---|---|
| DK-1 Mac-like desktop | Partial | menu bar, dock, tiles, command bar and a tile's expanded view (the agent becomes the menu bar's app); the desktop shows on the VM's screen with live tiles and clock, and takes clicks and keys from evdev. No app windows yet |
| UX-1 Chef's-choice defaults | Not started | |
| UX-2 Setup asks ≤ 5 questions (none when enrolled), desktop in < 5 min | Not started | first-run setup (Setup mockup) |
| UX-10 One theme restyles the whole system | Partial | the shell keeps every color and the wallpaper in one `Theme`; only Glass exists, no picker |
| UX-20 Everything by keyboard, cheat sheet, Windows shortcuts too | Not started | shell |
| UX-21 Snapping, saved layouts, optional tiling | Not started | compositor |
| UX-22 Performance budgets on every build; papercut program | Partial | OS-side budgets in `glass-e2e perf` (docs/dev/performance.md); input lag, app launch, window open and wake need the shell |
| UX-30 "Ask the agent" on every error, crash and failed update | Partial | the IT agent diagnoses, fixes from IT's list and files tickets (AE-4); the notification and its button need the shell |
| UX-3, UX-11, UX-31, UX-40 Try-it VM, org themes, explain any setting, tailorable desktop | P1 | UX-40 reuses tailoring layers (ADR 0014) |

## Agent engine

| Id | Status | Notes |
|---|---|---|
| AE-1 Agents are versioned harness bundles; hard limits in the OS | Partial | The hard limits are enforced in `glassd` today, outside any agent. The bundle format and runner are proposed (ADR 0010) |
| AE-2 First-party workflow agents; Pilot 1 ships invoice reconciliation | Not started | `glass-demo-agent` is a scripted stand-in with no model. Vendor email moved to AE-3 |
| AE-3 General desktop agent, the command bar's default; hands off to specialists | Not started | Needs the gateway and bundles (ADR 0010). `glassctl run <agent>` names the agent today; routing by `agents.list` descriptions is the gap (docs/design, Command bar) |
| AE-4 IT self-service agent: approved fixes through a dedicated broker role, logged and undoable, never security settings; ticket when it can't fix | Partial | `system.*` actions (network, storage, performance checks; cache cleanup, undoable; service restart from IT's `[fixes]` list) and `helpdesk.ticket.create`, run by glassd (ADR 0012); policy allows them to `it-agent` only. Scripted `glass-it-agent`. Unit tests, pipeline `only_the_it_agent_reaches_the_system…`, e2e `it`. Missing: certificates, printers, app installs, a real help desk adapter, the model-backed agent |
| AE-10 … AE-12 Eval suites, release bar, live quality metrics | Not started | Decided 2026-10-18: Pilot 1's workflow is invoice reconciliation with a 95% release bar (AE-11's proposed target). The e2e harness on ZFS snapshots is the base of the eval runner; take-overs, undos and declined approvals are already audit events per agent |
| AE-30 Re-run evals before a model switch | Not started | Needs the gateway and bundles |

## Tailoring and apps built on request

Users shape their own tools (PRD "The edge", ADR 0014). Success metric:
25% of seats tailor or build at least one tool; every layer save is a
`LayerChanged` audit event per user, which is what the metric will count.

| Id | Status | Notes |
|---|---|---|
| TL-1 Publishers declare customization points; the rest is locked | **Done** | `[[agent.customize]]` in the catalog (text, number, flag, choice, instructions), checked at startup; values checked on save. Unit tests in `glass-tailoring`, `tailoring::tl1_…`, e2e `tailor` |
| TL-2 Personal layers, versioned and undoable | **Done** | `layer.get/set/revert`, `glassctl tailor` (`--undo`, `--revert`, `--off`); the agent gets `GLASS_AGENT_SETTINGS` and its instructions; the card and the task's activity name the version. `tailoring::tl2_…`, e2e `tailor` |
| TL-10 Layers only narrow permissions | **Done** | a layer holds settings and limits only; limits must tighten the user's policy on save and apply as a maximum. `tailoring::tl10_…`, e2e `tailor` |
| TL-11 Re-run the evaluation suite before a layer turns on | Not started | needs eval suites (AE-10); the "Release bar" check reports "not available" rather than passing |
| TL-13 New agent version: re-apply and re-test every layer; pause failures, notify the owner | Partial | re-checked against the new version's points at startup and before use, paused with the reason, published agent runs; owner sees it on the card and in `glassctl tailor`. Missing: re-test with evals, a pushed notification. `tailoring::tl13_…` |
| TL-3, TL-4, TL-12, TL-14, TL-20 … TL-22 Team layers, copies, IT controls, inventory, suggestions | P1 | ADR 0014 "Proposed next" |
| GA-1 … GA-11 Apps built on request | P1 | A generated app is an ordinary app (UI-5): its actions join the registry like any `crates/apps` provider (ADR 0013) and get tiers, audit and undo for free. Catalog-first (GA-2) needs the catalog service; sharing steps (GA-8) reuse the DV-2/DV-3 checks |

## Pilot readiness

| Id | Status | Notes |
|---|---|---|
| GT-1 … GT-4, GT-10, GT-30 | Process | GT-3 (customer data stays in their boundary) and GT-21 (control plane on AWS) are architecture constraints for the console and activation services |
