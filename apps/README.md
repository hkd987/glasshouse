# Apps

Glasshouse's first-party apps are TypeScript web apps (UI-5, ADR 0009). An
app reaches the system only through its actions, which glassd runs through
the same pipeline as an agent's, attributed "by [app] for [user]", and only
the actions its manifest declares (DV-3, ADR 0024). The actions themselves
live in `crates/apps` (`glass-app-mail`, `glass-app-calendar`).

```text
apps/
  sdk/        @glasshouse/sdk: the bridge client, manifest types, attribution,
              a text-only DOM builder, the offline service worker, base styles,
              and the address field that suggests people as they're typed
              (address-field.ts, people.ts: the org's directory through
              people.search, then people from recent mail; AD-1)
  mail/       manifest.json, index.html, src/ (main.ts: the page; mail.ts: logic).
              With IT's IMAP service (ADR 0037): "Connect mail" (mail.connect,
              the person's password, kept by glassd, never by the page); Archive,
              Junk, Delete and Not junk (mail.move); a message's HTML framed from
              view.mail.localhost, which the bridge serves through mail.read
              under a CSP with no scripts and no network (no remote images);
              new mail from app.status waits
  calendar/   manifest.json, index.html, src/ (main.ts; week.ts: the week's logic)
  store/      manifest.json, index.html, src/ (main.ts; store.ts: what the org's
              catalog offers you, DV-10, DV-11)
  settings/   the person's device settings, what IT set or locked (UX-1)
  chat/       the org's chat (CF-91, ADR 0030): glassd seals and opens it; the
              page keeps nothing (bridge.call only, no offline permission) and
              hears of news from app.status waits, ids only. Like Slack (CF-92,
              CF-93, C4a): threads, reactions, edits, pins, topics, mentions,
              members, drafts and levels kept in glassd, and search; chat.ts is
              the logic without a DOM (mentions, the unread divider, summaries)
  artifacts/  pages people and agents share (AR-1 to AR-5, ADR 0033): the list,
              the viewer (each page framed on its own origin,
              <hex>.artifact.localhost, sandboxed, put back when it navigates
              itself), pinned comments drawn by the app from element paths
              (artifacts.ts checks every frame message), and Share; keeps
              nothing, like Chat
  docs/       docs people write together, live (DC-1 to DC-4, ADR 0035):
              ProseMirror bound by y-prosemirror to the doc's Yjs document,
              in the schema glassd's yrs reads (schema.ts); the live session
              over /_bridge/live and comments' anchors without a DOM
              (live.ts, docs.ts); keeps nothing, like Chat. The one app built
              on npm libraries (ProseMirror, Yjs: all MIT)
  sdk/template/ where a new app starts (ADR 0032): a manifest without an
              id and a small src/; apps.source {"template": true} hands it out
  console/    the admin console's web app (ADR 0023): a site glass-console serves,
              not a device app (no manifest, no service worker)
  e2e/        Playwright: the apps against dev-host glassd and the bridge;
              console.config.ts: the console against glass-console and glassd,
              and Chat and Artifacts with a colleague scripted on another
              computer (crates/apps/chat/examples/colleague.rs)
  build.mjs   esbuild: each app into <app>/dist (app.js, app.css, sw.js)
```

## Apps people make (ADR 0032)

Every app here ships its `src/` in images (in a folder of their own,
`/usr/local/share/glasshouse/source`, which glassd removes when IT turns
remixing off, TL-6), and people remix it, usually by
asking their agent: `apps.bases` lists what they can start from,
`apps.source` hands out an app's files, `apps.save` checks, bundles and
keeps their version (`crates/apps/studio`). Their app's id starts `my-`,
it has its own origin, the bridge serves it from glassd (`app.files`), and
it does only what its maker's policy allows, never with its base's own
rules. So keep our apps' source readable: it is what customers start from,
and `glass-app-studio`'s `tl5_first_party_apps_pass_their_own_checks`
fails when one stops passing the checks a remix of it gets (no `innerHTML`,
`fetch`, storage of its own, workers or action names it doesn't declare;
Meet's worker is the known exception).

## Running them

```sh
make dev-host      # glassd on this machine (insecure host backend)
make apps-dev      # in another shell: the bridge, serving the built apps
# http://mail.localhost:7300 and http://calendar.localhost:7300
```

Each app has its own origin, `http://<app>.localhost:<port>`, so one app's
page can't call another's bridge. The bridge (`glass-app-bridge --dev`)
relays a page's calls to glassd's app socket; glassd decides everything.
Asks wait for you on the desktop (`glassctl approvals`, `glassctl approve
<id>`); the app shows what it's waiting for.

Dev mode trusts anything on this machine that can reach the bridge's
loopback port.

## On the device

Images carry each app's manifest and `dist/` in
`/usr/local/share/glasshouse/apps/<app>` (`stage-dist.sh`, after `make
apps-build`) and the browser engine (ADR 0027). The launcher and the dock
open an app as the engine's window on its own origin; the desktop starts
`glass-app-bridge --session` for the person, on their own port
(`glass_api::app_bridge_port`), and it answers only connections from their
own processes. `make freebsd vm-deploy` puts all of it in the dev VM.

## Checks

| Command | What it does |
|---|---|
| `make apps-build` | esbuild, minified, one script and one stylesheet per app |
| `make apps-test` | types; unit tests (`node --test`, `*/test/*.test.ts`); Playwright in headless Chromium against dev-host glassd, with the launch and memory budgets (UI-7) |
| `make apps-lint` | types, every manifest as glassd will check it, and every npm package's license (`tools/legal/npm_licenses.py apps`) |

`make lint` runs `apps-lint` when anything under `apps/` changes. The
browsers are the machine's (`PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers`);
never `playwright install`. If the installed Chromium doesn't match
`@playwright/test`, set `PW_CHROMIUM=/opt/pw-browsers/chromium`.
`SHOTS_DIR` keeps the suite's screenshots somewhere other than
`e2e/results/shots`.

## The admin console

The org's admin console (AD-6, ADR 0023) is a web app on our servers, not
on a device: `glass-console` (`crates/bin/glass-console`) serves
`console/dist`, the admin API and the API devices sync signed policy and
catalog releases from. Pages: Policies (per group: tiers by action class,
allowed agents, apps in profiles, labels and models, versions and rings,
preview impact, publish, promote, roll back, live numbers), Catalog (what
an item declares; approve per group as required, available or blocked;
block), Settings (device settings per group, a default or locked, and
the order groups win in), Mail (the org's mail service), Remote help (support tiers: who
holds each and its tools) and Devices (reports, rings, enrollment). Support
staff, the members of the groups a support role names, see only Support:
their tools and the computers their role reaches (ADR 0028).

```sh
make console-dev FIXTURE=500,50       # http://127.0.0.1:7400, sign in as an IT admin or support
GLASS_DEV_ENROLL=build/console/enrollment.json make dev-host   # glassd as device dev-host
```

`glass-console --dev` keeps its state in `build/console` (the org's key at
mode 0600, `directory.toml` for people and groups, policy versions, the
catalog, the fleet) and signs everyone in by picking one of the directory's
`it-admins` or support staff (jordan, alex and sasha in the starter org);
our web servers will add TLS, the org's SSO and a managed
database. The page is served with a CSP that allows only its own files and
API; changes are JSON POSTs from its own origin with a SameSite=Strict
session cookie.

| Command | What it does |
|---|---|
| `make console-test` | the console's Rust tests, then Playwright (`e2e/console.config.ts`) on a 500-device, 50-item fixture and a dev-host glassd enrolled with it: every page interactive in under 1 s, a published change in force on the device in under 5 s, an approved item found by `catalog.search`, and a remix of Mail shared, reviewed, approved and installed from the Store (`studio.e2e.ts`: the share in under 15 s, approval to the computer in under 5 s) |
| `make console-lint` | clippy on the server, types, and no base-system name in what IT sees (BR-1); the console's npm packages are the apps' (`make apps-lint` checks their licenses) |

## Rules

- **Small and fast.** Vanilla TypeScript and esbuild; no framework. An app
  launches cold in under 1 s and stays under 150 MB (UI-7), and the suite
  fails when it doesn't.
- **Text, never markup.** Mail bodies, subjects, event titles and anything
  else from outside goes into the page with `h()` and `show()` from the SDK,
  which only create text nodes. Pages are served with a CSP that allows
  nothing but the app's own files and its bridge.
- **Refusals are results.** `bridge.call()` returns `{ok, text, data}`; show
  the text. Only an unreachable system throws (`Offline`).
- **Offline is read-only.** Use `bridge.read()` for side-effect-free actions:
  what it loaded is shown when the system can't be reached. Never queue a
  change.
- **Attribution inline (AU-10).** Whatever an agent wrote shows its "by
  [agent] for [user]" line (`attributionChip`); what the user wrote through
  the app doesn't (`showAttribution`).
- **Dependencies** are permissively licensed (the allow list in
  `deny.toml`) and pinned in `package-lock.json`. Our packages are private.
- Reference PRD ids in test names (`ui5_…`, `au10_…`), as in the Rust code.
