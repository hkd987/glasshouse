# 0028. Remote help: support roles, consent-gated sessions through the console

Status: **Accepted** (owner's answers 2026-10-19, below). Slice (a),
support roles and signing in by role, is built; slice (b), sessions, is
being built in three steps. PRD CF-80, CF-81, CF-82, UI-2, GT-3. Builds on ADR 0023 (admin console, signed sync), 0016
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

### 4. End to end, so our servers carry only ciphertext (GT-3)
The support person's browser and the device each make a one-time key
pair for the session (ECDH, WebCrypto in the browser); the public halves
travel in the offer and the answer, and both derive the session's keys
(HKDF, AES-GCM, one key per direction, sequence-numbered). The console
relays frames and input it cannot read and keeps none of them. A short
code from both public keys shows on the consent card and on the support
person's page; reading it aloud rules out a console that swapped keys.

### 5. Unattended access, set in the console
IT gives devices a type on the Devices page (`kiosk`, `lab`; none by
default) the way it gives them a ring, and a role's `unattended` lists
the types it may reach with nobody answering. The offer for such a
session names the device's type and is signed like any offer; glassd
starts it only if its own policy's role lists that type, and the banner
shows all the same. The preview warns about every role with unattended
access.

### 6. What remote control never reaches
Remote input never reaches the consent card, the lock or sign-in
screen, power, sign out, or answering, undoing or stopping an agent's
work; it acts in apps as the person, and every action in the session is
audited as made during it. (Proposed with the other answers; the owner
can narrow it.)

### Before slice (b): two gaps it must close first
- **glassd authorizes control calls by uid only.** The kernel gives it
  the caller's pid (`PeerCred.pid`, LOCAL_PEERCRED) but nothing reads it,
  and nothing tells glassd which process is the person's desktop: the
  sign-in screen starts it through `su -l` and never talks to glassd. So
  today any program the person runs could answer a consent card. Step 1
  starts by having the sign-in screen register the desktop's pid with
  glassd for the session, and glassd taking consent answers, frames and
  input only from that process.
- **Signed releases have no expiry or device binding.** `Signed::verify`
  checks the key, hash and `version > have` only, and a reload from disk
  skips the version check. Offers need an expiry, the device they are
  for, and a per-device sequence glassd remembers across restarts; policy
  and catalog releases should get the same expiry check.

### Building slice (b)
1. Offers and consent, no pixels: "Ask to see screen" signs an offer,
   the device checks it, the desktop asks, the answer and the banner are
   real and audited on both sides.
2. Frames: the desktop's changed tiles, encrypted end to end, relayed,
   drawn in the support person's browser.
3. Control: a second request and yes, input injected by the desktop with
   the limits above.

## Consequences
- Policy gains `[support]`; `PolicyDocument` refuses unknown fields, so
  the console deploys before devices that read it, as for `[mail]`.
- Support staff need console accounts through the same sign-in as IT;
  the directory's groups decide who they are (SCIM later).
- Slice (b) grows the device contract (offers, per-device keys, the
  frame relay) and needs the owner's answers first.

## The owner's answers (2026-10-19)
1. **Customer screens through our servers:** end-to-end encrypted
   between the device and the support person's browser (decision 4).
2. **Unattended access:** in V1, by device type IT sets in the console
   (decision 5).
3. **Separation of duties:** roles only; IT admins get no tools by being
   admins (decision 2, as built).
4. **Recording:** events, not video (decision 3's audit).
5. **What remote control never reaches:** decision 6, as proposed.
