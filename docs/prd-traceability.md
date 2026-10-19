# PRD traceability

Status of every **P0** requirement in the PRD ("Agent-Safe Enterprise Desktop
OS — PRD"), plus engineering notes on P1 work that shapes today's design.
**Done** means implemented and covered by a test named here. **Partial** says
what's missing. Update this file in the same change that moves a
requirement; the `prd-sync` skill re-reads the PRD and diffs it.

PRD revision last synced: 2026-10-19, seventh sync ("Building the base
system": pkgbase with a thin fork, BS-1 … BS-6, and "A thin platform
layer", PL-1 … PL-4; BS-1 … BS-6, PL-1 and PL-2 are P0; ADR 0022). The
sixth brought "Future direction: always-on agents with local hands", with
architecture commitments FD-1 … FD-4 in P0 and always-on cloud agents a
non-goal for Pilot 1. The fifth
brought the Mac-like desktop (UX-1 … UX-40) and the boot, sign-in,
snapping and themes mockups. V1 is **US-only** and **x86 (Intel/AMD)
only**: we build and test `x86_64-unknown-freebsd` and nothing else.

Tests: *unit* = crate unit tests, *pipeline* =
`crates/svc/runtime/tests/pipeline.rs`, *e2e* = `tests/e2e` in the FreeBSD VM.

## Platform, containment and identity

| Id | Status | Where | Verified by |
|---|---|---|---|
| PA-1 No grants → no files, network, processes | **Done** | `platform/sandbox/jail.rs` | e2e `containment` (21/21 probe checks in a real jail, including the resource limits) |
| PA-2 Grants per task, expire at end | **Done** | `runtime/supervise.rs`, `broker/desk.rs`; the jail's resource limits go with it (`exec.poststop`, `platform/sandbox/jail.rs`) | e2e `stop`, crash recovery (orphaned jail and its limits removed); pipeline `stopping_tears_down…`; e2e root checks: no jails, mounts or limits left behind |
| PA-3 Escapes are P0 bugs, bounty | Process | — | — |
| PA-4 On-behalf-of tokens downstream | Not started | — | needs identity federation |
| PA-5 Upstream FreeBSD + our layers | **Done** | `os/freebsd`, ADR 0002 | VM boots pinned 15.1-RELEASE |
| PA-6 FDE, secure boot, TPM keys by default | Not started | — | image build work |
| PA-7 Each agent its own directory identity with a human owner | Not started | catalog has id + version only | ADR 0008 (proposed) |
| AD-14 Agents in Microsoft Agent 365 registry | Not started | | |
| AD-15 Policy from Intune and Entra too | Not started | Managed policy from our own console is built (ADR 0021, 0023): boot from the last applied policy on disk, fetch in the background, verify, validate, apply whole with the previous version kept; Intune and Entra would feed the same path | unit `store::tests` (fallback, apply, rollback) |

## Endpoint protection

| Id | Status | Notes |
|---|---|---|
| EP-1 Read-only, signed, verified base; image updates via boot environments | Not started | Today the image is stock 15.1 plus our overlay (ADR 0002 notes the change) |
| EP-2 Only signed software, veriexec in user and agent jails | Not started | Design: ADR 0026 (Proposed). `mac_veriexec` and `veriexec(8)` aren't in 15.1's GENERIC kernel or base packages, so this needs our own kernel, loader and veriexec builds (BS-2). Agents already run only programs from read-only `agent_dirs` |
| EP-3 Scan incoming files | Not started | |
| EP-6 Ransomware recovery from clean snapshots, snapshots tamper-proof | Partial | Task snapshots live on datasets agents can't reach and users can't destroy without `zfs allow`; remote rollback from the console not built |
| EP-7 Alerts and telemetry in OCSF to the customer's XDR | Not started | The audit record needs an OCSF mapping (also AU-2) |
| EP-9 Removable-device control | Not started | Agent jails see no devices beyond the devfs jail ruleset |
| EP-11 … EP-13 Signing keys, reproducible builds, isolated update infra | Not started | Key layout in ADR 0026 (Proposed): an offline root over OS, package, developer CA and per-channel keys. Pinned toolchain and `Cargo.lock` are the start of reproducible builds |

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
| AW-1 Stack, newest at bottom, needs-you on top | Partial | ordering in `glassctl` and in the shell's agents menu (opened from the menu bar's pill). On the desktop, tiles show only as banners for a few seconds when something happens (a new agent, an ask, an ending), so they never take the screen; Approve on a banner answers the ask. Missing: the compositor, dismissing ended tiles | `glass-shell` `live::tests::ux_*`, `tests/live.rs` |
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
| NV-3 Full first-party coverage: files, mail, calendar, browser, settings | Partial | files; mail (mock store: list, read, draft, send); calendar (`glass-app-calendar`: list, read, hold, create, update, delete, invite; pipeline `nv3_calendar_holds_notify_invitations_ask_and_undo_takes_the_hold_back`, crate `nv3_*`). Mail and Calendar also ship as apps (UI-5). Browser and settings not started. No document or spreadsheet editors of our own: agents use Microsoft 365 and Google Workspace through their APIs |
| NV-10 Toolkit requires role + label | Not started | shell |
| NV-11 A11y tree through the broker | Not started | |
| NV-20 Screen agents on their own display | Not started | |

## Data protection and models

| Id | Status | Notes |
|---|---|---|
| DP-1 Gateway is the only model path | **Done** | native agents have no network (stricter than pf); they call `model_complete`, which glassd routes, redacts, sends and audits (ADR 0018; pipeline `models.rs` `dp1_model_calls_are_an_action_on_the_task_socket`, e2e `models`). Harnesses follow HX-7 |
| DP-2 Model allowlist, Bedrock default, routing by label and task type | Partial | `[models]` allowlist, default and per-label routes; group and agent lists only narrow; the card's model line is the route for the task's folders (unit `dp2_*` in glass-policy, pipeline `dp2_*`). The shipped policy approves no model until IT adds one (Bedrock first in its example). Routing by task type not started |
| DP-3 Redact secrets and PII, restore where safe | **Done** | secrets always (keys, tokens, private keys, credential assignments), PII per policy (email, SSN, card numbers, IT's patterns); PII restored in replies, secrets never (unit `dp3_*` in glass-gateway, pipeline `dp3_secrets_and_pii_are_redacted_before_leaving_and_pii_comes_back`) |
| DP-4 Injection screening of retrieved content | Not started | |
| DP-5 Every call logged: model, tokens, cost, labels, task | **Done** | the action's audit record with `detail.model`: model, provider, tokens, cost from policy prices, label, redactions, request/reply sizes and SHA-256, failovers; refusals too; no prompt text unless `log_prompts` (pipeline `dp5_every_call_is_audited_with_sizes_and_hashes_not_text`, unit `dp5_policy_can_keep_the_redacted_prompt`) |
| DP-6 Token budgets per user and team | Not started | |
| DP-10 Labels from sources, stored with files | Partial | folder rules + `glasshouse.label` xattrs enforced (e2e `labels`); Purview/Drive import not started |
| MP-1 Bedrock first, no long-lived AWS keys on devices | Partial | Bedrock provider (Converse, SigV4) takes temporary credentials only: `AKIA` keys refused, session token required, expiry honored (unit `mp1_only_temporary_credentials_are_used`). Not run against a real account; first-run setup and IAM Identity Center or role sign-in not started |
| MP-2 Bedrock region pinning, PrivateLink | Partial | policy pins each Bedrock provider to a region and can name a PrivateLink endpoint (unit `mp2_sends_a_signed_converse_request_and_reads_the_reply`); untested against AWS |
| MP-3 Bedrock Guardrails | Not started | |
| MP-4 API keys only in the secrets broker | Partial | Anthropic API keys come from a root-only `model-secrets.toml` glassd refuses unless private, outside every jail (unit `mp4_*`); the secrets broker, per-team keys, spend per key and rotation not started |
| MP-5 Bring your own subscription | Not started | Claude plans work through attended Claude Code (ADR 0015) |
| MP-6 Access modes per group | Partial | group `models` lists, such as Bedrock only for a confidential group (unit `dp2_groups_and_agents_only_narrow_the_allowlist`); subscription toggles not started |
| MP-7 Provider-neutral agents, local Anthropic and OpenAI endpoints | Partial | native agents call one provider-neutral tool, `model_complete`; the local HTTP endpoints in the jail are designed in ADR 0018, not built |
| MP-8 Routing and failover by label, task and cost | Partial | routes by label, failing over on an outage, rate limit or missing credentials, logged in `failed_over` (pipeline `mp8_an_outage_fails_over_to_the_next_model_and_is_logged`); by task and cost not started |
| MP-9 Model catalog in the admin console | Not started | policy already carries each model's provider, region, display name and prices |
| HX-1 Claude Code plugin: skills + MCP server | Partial | MCP server and `glass-mcp` bridge done; Claude Code is an attended catalog agent the user connects with `glassctl take-over` / `glassctl run claude-code` (ADR 0015); plugin package (skills, hooks) not started |
| HX-2 … HX-7 | Not started | needs a Node runtime in the jail and per-harness egress |

## Audit, attribution, undo

| Id | Status | Notes |
|---|---|---|
| AU-1 Each action logged with user, agent, task, action, target, decision, approval, snapshot | **Done** | model calls carry their call record (DP-5) |
| AU-2 Append-only, hash-chained, streamed in OCSF/CEF to the SIEM | Partial | chain + verify done (unit tamper tests); OCSF/CEF export, streaming and OpenBSM not started |
| AU-3 Rebuild any task end to end | **Done** | `audit.task`, `glassctl audit` |
| AU-10 "by [agent] for [user]" metadata | **Done** | xattr on files, header on mail (verified in the VM) |
| AU-20 Snapshots before Notify steps, every 5 min, kept 30 days | Partial | before Notify and first change done; cadence and retention not started (snapshots are never destroyed yet) |
| AU-21 Undo all, lists what it couldn't undo | **Done** | pipeline + e2e `invoices`; single changes too (Changes mockup): pipeline `undoing_single_changes…`, e2e `changes`. Only the task's own files, and a file changed since is left and listed (ADR 0017): `crates/svc/runtime/tests/undo.rs` |

## Admin, licensing, catalog

| Id | Status | Notes |
|---|---|---|
| AD-1 Policies per group, synced from Entra ID or Okta via SCIM | Partial | Groups come from the console's directory (`directory.toml` behind the `Directory` trait, the SCIM seam); a published policy carries each group's members from it. Missing: SCIM. `glass-console directory::ad1_…` |
| AD-2 Per agent: allowed actions, a tier per action class, labels, models, egress | Partial | The console edits a group's tier per action class, allowed agents, most sensitive label and apps, and an agent's network destinations, validating the whole draft as devices will (ADR 0023). Allowed actions and model lists are shown, not yet edited. `edits::ad2_…`, `server::ad3_edit_preview_publish…` |
| AD-3 Versioned, previewed ("affects 214 users and 3 agents"), rolled out in stages | **Done** | Signed versions, publish to pilot, promote to early and broad, roll back as a new version everywhere; preview against what the broad ring runs; devices verify, validate whole and apply in the background, keeping the previous (ADR 0021, 0023). `impact::ad3_…`, `server::ad3_…`, glassd `managed::ad3_…`, `dv1_…`, runtime `catalog::adr0023_a_policy_applied_live…`, e2e `console.e2e.ts` "ad3 dv1 …" (in force on a pilot device under 5 s) |
| AD-4 Live view: running agents, pending approvals, blocks in 24 h | Partial | Devices page and each policy page's live panel, from device reports (ids, names and counts only), sent on every change and each minute. Missing: a per-task view across the fleet. `server::ad4_…`, e2e "ad4 …" (pages under 1 s with 500 devices) |
| AD-5 IT approves, versions and publishes agents and flows to groups | Partial | Catalog page: approve per group with an install mode, block; each decision signs a catalog for every device (ADR 0023). Missing: installing on devices, item versions over time. `server::dv11_…`, e2e "dv11 ga2 …" |
| AD-6 The admin console: a web app on our servers, the fleet at a glance | Partial | `glass-console` serves `apps/console` (Policies, Catalog, Devices); admins of the directory sign in (ADR 0023). Development mode only: TLS at our web servers, the org's SSO and a managed database aren't built. `make console-test` |
| AD-7 Apps in a profile by role; admin UI and IT tools only for the roles that grant them | Partial | `[[group]] apps` in policy, set from the console; glassd's `apps.list` (`glassctl apps`) returns the caller's profile (apps not on this device left out) and `app.open` refuses an app outside it (audited `AppRefused`); the desktop's launcher offers only what the profile names, and nothing while glassd doesn't answer (`apps::offered`, `ad7_*`). The shipped default gives everyone Mail, Calendar, Activity Monitor and Terminal, and IT's role the admin console (a profile entry pointing at the console's servers). Missing: native apps are hidden by the launcher, not refused by the system (a person can still run the binary from Terminal); the console's on-device app needs the browser runtime |
| AD-10 Zero-touch enrollment | Not started | Stub: a device enrolls with `[managed]` in `glassd.toml` and the org's enrollment token (ADR 0023) |
| AD-11 OS and app updates in rings, automatic rollback | Partial | Policy releases go out in pilot, early and broad rings (ADR 0023); OS and app updates don't |
| AD-12 Remote lock and wipe, key escrow, compliance status | Not started | |
| AD-16 Communications archiving and supervision | Not started | agent-sent mail is already attributed and audited, which this builds on |
| LA-1 … LA-3, LA-10 … LA-12, LA-20 Licensing and activation | Not started | Design constraint: a lapsed license pauses agents only; desktop, data and security updates keep working (LA-3) |
| DV-1 Everything signed | Partial | Policy and catalog releases are signed with the org's ed25519 key and verified by devices against keys in their own config (kind, org, version, payload hash; replays refused). Missing: publisher identity and signing certificates. `signing::dv1_…`, glassd `managed::dv1_…` |
| DV-2 Automated checks on every submission | Not started | The console shows the checks a submission reports |
| DV-3 Listings declare their privileges; the OS enforces them | Partial | Catalog items declare actions, most sensitive label, network and default tiers; shown in the console and returned by `catalog.search`. Enforced for apps' manifests (ADR 0024), not yet for agents (ADR 0008). `catalog app dv3_…` |
| DV-5 Revoke a signature fleet-wide within an hour | Partial | Blocking an item drops it from the next catalog devices sync (`server.rs` test of the next catalog; nothing times the block end to end yet). Missing: signature revocation, disabling installed copies, notifying admins |
| DV-10 Each org's private catalog; only approved items; internal items never public | Partial | Releases hold approved items only; the store rule refuses anything public (DV-13). Missing: installing from the catalog on devices. `server::dv11_…_dv13_…` |
| DV-11 Per group: required, available or blocked | Partial | Set per group in the console and synced; blocked wins, then required over available; `catalog.search` honors it. Missing: pushing required items. `catalog::dv11_…`, `ga2_dv11_…` |
| DV-13 Only single-user apps from unmanaged devices go public; enterprise apps never | **Done** | `CatalogItem::public_store_check` refuses org-scoped items and anything built on a managed device (unknown provenance reads as both); the console's public-store request answers with the rule. No public store exists yet. `catalog::dv13_…`, `server::dv11_…_dv13_…` |

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
| BR-3 Source offer for GPL components | Partial | `NOTICES.txt` opens with the source offer: the 17 base components under CDDL or GNU licenses (5 also offer a permissive choice) and the unmodified source release by SHA-256; an image built from base packages (`make image-pkgbase`) names instead the pinned catalog's `FreeBSD-src` and `FreeBSD-src-sys` packages by SHA-256, whose patch level matches its packages (`tools/legal/notices.py --pkgbase-catalog`). Missing: the legal contact (`make image-release` refuses until it's set), legal review of the list, keeping those two source packages (about 260 MB) for every catalog we ship from for the offer's three years, and the notices of pkg (in package-list images) and the libraries it bundles, which its package doesn't carry. None of our own code is copyleft |
| BR-4 Full base details and SBOM to security teams under NDA, admins only | Partial | `make sbom` writes a CycloneDX SBOM to `build/`, never staged into the image. Admin export comes with the console (EP-10) |

## UI stack

| Id | Status | Notes |
|---|---|---|
| UI-4 Shell in Rust with iced and AccessKit | Partial | `shell/` (its own workspace): boot, sign-in, desktop and command-bar screens match the mockups, from `glass_api` data. `glass-shell-fb` draws the desktop on the VM's screen (console framebuffer) with live tiles from `events.subscribe` (`make shell-vm`), keyboard and pointer from evdev, and redraws only what changed. `glass-greeter` signs people in on that screen through PAM and starts their desktop, and checks the password that unlocks its lock screen (ADR 0019, `make greeter-vm`). Missing: the compositor (ADR 0016), AccessKit |
| UI-1 … UI-3 Wayland compositor on Smithay, isolation, agent displays | Partial | `shell/crates/glass-compositor` (UI-1): Wayland for apps, windows centered in the work area, focus and input routing, software composition (pixman) redrawing only what changed; tests with real clients (`ui1_*`). The desktop hosts it on the VM's screen: windows between the wallpaper and the shell's UI, hidden behind the lock screen, input routed to the window under the pointer or in focus (`tests/layers.rs`, `live::ui1_*`). Title bars, moving and closing windows (DK-1); isolation (UI-2) and agent displays (UI-3) not started |
| UI-5 Web apps with manifests, reaching the system only through the brokers, offline | Partial | ADR 0024: glassd's app socket runs an app's calls through the pipeline as "by [app] for [user]", refusing undeclared actions (DV-3); `glass-app-bridge` (loopback dev mode); `apps/` (SDK, Mail, Calendar; reads offline). Runtime `apps.rs` tests, glassd `dv3_apps_install_only…`, `make apps-test` (Playwright). Missing: our Chromium build as the app runtime, the OS-side app identity, install through the catalog, packaging into the image |
| UI-6 Native apps with manifests | Partial | `glass-window` puts an iced view in a Wayland window with pointer and keys (ADR 0025, `ui6_*`); Terminal and Activity Monitor build on it. Owed: the accessibility tree, and action manifests on the app socket (ADR 0024) |
| UI-7 Web app budgets: cold launch under 1 s, under 150 MB | **Done** (dev host) | `apps/e2e/perf.spec.ts` fails over budget; Mail and Calendar launch cold in about 0.18 s and use about 115 MB (renderer RSS), `docs/dev/performance.md` |
| UI-8 Terminal | Partial | `glass-terminal`: the person's login shell in a window (alacritty's terminal state and pty, xterm keys, 256 colors, scrollback on the wheel, which the desktop passes to the window under the pointer: `glass-compositor` `ui8_the_wheel_*`), shown on the VM's desktop. Opens from the launcher. Select with the mouse; Ctrl Shift C and V (and Ctrl C on a selection) copy and paste between apps, bracketed when the shell asks, with escapes taken out of what's pasted. Zoomed or resized by an edge, it fills the window and tells the shell its new size (`glass-terminal` `dk1_zoomed_*`). Not yet: the accessibility tree. Native, ADR 0025 |
| UI-9 Activity Monitor | Partial | `glass-activity`: the processors and memory, the person's agents (everything in each one's jail added up, by `ps`'s jail column) and their apps, by CPU and memory every 2 s; Stop agent (`task.stop`) and Quit (the person's own processes). Opens from the launcher; tested on the VM. Not yet: power per app (PM-7), "Ask the agent" about a slowdown (AE-4), network and disk. Native, ADR 0025 |

## Desktop experience

The desktop is Mac-like (DK-1, revised): menu bar, dock, window controls on
the left, agent tiles at the top right, command bar on Ctrl Space. The
element-by-element map is in [docs/design](design/README.md).

| Id | Status | Notes |
|---|---|---|
| DK-1 Mac-like desktop | Partial | apps' windows with title bars and the controls on the left (close asks the app to close; drag the bar to move; `glass-compositor` `dk1_*`); menu bar, dock, tiles, command bar and a tile's expanded view (the agent becomes the menu bar's app); the desktop shows on the VM's screen with live tiles and clock, and takes clicks and keys from evdev. The machine boots into the sign-in screen and signs in to the desktop; Shift Super Q or the system menu under the mark signs out (ADR 0019). Notification banners stay while the pointer is on them. The minimize control hides a window and Alt Tab brings it back where it was (`glass-compositor` `dk1_the_minimize_control_*`). The zoom control fills the work area with a window and puts it back where and as big as it was; apps that can, draw at the new size (`glass-window` `dk1_zoom_*`; Terminal re-lays its grid and tells the shell). Dragging an edge or corner resizes a window (`dk1_dragging_a_corner_*`) |
| DK-2 Windows shortcuts work | Partial | Super tapped on its own opens the launcher, as the dock's launcher icon does; Esc or a click outside closes it, and a click starts the app (`dk2_the_launcher_*` drives the dock icon; the Super tap is seen on the VM only, no test yet). Alt Tab shows the open windows, the one last used first; Tab and Shift Tab move, letting go of Alt raises the chosen window and gives it the keyboard, Esc cancels (`glass_shell` `dk2_alt_tab_*`, `dk2_the_switcher_*`; `glass-compositor` `dk2_activating_*`; keys held in an app are released there, `glass-shell-fb` `dk2_a_key_pressed_*`). Copy and paste across apps: the app with the keyboard gets the clipboard, native apps copy and paste through it (`glass-window` `dk2_copy_in_one_app_and_paste_in_another`), and Terminal selects with the mouse, copies with Ctrl Shift C or Ctrl C on a selection and pastes with Ctrl Shift V (`glass-terminal` `dk2_*`). Not yet: web apps' clipboard (with the browser runtime) |
| UX-1 Chef's-choice defaults | Not started | |
| UX-2 Setup asks ≤ 5 questions (none when enrolled), desktop in < 5 min | Partial | First-run setup in the sign-in screen (`glass_shell::setup`, `screens::setup`, `glass-greeter`): on a computer nobody signs in to yet and no org manages, three questions (your name, a password, how agents reach AI models), then the account is created (`glass-platform` `create_person`: `pw`, the home its own ZFS dataset so undo works) and the person is on their desktop; an enrolled computer (`[managed]`) asks nothing. Tested on a fresh package-list image VM: setup to desktop in under a minute of typing. Not yet: the AI-model choice is recorded (`/var/db/glasshouse/setup.toml`) but not applied; sign-in with Entra ID, licensing, Wi-Fi |
| UX-10 One theme restyles the whole system | Partial | the shell keeps every color and the wallpaper in one `Theme`; only Glass exists, no picker |
| UX-20 Everything by keyboard, cheat sheet, Windows shortcuts too | Partial | Tab and Shift Tab move between text fields on every screen (`ux2_tab_moves_*`), Alt Tab between windows (DK-2), and the shortcuts DK-2 lists. Super / and the system menu's Keyboard Shortcuts show a cheat sheet, checked against the keys the desktop handles (`ux20_the_cheat_sheet_*`). Ctrl F2 opens the system menu (a Mac's key for the menu bar) and Super N the agents menu (Windows' notifications); in them and in the launcher, arrows, Home and End move, skipping lines that can't be chosen yet, and Enter chooses; Enter on an agent opens it and never approves (`ux20_arrows_*`, `ux20_super_n_*`). The dock's live items are the launcher (Super) and the agents (Super N). Not yet: the expanded view's buttons by keyboard (Tab reaches only text fields), scrolling the agents menu to a selection below its fold |
| UX-21 Snapping, saved layouts, optional tiling | Not started | compositor |
| UX-22 Performance budgets on every build; papercut program | Partial | OS-side budgets in `glass-e2e perf` (docs/dev/performance.md), and the same reads with two busy agents running (`glass-e2e load`; agents run below the person's priority with resource limits); boot measured with `tools/boot/timeline.py` (no budget enforced yet; `check-boot.sh` in `make e2e-vm` checks the boot settings and times `glasshouse_identity`); input lag, app launch, window open and wake need the shell |
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
| GA-2 Catalog first: suggest an approved app or agent instead of building a duplicate | Partial | `catalog.search` (Auto, read only, audited) finds approved items the user's groups may install, by need, with their declared privileges, from the catalog IT published to the device; groups come from the runtime, never the request (ADR 0023). Claude Code has it by default. `crates/apps/catalog` `ga2_…`, runtime `catalog::ga2_…`, glassd `managed::ga2_…`, e2e "dv11 ga2 …". Missing: nothing tests an agent choosing it over building (its tool description says to); building apps is GA-1 (P1) |
| GA-1, GA-3 … GA-11 Apps built on request | P1 | A generated app is an ordinary app (UI-5): its actions join the registry like any `crates/apps` provider (ADR 0013) and get tiers, audit and undo for free. Sharing steps (GA-8) reuse the DV-2/DV-3 checks; GA-10's controls belong in the console |

## Base system and portability

Glasshouse builds on pkgbase with a thin fork, and stays portable above
the kernel through one platform crate (ADR 0022).

| Requirement | Status | Where | Notes |
|---|---|---|---|
| BS-1 A thin fork: a small documented patch set on FreeBSD's stable branch | Not started | We ship FreeBSD 15.1's binaries unmodified; branding is an overlay (BR-1) | The patch set starts with the first base change (kernel identity, ADR 0011) |
| BS-2 Rebuild only changed base packages; FreeBSD's re-signed into our repository | Not started | The pinned mirror comes first and exists: `os/freebsd/scripts/pkg-mirror.py` caches FreeBSD's `base_release_1` in `build/cache`, catalog SHA256 pinned in `release.env` | Our repository and key; packages still carry FreeBSD's signature |
| BS-3 Each image a package list from that repository, without compilers, tests, debug files or 32-bit libraries | Partial | `make image-pkgbase` installs exactly `os/freebsd/images/desktop.pkgs` (62 packages; no `-dbg`, `-dev`, `-lib32`, `-tests`, toolchain or manual pages) with `pkg --rootdir` on a new pool (`bake-pkgbase.sh`, `pkgbase-root.sh`); the build fails on an unlisted package or a missing library. Image 157 MiB against 978 MiB (ADR 0022) | Boots to sign-in and passes e2e (13/13, containment 21/21 with the app-socket check, 2026-10-19 03:55) in a VM from it. Missing: our repository (BS-2); `make image` and the dev VM still start from the official image; sshd and nuageinit are in the list for development |
| BS-4 Whole signed images through ZFS boot environments | Not started | | With EP-1 |
| BS-5 Advisory → package → tested image → release within CF-50 | Not started | | Needs the release pipeline |
| BS-6 Every component a versioned package, for inventory and advisory matching | Partial | The base is packages (`pkg query`). A package-list image records its packages and versions in `/usr/local/share/glasshouse/packages.txt` and beside the image; `make sbom` lists them as base components next to our crates (`tools/legal/notices.py --base-packages`) | The three third-party libraries staged from ports (libxkbcommon, pixman, xkeyboard-config) are in the SBOM but not in the on-device `packages.txt`; our binaries and overlay as `glasshouse-*` packages; advisory matching |
| PL-1 FreeBSD features only through a thin platform adapter, each with a Linux equivalent | Partial | `glass-platform`: jails, ZFS, framebuffer, evdev, PAM, accounts | Document each facility's Linux equivalent in the crate |
| PL-2 Brokers, gateway, policy, shell, agents and apps free of FreeBSD code; built and tested on Linux | Partial | `make test` and the shell tests run on the Linux host (host sandbox, copy snapshots); `make lint` refuses `target_os = "freebsd"` outside `glass-platform` | CI runs the same on Linux |

## Architecture commitments (future direction: always-on agents)

The PRD's future direction runs an agent's reasoning in the cloud with its
actions still on the laptop (not in Pilot 1). These P0 commitments keep the
architecture from blocking it; ADR 0020 has the rules for new code.

| Requirement | Status | Where | Notes |
|---|---|---|---|
| FD-1 Brokers are location-independent | Partial | Agents reach glassd only through the task's MCP socket; the pipeline decides on the task, not the transport; paths are relative to grants | A remote transport bound to device keys is future work |
| FD-2 Work checkpoints and moves mid-task; the same mechanism handles sleep | Not started | Sleep (PM-5) holds agents at their next call in memory | Checkpoint call in the task API (ADR 0020) |
| FD-3 Agent identity not tied to one machine | Not started | Catalog id + version | Directory identities (PA-7, ADR 0008) |
| FD-4 Snapshots and undo history follow a task across machines | Not started | Change records (ADR 0017) are in memory; snapshots are local ZFS | Persist change records with the task; per-task snapshots can be sent |

## Pilot readiness

| Id | Status | Notes |
|---|---|---|
| GT-1 … GT-4, GT-10, GT-30 | Process | GT-3 (customer data stays in their boundary) and GT-21 (control plane on AWS) are architecture constraints for the console and activation services |
