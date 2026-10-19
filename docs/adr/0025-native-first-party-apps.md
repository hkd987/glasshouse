# 0025. Terminal and Activity Monitor are native apps on `glass-window`

Status: **Accepted** (PRD UI-6; product request 2026-10-19, pending PRD).
Numbered 0025 because 0023 and 0024 are taken on branches not yet merged.

## Context
The PRD makes first-party apps TypeScript web apps on our Chromium build
(UI-5, ADR 0009), and allows native apps built with Rust if they run on
Wayland, expose a full accessibility tree and declare their actions in a
manifest (UI-6). The product owner asked for a Terminal and an Activity
Monitor (now UI-8 and UI-9 in the PRD). Both need
what a web app can't have: a pseudo-terminal and a shell for one, the
machine's processes for the other. And our browser engine isn't in the
image yet, so no web app can open a window on the device today.

## Decision
- Terminal and Activity Monitor are **native** apps (UI-6), not web apps.
  Other first-party apps (Files, Mail, Calendar, PDF, Settings) stay web
  apps (UI-5).
- They're built on **`glass-window`** (`shell/crates/glass-window`): one
  Wayland toplevel whose content is an iced view drawn with the shell's own
  renderer (`glass_shell::render::Screen`) into two shared-memory buffers,
  sent again only where it changed. The look is the desktop's: same fonts,
  theme and widgets. Pointer and keys come back as iced events; keys are
  read through the compositor's keymap with xkbcommon (never a built-in
  layout), repeated by the app while held, and an app may take keys before
  its UI does (a terminal takes all of them). Other threads wake the loop
  through a `Waker`.
- They live in the shell workspace and ship like the desktop (dist
  manifest, notices).

## Consequences
- Owed before they count as UI-6 apps: an **accessibility tree**
  (AccessKit, as the shell owes too, UI-4) and a **manifest** declaring
  their actions, so agents can use them the way UI-5 apps' manifests allow.
  Until then agents can't drive them, which is also the safe default for a
  terminal.
- A Terminal runs the person's shell as the person: no new reach for
  agents (they never get a terminal; their tools come through the broker).
  The Activity Monitor reads processes through base commands (ADR 0007).
- Apps that need neither a pty nor process access wait for the browser
  engine rather than going native, so the web-app path (UI-5) stays the
  rule.
