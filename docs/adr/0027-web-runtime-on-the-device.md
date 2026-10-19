# 0027. The browser engine on the device: FreeBSD's Chromium until our own build

Status: **Accepted** for the first slice. Supersedes in part 0024: how an
app's page reaches the bridge on the device (item 4). PRD UI-5, CF-15,
CF-16, NV-3, AD-7, BR-1, BR-3, DK-1.

## Context
UI-5 makes Glasshouse apps TypeScript web apps "running on our Chromium
engine"; CF-15 ships our own Chromium-based browser build. Neither exists
yet, so Mail and Calendar were built and tested on the development host
but couldn't open on a device, and the launcher hid them.

FreeBSD's ports build Chromium 154. Taken as a package, it brings its
whole run-time dependency tree: 174 packages and 3.5 GB installed (LLVM
through Mesa alone is 1.8 GB, then Python and Perl), and licenses that
include GPL-3.0. What the engine actually links is much less.

## Decision
1. **The engine is FreeBSD's Chromium package, by its files.**
   `os/freebsd/images/browser.third-party` lists what the image takes: the
   program, its resource packs, US English, its own ANGLE and SwiftShader
   libraries. Below a marker line, `os/freebsd/scripts/third-party.py
   update` writes every shared library those link, from the packages in
   Chromium's dependency tree, until only the base system is left: 57
   packages, 33 MB. NSS loads its crypto modules by name, so the list
   names them by hand and their libraries are filled in too. Updating
   (CF-16) is `third-party.py update --bump`, then the usual checks.
2. **Software drawing, no GPU yet.** `--ozone-platform=wayland
   --disable-gpu`: the engine draws into shared-memory buffers, which our
   pixman compositor composites. No Mesa, no LLVM. GPU rendering waits for
   the compositor's DRM output (ADR 0016).
3. **One engine per person, two profiles, kept running.** Both profiles
   live in one engine data folder (`~/.local/share/glasshouse/engine`):
   `Browser`, and `Apps` for every web app, so browsing never shares an
   app's cookies, storage or extensions, and one set of engine processes
   serves both. A web app is the engine's window on its own origin,
   `--app=http://<app>.localhost:<port>/`. Windows name their app:
   `chrome-<app>.localhost__-Apps` for an app, `chromium-browser` for the
   Browser (`glass_shell::engine`). The engine runs without first-run,
   default-browser or background services, and a new profile is seeded
   with the engine's own window frame off.

   The desktop starts the engine with no window as soon as it has drawn
   its first frame after sign-in (UI-7), held open by its control pipe
   (`--remote-debugging-pipe` on descriptors 3 and 4): the desktop keeps the
   write end and never writes, so the engine runs until the desktop ends
   rather than until its last window closes, and nothing else can drive
   it through that pipe (a debugging *port* would let any local process
   in, so it's never used). Opening an app hands the window to the running
   engine on its socket in the person's engine folder, in the engine's own
   start message (`engine::window_request`), instead of starting a second
   copy of the engine to pass it on; if that fails, the engine is started
   with the window's command line as before. It costs about 90 MB with no
   window. Measurements: `docs/dev/performance.md`.
4. **The bridge on the device: kernel-checked loopback.** ADR 0024 planned
   a Unix socket between the runtime and the bridge; a stock browser
   speaks HTTP only over TCP. So `glass-app-bridge --session`, which the
   desktop starts when the person first opens a web app, listens on
   `127.0.0.1` at their own port, `glass_api::app_bridge_port` (7300 plus
   their uid modulo 1000: fixed per person, because the origin includes
   it and the engine keeps an app's offline data by origin; a device has
   one session at a time). It answers a connection only if the kernel says
   the connecting socket belongs to its own uid
   (`glass_platform::net::loopback_peer_uid`: `sockstat -n`, ADR 0007),
   and closes it unanswered otherwise, including when the owner can't be
   seen. `--dev` stays for development and the tests. Our own build can
   return to a Unix socket and to naming the app from the copy it launched.
5. **Our frame on every window.** The compositor answers xdg-decoration
   with server-side, always: app windows draw no frame of their own, and
   the Browser shows its tabs under our title bar without a second set of
   window controls. New windows are told the room they have
   (`configure_bounds`) and asked to fit if they draw bigger.
6. **Managed policies.** `/usr/local/etc/chromium/policies/managed/
   glasshouse.json`: no browser sign-in, sync, metrics, promotions, guest
   mode, shopping, Labs or translation; HTTPS upgrades on;
   `chrome://version`, `gpu`, `system` and `sandbox`, which name the base
   system, blocked (BR-1). Defaults the person may change are in
   `policies/recommended/`: Memory Saver, tabs restored at sign-in,
   third-party cookies blocked, spelling, downloads to `~/Downloads`
   (`docs/design/browser.md`). The engine's user agent already reads
   Linux, which sites accept. The console sets these per org later (CF-15).
7. **What an image may take from packages.** `tools/legal/notices.py`
   reads every `*.third-party` list, maps ports' license names to SPDX
   ids, and judges each package: permissive licenses ship; LGPL, MPL and
   GPL-2.0 libraries ship unmodified, as separate shared libraries, with
   their source on offer (the source offer names their distribution
   files, BR-3); GPL-3.0 and LGPL-3.0 never, since their
   installation-information terms don't fit devices that run only signed
   software (EP-2). A list may correct a package's catalog line for the
   files it takes, with the reason: libFLAC, libintl and GNU libiconv's
   library are under narrower licenses than their packages' programs.
8. **Fonts.** The engine uses the desktop's own, Geist and Geist Mono
   (`/usr/local/share/fonts/glasshouse`, our `fonts.conf`).

## What it trusts
- **The person's own processes** can act as any of their apps, as in ADR
  0024: the Host header names the app. Nothing another user runs can,
  and agents can't connect at all: their jails have no network, and the
  containment probe checks the bridge's port (`net: reach the app
  bridge`).
- **The kernel's socket table** says who owns a connection. A socket
  handed to another process keeps its first owner.

## Containment review
- **Reach.** Nothing new in a jail. The bridge's port is loopback; jails
  have `ip4` and `ip6` disabled; a probe check proves it (22 of 22).
- **Identity.** By the kernel's owner of the connecting socket, never by
  anything in the request; the app is still the Host header's, within
  the person's own processes.
- **Decisions and record.** Every app call still goes through glassd's app
  socket and the pipeline; refused connections never reach glassd and
  are logged by the bridge.
- **Lifetime.** The bridge and the engine are the person's processes:
  signing out ends them (`users::end_processes`); the engine's control
  pipe closing as the desktop exits ends it too.
- **Control pipe.** Only the desktop holds it; the engine reads commands
  from it and the desktop never sends any. It isn't a new reach: the
  desktop already runs as the person, and the engine's socket for new
  windows is in the person's own engine folder (mode 0700), as before.

## Consequences
- Images grow by about 400 MB (the engine 340 MB, its libraries 33 MB).
  `deploy.sh` sends only what changed, so the VM loop sends it once.
- The engine shows its own name ("Chromium", "Customize Chromium") and
  Google's new tab page until our build (CF-15). Pages in scripts Geist
  doesn't cover show boxes.
- Security updates follow FreeBSD's ports (CF-16) until our build. Our
  pinned ports catalog ages: a package rebuilt upstream is fetched by its
  plain name for the VM, and an image build refuses it until the catalog
  is fetched again and the lists bumped (`ALLOW_REBUILT_PACKAGES=1`
  overrides).
- Not done: our own build (CF-15); enterprise policies and extensions from
  the console; GPU; other languages; a compatibility suite (CF-17). The
  Store (`apps/store`, DV-11) is a web app like Mail and runs the same way.

## Open questions (owner)
1. LGPL, MPL and GPL-2.0 libraries under the source offer: acceptable?
   The GPL-2.0-only ones are `libpci` (the engine's GPU detection; a stub
   of our own could replace it) and `libdbus` (AFL-2.1 or GPL-2.0, shipped
   under the GPL with the offer).
2. Fonts for other scripts: Noto (about 100 MB), or wait for demand?
3. The Browser's start page and search engine: Google's, as now, or ours?
