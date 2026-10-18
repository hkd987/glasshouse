# 0009. A native Rust shell on Smithay and iced; apps are web apps

Status: **Accepted** (PRD UI-1 … UI-7). Not started in code.

## Context
The October PRD fixes the UI stack: a Wayland-only compositor in Rust on
Smithay (UI-1), each agent's display its own isolated compositor output
(UI-3), the shell (taskbar, tiles, notifications, lock screen, settings
frame) in iced with AccessKit (UI-4, Slint as fallback pending a license
review), and Glasshouse apps as TypeScript web apps on our Chromium build,
each with a manifest of actions, permissions and labels (UI-5). Native
Wayland apps are allowed if they expose a full accessibility tree and
declare actions (UI-6). The PRD's open questions still ask to confirm the
toolkit; this records what we build against until then.

## Decision
- **One shell process, a client of the control API.** Tiles, the expanded
  view, the handoff card and the Changes view read `glass-api` (`task.*`,
  `events.subscribe`) exactly as `glassctl` does. The shell links
  `glass-api` and `glass-rpc`, never service crates (ADR 0001). The design
  map in `docs/design/README.md` is its contract.
- **The compositor owns capture and input.** Screen capture, remote viewing
  (CF-80) and take over go through compositor-level, consent-gated paths
  (UI-2); nothing else can read another client's surfaces or input. Agent
  displays (UI-3) are compositor outputs, so tiles, take over and recording
  share one mechanism.
- **Apps reach the system only through brokers.** A web app's manifest
  actions register in the action registry (NV-1) like a built-in provider;
  their calls go through the same decide → snapshot → audit pipeline.
- **Where it lives:** a new top-level `shell/` area (compositor, shell UI)
  and `apps/` (first-party web apps), as `docs/repo-layout.md` anticipates.
- **Licenses:** Smithay (MIT), iced (MIT) and AccessKit (MIT/Apache-2.0)
  fit `deny.toml`. Slint is GPL-3.0 or a royalty-free or commercial license,
  so the fallback needs the commercial terms before anyone depends on it.

## Consequences
- The shell can be built and tested before the compositor runs on real
  hardware: Smithay's headless backend renders outputs offscreen, which is
  also how agent displays work, so screenshot tests run in the TCG VM.
- Cross-compiling the compositor needs FreeBSD's graphics libraries
  (libinput, libseat, Mesa, DRM) in the sysroot, from packages rather than
  `base.txz`. Their licenses, and the drm-kmod kernel modules', need the
  same review as our crates before they ship.
- The PRD makes first-party apps web apps, so the files and mail providers
  in `glass-actions` become the system side of those apps' actions; the app
  UI is TypeScript.
- UI-7's budgets (cold launch under 1 s, under 150 MB per app) become
  release checks once the first app exists.
