# 0029. Device settings per group, set in the console

Status: **Accepted.** Built: policy, the console, computers applying them
and the Settings app. PRD UX-1, AD-2; the owner
asked that the console control device settings, pushed per role in a
simple way (2026-10-19).

## Context
IT wants to set what people's computers do (lock after idle, the clock,
the browser's home page and blocked sites, camera and microphone) for a role, once, and have it reach the right computers.
Policy already reaches devices signed, per group, through the pilot,
early and broad rings (ADR 0023). There is no Settings app on the device
yet.

## Decision
1. **Settings are policy, per group.** A `[[group]]` gains `settings`, by
   id: `"lock.after_minutes" = { value = 5, locked = true }`. One table in
   `glass_policy::settings` names every setting a computer has (id,
   section, title, kind, bounds, default, scope); policy, the console page
   and the device's Settings app all read it. An id not in the table, or a
   value out of bounds, is refused like any bad rule.
2. **Default or locked.** A setting set without `locked` is the person's
   default, which they can change; locked, they see it and can't.
3. **Which group wins: the higher one.** A person's setting comes from the
   first of their groups, in the policy's order, that sets it, then from
   `everyone`, then from the table. Tiers stay strictest-wins and labels
   and models narrowest-wins: group order changes no agent rule, only
   settings (and which group a tied tier names). IT orders groups on the
   console's Settings page; the preview names people in two groups that
   disagree and says which applies.
4. **The whole computer's settings are everyone's.** The browser's (home
   page, blocked sites, tabs, camera, microphone) are one policy per
   computer whoever uses it, so only `everyone` sets them. Per role needs
   the device to know who is at the screen, which remote help also needs
   (ADR 0028, "Before slice (b)"); it comes with that.
5. **Nothing new to push.** Settings ride the policy release: saved to the
   draft, previewed, published to the pilot ring, promoted.

## Consequences
- `GroupPolicy` refuses unknown fields, so devices must run a glassd that
  knows `settings` before the console publishes any (as for `[mail]`).
- glassd keeps one `SettingsHandle`: the policy in force and each
  person's own choices, in its state directory by uid, never in their
  home. The desktop reads it with `settings.get` and hears
  `SettingsChanged`; the Settings app reads and changes it with
  `settings.list` and `settings.set`; agents may read, not change.
- glassd writes the browser's settings as `managed/org.json` (locked) and
  `recommended/org.json` (defaults). The image's own `glasshouse.json`
  never sets a key those files own, which a test checks, because the
  engine leaves it undefined which file wins.
- Keeping the dock shown waits for the window manager to keep its space
  free; until then the dock steps aside for windows (DK-1).
