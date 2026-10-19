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
  e2e/        Playwright: the apps against dev-host glassd and the bridge
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
