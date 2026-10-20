# 0038. The owner of a computer no organization manages

Status: **Accepted** (2026-10-20). Built: the owner, their settings for
the computer, and Settings showing who may change what. Next: the AI
models setup asked about, applied (slice 2). PRD UX-1, UX-2, UX-31. The
owner asked that individuals, not only enterprises, can use their own
computer and turn its settings on and off (2026-10-20). Extends ADR 0029
and ADR 0031.

## Context
- ADR 0029 split settings by scope. A person's own (`person`: the lock
  screen, the clock, Do Not Disturb, hidden apps) are theirs unless IT
  locks them. The computer's own are IT's alone: `device` (the time zone,
  remixing) and `seat` (USB drives and the browser's five, which follow
  whoever is signed in, ADR 0031).
- On a computer no organization manages there is no IT. Before this
  decision, nobody could change the time zone, USB drives, the browser's
  home page, blocked sites, camera or microphone, or remixing. The
  Settings app showed them as values only.
- First-run setup (UX-2) creates the computer's first account, on a
  computer nobody signs in to yet and no organization manages. That person
  is who a Mac would make its administrator.

## Decision
1. **A computer no organization manages has an owner: the account
   first-run setup made.** The sign-in screen records it in
   `/var/db/glasshouse/setup.toml` (`owner = "<login>"`), beside the AI
   models answer. A computer set up before that line existed gets its
   first person's account, the lowest uid with a login. glassd reads the
   owner at start, and only when its config has no `[managed]` section. An
   enrolled computer has no owner: IT decides, as before.
2. **The owner sets the computer's own settings, for everyone on it.**
   Both `device` and `seat` settings are included. A seat setting
   (USB drives, camera, blocked sites) is the computer's posture, and on a
   shared home computer the owner sets it as IT would. It doesn't follow
   whoever signs in. The owner's choices are kept root only in
   `<state>/settings/computer.json`. They are separate from each person's
   `<uid>.json`, whose loader keeps only `person` settings, so a person's
   own file is never a way in.
3. **One resolution, in `SettingsHandle`.** In order, a value comes from:
   1. a setting policy locks;
   2. the owner's choice;
   3. policy's default;
   4. the built-in default.

   `person()` (what Settings shows) and `computer(seat)` (what glassd
   applies) both read it, and a test holds them equal for every one of the
   computer's settings. The computer applies the owner's choice as
   enforced, as it does what IT locked: the browser gets it as a managed
   policy, not a recommendation anyone could undo in the browser's own
   settings. Only the owner changes it, in Settings. Remixing, which the runtime and the studio asked
   policy for directly, now asks the handle. A change by the owner tells
   every listener (`None`), so glassd applies it at once: the time zone,
   the USB rule, the browser's policy, remixing.
4. **glassd says who may change what.** Each setting carries `editable`
   (and `from: "owner"` when another person's owner chose it). Settings,
   the desktop and the console don't each work the rule out. Settings
   shows the owner a control for each of the computer's settings, with
   "Your choice, for everyone on this computer". Anyone else sees "Chosen
   by this computer's owner".
5. **Never an agent.** `settings.set` is in no agent's allowed actions,
   and the provider also refuses a change to one of the computer's own
   settings from anything but the Settings app itself. A remix of
   Settings has its own id, so it is refused too. UX-31's "changed by the
   agent, within policy" holds for a person's own settings. An agent
   changing the computer's would be its own action that asks the owner,
   if it's ever wanted. The person's own settings stay changeable as
   before, from the desktop (`settings.set` on the control socket, by
   kernel uid) and from Settings.

## Consequences
- An individual can turn USB drives off, set the time zone, block sites,
  set the browser's home page, turn the camera or microphone off for
  websites, and turn remixing off. Each is on the record like any
  setting change, and applied at once.
- A computer that later enrolls with an organization stops applying the
  owner's choices; IT's take over. The file stays but is ignored while
  there is no owner, so un-enrolling brings them back.
- The owner's login is named to other people on the computer when they
  try to change one ("its owner, dana, changes it in Settings").
- Not decided here: a second owner, or handing ownership over. Setup
  records one. The AI models answer is recorded but still not applied
  (slice 2: the owner's key, kept root only, for the model gateway).
