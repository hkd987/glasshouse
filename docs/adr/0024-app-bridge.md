# 0024. The app bridge: an app is a principal its manifest bounds

Status: **Accepted** for the first slice (UI-5, DV-3, NV-1, AU-10). The
OS-side identity of an app (below, "What this slice trusts") is proposed.
Numbered 0024 because 0023 is taken on the admin console's branch.

## Context
ADR 0009 makes Glasshouse apps TypeScript web apps whose manifest actions
"go through the same decide → snapshot → audit pipeline". Nothing said how
a page reaches an action. The PRD sets the terms: apps reach the system
only through the brokers and work offline (UI-5); a listing declares the
actions it calls and the data labels it touches, and the OS enforces the
declaration so an item can never exceed it (DV-3); whatever an agent writes
carries "by [agent] for [user]" (AU-10). There is no Chromium build of our
own yet, so the first apps run in a stock Chromium, and the tests drive
them with Playwright on the development host.

## Decision
- **An app is a principal, like an agent.** Its manifest's `id` is its
  identity, in the same namespace as catalog agents, since both act "by
  [id] for [user]". An open app is an *app session*: a task in the runtime
  whose principal is the app, acting for the user who opened it. Every call
  runs the agent pipeline (`runtime/src/execute.rs`): preview → decide →
  ask → snapshot → invoke → audit. Mail sending a reply is audited and
  attributed "by mail for demo", exactly as Claude Code's draft is "by
  claude-code for demo". An app session has no sandbox, no program and no
  folder grants; it reaches data only through its declared actions.
- **Manifests come from the installed copy.** glassd reads
  `<apps_dir>/<id>/manifest.json` at start (`/usr/local/share/glasshouse/apps`,
  read-only system files), never from the running app. A manifest installs
  only if `AppManifest::check` passes (one-line name and version, no
  patterns, no duplicates, labels declared, never `restricted`, no network
  destinations yet), its folder is named after its id, the id isn't an
  agent's, and every action it declares exists. A bad manifest leaves that
  app uninstalled with a warning; glassd still starts (ADR 0021).
- **Declaration ∩ policy (DV-3, ADR 0008).** `Policy::for_app` gives a
  session the org and group rules for the actions its manifest declares,
  everything else Never, its labels capped at the manifest's most sensitive
  and the user's groups' limit, no models and no network. Before the
  pipeline, glassd also refuses any undeclared action itself, as a result
  that names the manifest ("Calendar doesn't declare mail.send in its
  manifest"), audited as Blocked under the rule `app manifest <id>
  <version>`. Policy still decides the rest: a declared action a group
  makes Never is blocked too.
- **The app socket.** glassd serves `/var/run/glasshouse/apps.sock`,
  authorized by the caller's kernel uid like the control socket. It carries
  only `app.open`, `app.call`, `app.status` and `app.close`
  (`glass_api::app_methods`). Approvals, tasks and undo stay on the control
  socket: an app can never answer its own ask. A session belongs to the
  connection that opened it, no other connection can call through it, and
  it ends when the connection does, withdrawing any open ask (PA-2). A
  closed session that changed nothing leaves only its audit trail.
- **Asks show on the desktop.** An app session is a tile only while it
  needs the user: its ask is a Needs you tile, asked as the app, answered
  on the desktop like any agent's. The call waits meanwhile; `app.status`
  tells the app what the desktop is asking, so it can say so. A decline or
  a timeout is a result the app shows ("Declined on the desktop, so this
  wasn't done"); an app is never paused.
- **Refusals are results.** `app.call` returns `{ok, text, data}`; only a
  malformed request or an unknown session is a protocol error.
- **A user-level bridge between the page and the socket.**
  `glass-app-bridge` runs as the user. It serves each app's built files and
  relays the page's calls, through one session per app; it decides
  nothing. This slice has one mode, `--dev`: loopback HTTP, each app on its
  own origin `http://<app>.localhost:<port>`, so one app's page can't call
  as another (the browser's same-origin policy). Calls must be JSON POSTs
  whose `Origin` is the app's own (a foreign page needs a CORS preflight
  the bridge never answers), the `Host` must name an installed app exactly
  (no DNS rebinding), files come only from the app's `dist/`, and pages
  carry a CSP that allows nothing but the app's files and its bridge.
- **Offline (UI-5).** The SDK keeps each successful read (side-effect-free
  action) in the page's storage, per app, and shows it when the bridge
  can't be reached; a service worker keeps the app's own files. Nothing
  that changes anything is ever queued: offline, sends and new events are
  simply unavailable.
- **The SDK.** `apps/sdk` is the typed client every app uses: the bridge
  calls, the manifest types (in step with `glass_api::apps`), the
  attribution line, and a DOM builder that only inserts text, so mail and
  events can't inject markup.

## What this slice trusts
- **The app socket proves the user, not the app.** The uid is the
  kernel's; the app id in `app.open` is what the connecting process says.
  Any process running as the user can open a session as any installed app,
  including the user's own attended harness (Claude Code through
  `glass-mcp` runs unjailed, as the user). It gets no more than that app's
  manifest at policy's tiers, and nothing the user's uid couldn't already
  do on the control socket, but its calls would read "by mail for demo".
  Jailed agents can't reach the socket at all: `/var/run/glasshouse` is not
  mounted in a jail. The OS version closes the gap with the app runtime:
  glassd admits only the runtime on the app socket (mode 0660, a dedicated
  group the runtime alone holds), and the runtime names the app from the
  installed copy it launched, each app in its own sandboxed renderer.
- **Dev mode trusts loopback.** Anything on the machine that can reach the
  bridge's port acts as any installed app for the user who ran the bridge,
  and the `Host` header names the app. That is acceptable for development
  and tests on a single-user host and nowhere else; the bridge refuses to
  start without `--dev` and refuses non-loopback addresses. In the OS the
  runtime reaches the bridge over the user's own Unix socket, and no TCP
  port exists.
- **Development homes.** With the development host backend, `dev_home`
  puts every user's app data in one folder; glassd refuses it with jails.

## Containment review
- **Reach.** Nothing new inside a jail: no mount, socket or network changes
  in `jail.rs`. The app socket is in `/var/run/glasshouse`, outside every
  jail, so no probe check is added (it would test nothing new).
- **Data leaving.** Apps have no network (`network` must be empty) and no
  model calls (`for_app` allows none). Mail and invitations leave only
  through actions, at Ask by default. The dev bridge listens on loopback.
- **Root acting for an app.** Providers resolve every path under the user's
  home through `ConfinedDir`, as for agents; message and event ids are
  checked to stay inside their folder (`mail.read`, `calendar.read`).
- **Decisions.** Every app call passes the manifest check, then `decide()`,
  labels, asks and snapshots in the shared pipeline; no new side effect
  bypasses it.
- **Identity.** By kernel uid on the app socket, with the app id asserted
  (above). Sessions are bound to their connection, never to an id in the
  request alone.
- **Record.** Opening, every call (completed, blocked, denied, timed out)
  and closing are audited with the app as the agent and the user as the
  user; a refused undeclared call too. Audit failure still fails closed.
- **Lifetime.** A session ends with its connection or `app.close`; the
  supervisor withdraws its asks and records it closed (PA-2).
- **Undo.** App changes are change records with snapshots, like agents'
  (`calendar.create` snapshots first). There's no view yet that lists an
  app session's changes to undo (open question below).

## Consequences
- Tests: `runtime/tests/apps.rs` (attribution, manifest refusals, policy
  still bounding, asks on the desktop and not on the app socket, sessions
  bound to and ended with their connection), `glassd` config tests
  (installing manifests, `dev_home` refused with jails), the bridge's unit
  tests (origins, call checks, file paths), and `make apps-test`: unit
  tests and a Playwright suite against the dev-host glassd, including UI-7's
  budgets.
- Not done: the app runtime (our Chromium build) and the OS-side identity
  above; packaging the apps into the image; a Changes view for app
  sessions; a lower tier for sends inside the team (every send asks).
- Third-party apps will use the same socket and SDK; their manifests will
  come from the catalog (DV-2) rather than the image.
