# 0071. Glasshouse Agents' desktop app

Status: **Accepted, built** (2026-10-28). The owner: "The agents desktop
client should have been built". ADR 0065 §12 planned it, and ADR 0067
said how it signs in.

## Context
- ADR 0065 §12 named three ways in to a person's agents: the web app,
  the desktop app and the `glass` CLI. Only the web app and the CLI were
  built.
- The plan for the desktop app:
  - the same web app, in a small native window, for Mac and Windows;
  - a menu-bar icon that says who needs you, and notifications;
  - the sign-in kept in the system's keychain;
  - "the app adds only what a browser can't", on Tauri.
- ADR 0067: it signs in as `glass login` does. The browser does the
  company's sign-in, and a one-time code comes back to this computer and
  is traded for a token.
- Not this ADR: the Glasshouse OS desktop routing its own agents to the
  person's cloud computer. That is ADR 0068's "later integration".

## Decision
1. **Tauri's engine, not its framework.**
   - wry (the system's webview: WebView2, WKWebView, WebKitGTK), tao (the
     window), tray-icon and muda (the tray and menus), notify-rust, and
     keyring-core with each system's store.
   - Nothing yet needs the framework's IPC, plugins or bundler. Without
     them there is far less to build, and no generated configuration.
   - Its own Cargo workspace, `agents-app/`, like `shell/`, so the core
     never compiles a webview. It shares `target/`.
2. **The web app, built in.**
   - `build.rs` embeds `apps/agents`.
   - The app serves it from a scheme of its own: `agents://localhost`, or
     `http://agents.localhost` where WebView2 serves custom schemes. The CSP
     is the front's own.
   - One interface for the web, the desktop and later systems, as ADR 0065
     asked.
3. **Its calls go through the app, with the token.**
   - The scheme's handler passes `/api/v1/*` to the person's Glasshouse
     Agents with `Authorization: Bearer`. Each call runs on a thread of its
     own, since the events long poll waits up to 25 seconds.
   - The answer comes back as it is, so the page shows the front's own
     words.
   - The page never holds the token, and there's no cookie and no second
     sign-in inside the webview.
   - A 401 signs the app out, back to the sign-in page saying why.
4. **Signing in, shared with `glass`.**
   - `glass_gateway_providers::agents::SignIn` holds the loopback wait,
     the state check and the code's trade. `glass login` and the app both
     call it.
   - The system's browser does the company's sign-in, never the app's
     webview. Passkeys, device checks and the company's own sign-in page
     are the browser's.
   - The front's page names who asks (`app=desktop`): "Let the Glasshouse
     Agents app on <computer> use your agents?". It accepts no other value.
   - The token goes into the system's keychain:
     - **Mac:** the Keychain.
     - **Windows:** the Credential Manager.
     - **Linux (development):** the kernel's keyring.

     There is one entry per Glasshouse Agents address.
5. **The address.**
   - IT sets it when it installs the app, with `--url` or
     `GLASS_AGENTS_URL`.
   - Otherwise the first page asks for it, and the app's settings file
     keeps it. That's the only thing the file keeps.
   - HTTPS only, except this computer's own addresses (development).
6. **What a browser can't do.**
   - **Tray.** Its icon gets an orange dot, its tooltip (a title on a Mac)
     says how many questions wait, and its menu offers Open, the count,
     Sign out and Quit.
   - **Notifications.** One when an agent asks, which says only the ask's
     summary, since its detail can be confidential (labels, ADR 0018).
     One when an agent finishes.
   - **Window menu** (Mac and Linux): who is signed in, Sign out and Quit,
     plus the Edit menu a Mac needs for typing.
   - **Sign out** does what `glass logout` does: every token of the
     person's stops working, then the keychain entry goes.
   - **Closing** the window hides it to the tray on a Mac or Windows.
   - The app follows the person's events as the web app does. After each
     change it looks at the questions waiting and the agents' states
     (`watch.rs`).
7. **Links elsewhere open in the browser.** The webview loads only the
   app's own scheme, and new windows are refused.
8. **Licenses.** wry and tray-icon reach `option-ext` (MPL-2.0) through
   `dirs-sys`. The app builds a patched `dirs-sys` without it
   (`agents-app/vendor/README.md`). Every crate on Mac, Windows and Linux
   is under a license `deny.toml` allows, and `agents-app/deny.toml`
   checks those targets.

## Tested
- **Unit tests:**
  - `glass_gateway_providers::agents`: the app and `glass` sign in, a
    wrong state is waited past, the front's refusal comes back in words,
    a quiet browser times out, plain HTTP is refused.
  - The front: `app=desktop` names the app on the page, and an odd value
    names nothing new.
  - The app's own (`make agents-app-test`): its routes, a form's fields,
    the keychain kept and forgotten, an old token's refusal not signing
    out a newer sign-in, the address rules, pages never showing markup,
    what's news, the tray's words, the mark.
- **By hand** (`agents-app/README.md`). The app ran under Xvfb against the
  local AWS stack's load balancer and an agent host on Claude Haiku, with
  Chromium playing the browser:
  - first run, a plain-HTTP address refused, sign in, waiting for the
    browser, the consent page naming the app;
  - signed in, and reopened straight into the agents from the keychain;
  - an ask that wakes a sleeping computer, the approval card, the "needs
    you" notification, approve, the "done" notification;
  - take over and hand back;
  - sign out, which left no key in the keyring.

## Found while building it
- WebKitGTK follows neither a custom scheme's redirect nor a refresh page
  after a form's post. Each of the app's forms has the window load its
  root itself.
- In a shell that has no session keyring of its own, a key that an earlier
  run kept isn't found. A Linux login session has one, which its programs
  share; tests start every run of the app in one named session keyring.

## What it doesn't do yet
- **Mac and Windows builds aren't made here.** Each system's code is in
  place (`cfg` branches for the keychain, the menus and the webview), but
  this environment builds and runs Linux only.
  - The agents client is in glass-gateway-providers, which links
    glass-crypto's OpenSSL. So a Mac build needs OpenSSL (Homebrew), and a
    Windows build needs vcpkg's.
  - Moving the client to a crate without OpenSSL comes before the
    installers.
- **CI doesn't build it.** `make ci` would need WebKitGTK on its hosts
  (`make agents-app-deps`) before `agents-app-test agents-app-lint` can
  join it. Until then, `make lint` checks the app wherever WebKitGTK is
  installed.
- **Installers, signing, notarization and updates.** Tauri's bundler and
  updater, when the app takes on the framework.
- **IT's managed settings** for the address (a Mac profile, Group Policy).
  Today IT uses the flag or the variable at install, or the settings file.
- **Dropping a file on an agent.** The front has no route to put files on
  a computer yet (`glass cp`, ADR 0065 §12).
- **Linux:** the kernel's keyring keeps the token until logout or reboot.
  A Linux desktop build would use the Secret Service.
- **Not seen working:** the tray and notifications rendering. Xvfb has
  neither a tray host nor a notification service. Their logic is
  unit-tested, and the app logs each notification it sends.
- **Clicking a notification to open the agent.** notify-rust's actions
  work on Linux only.
