# 0028. Remote help: support roles, consent-gated sessions through the console

Status: **Proposed.** Slice (a), support roles and signing in by role, is
built; slices (b) and (c) wait on the owner's answers below. PRD CF-80,
CF-81, CF-82, UI-2. Builds on ADR 0023 (admin console, signed sync), 0016
(the desktop draws the screen itself), 0019 (sign-in screen), 0015
(attended agents run as the person).

## Context
Remote help is part of the OS (CF-80). What a support person can do
depends on their directory role, and IT configures the tiers: help desk
views a screen and controls it with consent; desktop support can also
install approved apps, collect logs and restart services; security can
isolate a device and collect forensic data (CF-81). The person approves
each session and sees a banner while support is connected; unattended
access only for roles and device types IT permits; every session is in
the audit log (CF-82). Screen capture happens only through a consent-gated,
logged system prompt (UI-2).

What exists: the desktop composes the whole screen itself (wallpaper, app
windows, its own UI) and reads input from evdev, so it can capture its own
frames and inject input without any capture protocol. Devices reach the
console outbound only, by a long-polled sync (ADR 0023). The console
authorized every signed-in person for every route.

## Decision

### 1. Roles are policy (slice a, built)
A `[support]` section of policy names the tiers, maps directory groups to
them, and says how far each reaches:

```toml
[support]
answer_within_secs = 120
[[support.role]]
id = "help-desk"
name = "IT Help Desk"
groups = ["it-helpdesk"]       # directory groups
tools = ["view", "control"]
people = ["everyone"]          # whose screens it may ask for
unattended = []                # device types it may reach with nobody answering
max_minutes = 60
```

Tools are a closed set: `view`, `control`, `install_apps`, `collect_logs`,
`restart_services`, `isolate`, `forensics`. `control` needs `view`. Being
policy, roles are checked like any rule, previewed, published to the pilot
ring and rolled out, and a device will check a session against its own
signed copy (slice b). The console seeds the three PRD tiers into a new
org's first policy; the device's built-in default has none, so an
unmanaged computer has no remote help.

### 2. Signing in by role, every route closed by default (slice a, built)
Anyone in IT's admins, or in a group a role names in the newest policy the
pilot ring runs, may sign in; in development they pick themselves from a
list of admins and support staff, never the whole directory. A role change
takes effect once IT publishes it. Every console route needs admin unless
it is named as a support route; a support person's session reaches only
the Support page and their own session. Separation of duties: IT admins
get no support tools unless a role names their group.

### 3. Sessions (slice b, proposed)
A support person asks to view a device's screen; the console checks their
role, signs a one-time offer for that device (a new release kind, so it
can't pass for a policy or catalog, with an expiry and a sequence number
against replay) and hands it to the device's long poll. glassd checks it
against its own policy (the role, its tools and reach, the person signed
in, unattended rules) and the desktop asks the person: "Jordan Lee, IT
Help Desk, wants to see your screen (ticket HD-48213)", Allow or Not now,
no default button, lapsing unanswered. While connected the menu bar turns
into a banner naming who is watching, with End session. Control is a
second request and a second yes. Either side ends it; signing out,
locking the screen, a time limit or a lost console end or pause it.

Only the desktop answers and sends frames: glassd accepts those calls from
the process the sign-in screen registered for the session, by pid as well
as uid, so another process of the person's (Terminal, their own Claude
Code) can't answer or watch. Agents can never start or see a session.
Remote input never reaches the consent card, the lock screen, power, sign
out, or answering, undoing or stopping an agent's work.

Frames go from the desktop to glassd to the console to the support
person's browser, as compressed tiles of what changed, a few a second, on
connections the device opens; nothing listens on the device. Everything
is audited on both sides (requested, answered, started, control, ended,
with who, why and how long; never pixels or keystrokes).

## Consequences
- Policy gains `[support]`; `PolicyDocument` refuses unknown fields, so
  the console deploys before devices that read it, as for `[mail]`.
- Support staff need console accounts through the same sign-in as IT;
  the directory's groups decide who they are (SCIM later).
- Slice (b) grows the device contract (offers, per-device keys, the
  frame relay) and needs the owner's answers first.

## Open questions (owner)
1. **Customer screens through our servers.** The PRD says our cloud holds
   metadata only (GT-3); relayed frames are not metadata. End-to-end
   encryption between the device and the support person's browser as a
   pilot gate, or a relay in the customer's own account?
2. **Unattended access at the sign-in screen** (kiosks, labs): in V1?
   Which device types, and who sets them (enrollment or the console)?
3. **Separation of duties:** should IT admins get remote help without a
   role naming them? Proposed: no.
4. **Recording:** "every session is recorded in the audit log" read as
   events, not video. Does compliance want recordings (which conflicts
   with GT-3)?
5. **What remote control may never do:** proposed above (no asks, power,
   sign-out, unlock). Is acting in apps during control, marked with the
   session, acceptable under AU-1?
