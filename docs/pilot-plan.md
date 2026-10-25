# The pilot plan

What stands between today's build and a pilot, ranked (2026-10-25). The
pilot: a few dozen people at one organization, on the laptops ADR 0057
picked (ThinkPad T14 Gen 2 Intel first, Dell Latitude 7490, ThinkPad X1
Carbon Gen 9), run from the org's admin console in its pilot ring, agents
doing real mail, calendar, files and IT work. Nothing has run on a real
laptop yet: everything so far ran in the emulated dev VM and on the dev
host.

Sizes: **S** days, **M** one to two weeks, **L** more. Statuses are
`docs/prd-traceability.md`'s.

## Where the requirements stand
205 rows of the traceability, 267 ids (a row can hold several):

| Status | Rows | Ids |
|---|---|---|
| Done (12 of them qualified: "data", "dev host", "on hardware") | 41 | 40 |
| Partial | 92 | 109 |
| Not started | 62 | 96 |
| In progress (CF-91, DK-12, EM) | 3 | 6 |
| Process (PA-3, CF-50, PM-10, GT-1 … GT-4, GT-10, GT-30) | 4 | 9 |
| Other ("P1", "See below") | 3 | 7 |

The traceability covers the PRD's P0 items and some P1s, not every id.
GT-1 and GT-2 make SOC 2 Type I, a penetration test, questionnaires, a
trust center and insurance pilot gates; GT-10 the customer terms. They're
process, outside this ranking.

## P0: the pilot can't start without these

| Id | What | Missing | Size |
|---|---|---|---|
| DK-12 | The pilot laptops | No pilot model has run the image. `tests/hardware/capture.sh` and the `docs/dev/hardware.md` checklist on one of each, T14 Gen 2 first. It is also the hardware check for Wi-Fi (CF-12), Bluetooth (CF-13), sleep (PM-5, UX-1), graphics (ADR 0058), brightness (ADR 0060) and the Wi-Fi kernel patch | M |
| PM-1 (S3) | Lid sleep and wake, timed | Never measured on a laptop | M |
| PM-4 | Sleep and wake cycles | None on real hardware; needs Wake on LAN or a key presser (the X1 Carbon has no Ethernet) | M |
| BS-4 | OS updates on a laptop | Passed in the dev VM only; two versions take about 12 GB of disk | M |
| BS-3 | The pilot image | The package-list image, without the development-only sshd and nuageinit | S |
| EP-11 … EP-13 | Production keys | Releases and kernel patches are signed with development keys (see the production checklist) | M |
| BR-3 | The source offer | No legal contact, so `make image-release` refuses; the list wants legal review | S |
| AD-6 | The console in production | Development mode only: no TLS at our servers, no org SSO, no managed database | L |
| CF-1 (minimum) | Accounts on an enrolled laptop | A managed computer's first boot makes no accounts (ADR 0053): pilot people can't sign in | M |
| MP-1, MP-2 | Models through the org's Bedrock | Never run against a real AWS account; temporary credentials not started; region pinning untested | M |
| PA-6 | Disk encryption | None, on laptops that will hold real mail. Secure Boot can wait | L |
| AD-12 | Key escrow, remote lock | Nothing. Escrow comes with encryption; wipe may wait | L |
| CF-20 | Teams, Zoom and Meet in the browser | Never tried. Screen sharing needs UI-2 | M |
| CF-40 | Cloud files | Files and the calendar live only on the laptop: a dead laptop loses them | L |
| AE-4 (tickets) | The IT agent's tickets | They reach no real help desk. Or the agent stops offering them | M |

**P0 if the org needs it**, settled in the pilot's first week:
Microsoft 365 as the provider (IN-1 … IN-5; mail already works over any
IMAP and SMTP service), 802.1X Wi-Fi (CF-10), Chat (ADR 0030's C2 and C3),
SIEM and XDR streaming (AU-2, EP-7), communications archiving (AD-16), the
invoice agent if this is the PRD's "Pilot 1" (AE-2), our model credits
(MP-10, MP-11), Glasshouse Mail (EM), Glasshouse meetings (CF-21), and
artifacts' controls (AR-6).

**In what order**
1. **DK-12 on one T14 Gen 2, now.** Every hardware claim is unproven
   until then, and what it finds may add kernel patches and reorder the
   rest. PM-1 and PM-4 follow once sleep works; BS-4 on its disk.
2. **Meanwhile, what needs no laptop:** AD-6, the keys and BR-3 (no release
   image without them), and Bedrock (ask the org for its AWS account
   early).
3. **A person's first day:** CF-1's minimum, BS-3, PA-6 with AD-12's
   escrow. Encryption changes how laptops are installed: settle it before
   the image is frozen.
4. **Daily work:** CF-20, CF-40, AE-4's tickets.
5. **The pilot's policy:** attended agents off (they run with no sandbox,
   ADR 0053), community kernel patches off (ADR 0057).

## P1: pilot people would hit these weekly

| Id | What | Missing | Size |
|---|---|---|---|
| AE-3, DA-1, DA-3 | The desktop agent and command bar | Handing off to specialists; answers in the bar; dragged-in files and selections; the scope card before a task starts | M |
| CF-80 … CF-82 | Remote help | Seeing and controlling a screen, consent and its banner; waits on the owner's answers in ADR 0028 | L |
| BS-5, AD-11 | Fixes in days | No release pipeline; IT can't restart computers or set a deadline; app updates aren't in rings | M |
| CF-32, DK-11, DK-2, UI-2 | Desk work | Monitors and docks, printing, the web apps' clipboard, screen-share isolation. Rank after DK-12 | L |
| AE-12 | The pilot's results | Live metrics: take-overs, undos and declines are already audit events | M |
| DP-4, EP-3 | Security asks | Prompt-injection screening; scanning incoming files | L |
| AD-2, AD-10, UX-2, BR-4, AW-1, AE-4 | Smaller gaps | Per-agent policy edits in the console; zero-touch enrollment; Wi-Fi in first-run setup; SBOM export; dismissing ended tiles; the IT agent's other fixes | S–M |

## P2 and later
For general availability: EP-1's read-only, verified base and EP-2's
verified execution (ADR 0026), identity federation (PA-4, PA-7, CF-1 …
CF-3), Intune and Entra (AD-14, AD-15), our Chromium build (CF-15 …
CF-17), the developer workspace (DW), licensing (LA), sheets and
scheduling (SH, SC), the store's signing and revocation (DV), and the
rest of the Partial rows. Later still: modern standby (PM-1, upstream),
bring-your-own subscriptions and harnesses in jails (MP-5, HX, waiting on
Claude Code for this OS), the future directions (FD).

## The production checklist
What only the owner can decide or provide; none of it is code here.

- **Laptops:** one ThinkPad T14 Gen 2 (Intel) to start DK-12, then one of
  each pilot model; or SSH to one booted from USB.
- **The offline root key** (EP-11): an HSM, a key ceremony with its
  runbook, and the root signing the OS key and one key per channel
  (pilot, early, broad). The key list's format is ADR 0061's.
- **The release pipeline** (EP-13, BS-5): isolated build hosts that make
  each image from its `@glass-image` snapshot, keep the previous one for
  deltas, and sign with the OS and channel keys from the HSM.
- **Glasshouse's update server** (ADR 0061): an HTTPS host, with a CDN for
  images of gigabytes, serving `keys.json` and each channel's release;
  each org's console mirrors it hourly (`--updates-from`).
- **The kernel build service** (`glass-kbuild`, ADR 0057): a host running
  the release the patches are for, its signing key in the HSM.
- **The admin console** (AD-6): where each org's console runs, its TLS,
  its backups (`fleet.json`, policy releases, the org key's custody), and
  SSO for its admins.
- **Model access:** the pilot org's AWS account for Bedrock (MP-1), and,
  for orgs on our credits, production OpenRouter credentials and the
  billing relay's Stripe account (ADR 0039, MP-10).
- **Meetings and chat**, if the org uses them: the meeting relay's hosting
  (`glass-sfu`) and the chat service's.
- **Legal:** the source offer's contact and review (BR-3), the customer
  terms (GT-10).
- **Decisions waiting:** ADR 0026's open questions (scripts under
  verified execution, catalog installs at runtime, development images on
  customer hardware), ADR 0028's remote-help questions, ADR 0063 (Macs).

## Traceability fixes
Rows that contradict their own notes, found while ranking. Fixed now:
NV-3 (documents are our own editor's), IN-2 listed twice, EP-9 (USB
drives per group are built: Partial). Still to settle: the compositor
called missing in the Agent window rows (it exists, UI-1); DA-1 and DA-2
overstating the command bar's handoff; AE-3 and MP-4's "no real model run"
(there have been); CF-12, CF-13 and PM-5 "Done" without hardware; AD-10's
working stub; rows missing for DK-3, CF-11, CF-41, CF-43 and DK-4.
