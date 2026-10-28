# Glasshouse Agents desktop app

The Agents web app (`apps/agents`) in a small native window for Mac and
Windows, plus what a browser can't do for it (ADR 0065 §12, ADR 0071):

- **Sign in** as `glass login` does (ADR 0067). The person's browser does
  the company's sign-in, and the app keeps the token in the system's
  keychain: the Keychain on a Mac, the Credential Manager on Windows, and
  the kernel's keyring on Linux (development).
- **The web app, built in.** The app serves it from its own `agents:`
  scheme and passes its API calls on with the token. The page never sees
  the token.
- **Who needs you.** The tray shows how many questions wait. The system's
  notifications say when an agent asks (the ask's summary only) or
  finishes.
- **Sign out** from the window's menu or the tray. As `glass logout` does,
  every token of the person's stops working.

It is its own Cargo workspace, like `shell/`, so the core never compiles a
webview. It is built on Tauri's engine (wry, tao, tray-icon, muda) without
the Tauri framework.

## Run it

```sh
make agents-app-deps          # Linux: WebKitGTK and the tray's libraries
make agents-app               # builds the web app, then the app, which embeds it
target/debug/glass-agents-app --url https://agents.example.com
```

Against the local AWS stand-in (`make aws-local`, then an agent host behind
its load balancer), the address is `http://127.0.0.1:7613`. Plain HTTP is
accepted only for this computer's own addresses.

- `--web apps/agents/dist` serves the web app from disk, for working on it.
- `GLASS_AGENTS_URL` sets the address as `--url` does.
- `GLASS_AGENTS_APP_BROWSER=<program>` hands the sign-in page to another
  program than the system's browser, for tests.

The address the person types is kept in `settings.json` in the app's own
folder:

| System | Folder |
|---|---|
| Mac | `~/Library/Application Support/Glasshouse Agents` |
| Windows | `%APPDATA%\Glasshouse Agents` |
| Linux | `~/.config/Glasshouse Agents` |

## Test it

```sh
make agents-app-test agents-app-lint
```

Under Xvfb, `xdotool` drives the window and `import -window root` takes
screenshots. A Chromium driven by Playwright plays the browser's part:
the company's sign-in, then Allow.

On a Linux test machine, start every run of the app in one session keyring,
as one login's programs share theirs. A shell that has no session keyring
won't find a sign-in kept by an earlier run.

## Layout

```text
crates/glass-agents-app/
  build.rs        embeds apps/agents (index.html, dist/app.js, dist/app.css)
  src/main.rs     the window, its menu, the tray, following the person's events
  src/web.rs      the agents: scheme: the web app, the API passed on, the forms
  src/pages.rs    the app's own pages: the address, sign in, waiting for the browser
  src/session.rs  the address (settings file) and the token (keychain)
  src/watch.rs    what's news: a new ask, an agent done; the tray's words
vendor/           patched crates (vendor/README.md)
```
