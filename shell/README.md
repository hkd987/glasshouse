# Desktop shell

The Glasshouse desktop (PRD UI-4, ADR 0009): boot screen, sign in, the
desktop with its menu bar, dock and agent tiles, and the command bar, drawn
to match the mockups (`docs/design/`). It is a control-API client like
glassctl: everything it shows comes from `glass_api` types (`TaskView`,
`ApprovalView`), and `--socket` renders a running glassd's real tiles.

M1 renders headlessly to PNG. M2 puts the same views on a screen: first
straight onto the console framebuffer (`glass-shell-fb`, ADR 0016), then
inside the compositor (below).

## Run it

```sh
make shell-preview                 # build/shell-preview/{boot,login,desktop,command-bar,expanded*}.png
make shell-preview SOCKET=/var/run/glasshouse/control.sock   # live tiles from glassd (task.list)
make shell-test                    # unit tests, landmarks, snapshot digests
make shell-lint                    # fmt check and clippy -D warnings
make shell-vm                      # the desktop on the dev VM's screen, live tiles
make greeter-vm                    # sign in on the dev VM's screen (demo / demo), then the desktop
os/freebsd/scripts/vm.sh screendump desktop.png   # see it
```

`make shell-vm` cross-builds `glass-shell-fb`, copies it into the running
VM and starts it on the first virtual terminal as `demo`
(`os/freebsd/scripts/shell-vm.sh start|stop|once [user]`). It maps the
console framebuffer (`glass_platform::framebuffer`), draws the user's
tiles, and redraws when glassd reports a change (`events.subscribe`), the
person clicks or types, or the menu bar clock turns over, copying only the
rows that changed. Keyboard and pointer come from evdev
(`glass_platform::input`). Agent tiles show as banners for a few seconds
when something happens (a new agent, an ask, an ending) and then go, unless the pointer is on
them; the menu bar's agents pill opens the agents menu with every agent,
and the mark at its left the system menu (Hand an agent a task, Sign
out). With no app in front, the mark is all the menu bar shows on the
left. Buttons and menu
items answer the pointer as it moves, redrawing only when one changes. Approve on a
needs-you banner or menu tile answers the ask;
clicking a tile, or its Review, opens the expanded view (`task.get`, fetched
again whenever that task changes) with Pause, Stop, Take over, the ask's
answers and Roll back (`task.undo`, a dry run first); Ctrl Space opens the
command bar; Esc closes the command bar, then the agents menu, then the
expanded view. If glassd isn't answering (still starting, or restarting
after an update or a crash) the desktop stays up and tries it again every
half second, then reloads the tiles. Its log is
`/var/log/glass-shell-fb.log` in the VM.

Images ship `glass-greeter` (ADR 0019) and `glass-shell-fb` with the rest
(`os/freebsd/dist.manifest`, built by `make freebsd`), and `post-install.sh`
turns on `rc.d/glasshouse_greeter`, so a machine, and the dev VM after
`make freebsd vm-deploy`, boots into the sign-in screen. `make greeter-vm`
installs debug builds instead: the sign-in screen on the
first virtual terminal, PAM checks the password (`demo` on development
VMs), and the desktop starts as that person through `su -l`. Shift Super Q
signs out back to the sign-in screen. The greeter stays on across reboots
until `os/freebsd/scripts/shell-vm.sh greeter-off`; `make shell-vm` stops
it while the desktop runs directly. Its log, the desktop's included, is
`/var/log/glass-greeter.log`.

Drive it from the host through QEMU's USB tablet and keyboard:

```sh
os/freebsd/scripts/vm.sh click 1150 172        # a screen pixel
os/freebsd/scripts/vm.sh key ctrl-spc          # QEMU key names
os/freebsd/scripts/vm.sh type "Prep me for my 2pm"
os/freebsd/scripts/vm.sh screendump shot.png
```

`glass_shell::live::Live` holds the desktop's state and turns messages
into control-API calls (`Effect`), and takes their answers back
(`Live::detail` for `task.get`, `Live::undo_report` for `task.undo`);
`glass_shell::render::Screen` keeps the UI's state (focus, held buttons,
scroll positions) between frames. `tests/live.rs` clicks and types through
them headlessly.

`glass-shell-preview --scale 2` renders at 2x. The snapshot test keeps an
FNV digest per screen in `crates/glass-shell/tests/snapshots/`; after an
intended visual change, look at the new PNGs, then re-bless with
`GLASS_SHELL_BLESS=1 make shell-test`. A mismatch writes the new frame
under `shell/target/tmp/` so you can compare it.

## Layout

| Path | What |
|---|---|
| `crates/glass-shell/src/theme.rs` | `Theme` (UX-10): every color, the wallpaper recipe. Screens take colors from it, never literals for themeable parts |
| `crates/glass-shell/src/screens/` | `boot`, `login`, `desktop` (menu bar, dock, windows, banners and the agents menu), `command_bar`, `expanded` (a tile opened: header controls, activity, changes, timeline and roll back, AW-10 to AW-16) |
| `crates/glass-shell/src/tiles.rs` | An agent tile from a `TaskView` (AW-1 to AW-4) |
| `crates/glass-shell/src/render.rs` | Rendering: a view to an RGBA frame or PNG, once, or frame after frame redrawing only what changed (`Screen`) |
| `vendor/` | Crates built from a patched copy (`vendor/README.md`) |
| `crates/glass-shell/src/live.rs` | The live desktop's state and what clicks and keys ask of glassd |
| `crates/glass-shell/src/icons.rs`, `wallpaper.rs` | Icons from the mockups' SVG paths and the blurred wallpaper, rasterized with tiny-skia |
| `crates/glass-shell/src/fixtures.rs` | The mockups' content, for previews and tests |
| `crates/glass-shell-preview` | The PNG renderer binary |
| `crates/glass-shell-fb` | The desktop on the console framebuffer, with live tiles, keyboard and pointer (M2 step 1) |
| `assets/fonts/` | Geist and Geist Mono, with their licenses |

## Why a separate workspace

The UI toolkit is about 120 crates. Keeping it out of the root workspace
means `make build`, `make test`, `make lint` and the FreeBSD cross-builds of
glassd and friends never compile or wait on it. The shell depends on the
core's `glass-api`, `glass-types`, `glass-rpc` and (for the screen)
`glass-platform` by path, so an API change
that breaks it shows up in `make shell-test`. Its build output goes to
`shell/target/`.

Build times (4 cores, debug): clean about 2 min 50 s, mostly dependencies,
which build once at `opt-level = 2` so a whole 1280x800 frame renders in
about 60 ms even in debug; an edit to `glass-shell` rebuilds in about 1 s.

## Redrawing fast

A live screen (`render::Screen`) keeps its pixels and redraws only what
changed: it diffs the frame's layers against the last one (iced's damage
diff, extended to cover drop shadows), redraws each changed area on its
own, and keeps only that area. The wallpaper is the screen's backdrop
(`Screen::set_backdrop`, `wallpaper::backdrop`), drawn once, so a small
change doesn't scale a full-screen image again. Icons are rasterized once
per color and size. `shell/vendor/iced_tiny_skia` carries one patch: a
drop shadow is computed only inside the area being redrawn. A clock tick
costs 1.8 ms on a laptop and a keystroke 5 ms; numbers and the VM's are in
`docs/dev/performance.md`. `tests/live.rs` checks that redrawing only the
changed areas gives the pixels a full redraw gives, and `tests/redraw_cost.rs`
measures it (`-- --ignored --nocapture`).

## Dependencies and licenses

iced 0.14 with only the tiny-skia software renderer: `iced_core`,
`iced_widget` (`image`), `iced_runtime`, `iced_renderer` (`tiny-skia`,
`image`, no defaults) and `iced_tiny_skia` (no defaults). No wgpu, no
winit, no system font lookup: the bundled fonts are the only ones loaded,
so renders are the same on every machine. `iced_tiny_skia` always links
softbuffer, which needs one backend to compile; the shell enables only
`kms` (DRM dumb buffers).

`deny.toml` is a link to the root one; check with
`cargo deny --manifest-path shell/Cargo.toml check licenses bans sources`.

Geist and Geist Mono are under the SIL Open Font License 1.1
(`assets/fonts/OFL-*.txt`), shipped unmodified. When the shell ships,
pass `--assets shell/assets/fonts` to `tools/legal/notices.py` so the
font licenses land in the image's notices next to the crates'.

## Next: M2, inside the compositor

1. **Present.** A Smithay-based compositor owns the display on the device
   (DRM/KMS, libinput) and draws the shell as its desktop layer: the same
   `tiny-skia` frames, presented through dumb buffers (or a pixman
   renderer), so no GPU driver is required.
2. **Live state.** Done in `glass-shell-fb`: it subscribes to
   `events.subscribe` and redraws tiles, the agents pill and the clock as
   they change.
3. **Input.** Started in `glass-shell-fb` from evdev: Ctrl Space opens
   the command bar, Approve sends `approval.answer`, a tile opens its
   expanded view with its controls and roll back. Next: sign in talks to
   the session manager; then libinput and xkbcommon through the
   compositor.
4. **Agent screens.** Screen agents' virtual displays as tile thumbnails
   (UI-3), replacing the placeholder.
5. **Accessibility and themes.** AccessKit for screen readers; a theme
   picker over `Theme`.
