# Apps

Glasshouse's first-party apps are TypeScript web apps (UI-5, ADR 0009). An
app reaches the system only through its actions, which glassd runs through
the same pipeline as an agent's, attributed "by [app] for [user]", and only
the actions its manifest declares (DV-3, ADR 0024). The actions themselves
live in `crates/apps` (`glass-app-mail`, `glass-app-calendar`).

```text
apps/
  sdk/        @glasshouse/sdk: the bridge client, manifest types, attribution,
              a text-only DOM builder, the offline service worker, base styles
  mail/       manifest.json, index.html, src/ (main.ts: the page; mail.ts: logic)
  calendar/   manifest.json, index.html, src/ (main.ts; week.ts: the week's logic)
  console/    the admin console's web app (ADR 0023): a site glass-console serves,
              not a device app (no manifest, no service worker)
  e2e/        Playwright: the apps against dev-host glassd and the bridge;
              console.config.ts: the console against glass-console and glassd
  build.mjs   esbuild: each app into <app>/dist (app.js, app.css, sw.js)
```

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
loopback port; see ADR 0024 for what the OS version will trust instead.

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
block) and Devices (reports, rings, enrollment).

```sh
make console-dev FIXTURE=500,50       # http://127.0.0.1:7400, sign in as an IT admin
GLASS_DEV_ENROLL=build/console/enrollment.json make dev-host   # glassd as device dev-host
```

`glass-console --dev` keeps its state in `build/console` (the org's key at
mode 0600, `directory.toml` for people and groups, policy versions, the
catalog, the fleet) and signs everyone in by picking one of the directory's
`it-admins`; our web servers will add TLS, the org's SSO and a managed
database. The page is served with a CSP that allows only its own files and
API; changes are JSON POSTs from its own origin with a SameSite=Strict
session cookie.

| Command | What it does |
|---|---|
| `make console-test` | the console's Rust tests, then Playwright (`e2e/console.config.ts`) on a 500-device, 50-item fixture and a dev-host glassd enrolled with it: every page interactive in under 1 s, a published change in force on the device in under 5 s, an approved item found by `catalog.search` |
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
