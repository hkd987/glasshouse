# Desktop shell

The Glasshouse desktop (PRD UI-4, ADR 0009): boot screen, sign in, the
desktop with its menu bar, dock and agent tiles, and the command bar, drawn
to match the mockups (`docs/design/`). It is a control-API client like
glassctl: everything it shows comes from `glass_api` types (`TaskView`,
`ApprovalView`), and `--socket` renders a running glassd's real tiles.

M1 renders headlessly to PNG. There is no window yet; M2 runs the same
views inside the compositor (below).

## Run it

```sh
make shell-preview                 # build/shell-preview/{boot,login,desktop,command-bar}.png
make shell-preview SOCKET=/var/run/glasshouse/control.sock   # live tiles from glassd (task.list)
make shell-test                    # unit tests, landmarks, snapshot digests
make shell-lint                    # fmt check and clippy -D warnings
```

`glass-shell-preview --scale 2` renders at 2x. The snapshot test keeps an
FNV digest per screen in `crates/glass-shell/tests/snapshots/`; after an
intended visual change, look at the new PNGs, then re-bless with
`GLASS_SHELL_BLESS=1 make shell-test`. A mismatch writes the new frame
under `shell/target/tmp/` so you can compare it.

## Layout

| Path | What |
|---|---|
| `crates/glass-shell/src/theme.rs` | `Theme` (UX-10): every color, the wallpaper recipe. Screens take colors from it, never literals for themeable parts |
| `crates/glass-shell/src/screens/` | `boot`, `login`, `desktop` (menu bar, dock, windows, tile stack), `command_bar` |
| `crates/glass-shell/src/tiles.rs` | An agent tile from a `TaskView` (AW-1 to AW-4) |
| `crates/glass-shell/src/render.rs` | Headless rendering: a view to an RGBA frame or PNG |
| `crates/glass-shell/src/icons.rs`, `wallpaper.rs` | Icons from the mockups' SVG paths and the blurred wallpaper, rasterized with tiny-skia |
| `crates/glass-shell/src/fixtures.rs` | The mockups' content, for previews and tests |
| `crates/glass-shell-preview` | The PNG renderer binary |
| `assets/fonts/` | Geist and Geist Mono, with their licenses |

## Why a separate workspace

The UI toolkit is about 120 crates. Keeping it out of the root workspace
means `make build`, `make test`, `make lint` and the FreeBSD cross-builds of
glassd and friends never compile or wait on it. The shell depends on the
core's `glass-api`, `glass-types` and `glass-rpc` by path, so an API change
that breaks it shows up in `make shell-test`. Its build output goes to
`shell/target/`.

Build times (4 cores, debug): clean about 2 min 50 s, mostly dependencies,
which build once at `opt-level = 2` so a 1440x900 frame renders in
40–330 ms even in debug; an edit to `glass-shell` rebuilds in about 1 s.

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
2. **Live state.** Subscribe to `events.subscribe` and redraw tiles,
   the agents pill and the dock badge as tasks change, instead of one
   `task.list`.
3. **Input.** Keyboard and pointer from the compositor into iced events:
   Ctrl Space opens the command bar, Approve sends `approval.answer`,
   opening a tile goes to `task.get` and its activity, sign in talks to the
   session manager.
4. **Agent screens.** Screen agents' virtual displays as tile thumbnails
   (UI-3), replacing the placeholder.
5. **Accessibility and themes.** AccessKit for screen readers; a theme
   picker over `Theme`.
