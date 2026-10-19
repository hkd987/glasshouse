# Vendored crates

Crates the shell builds from a patched copy instead of crates.io, through
`[patch.crates-io]` in `shell/Cargo.toml`. Each patch is small, marked
`Glasshouse patch` in the source, and worth offering upstream; when upstream
has it, delete the copy and the `[patch]` entry.

| Crate | Version | Patch | Why |
|---|---|---|---|
| `iced_tiny_skia` | 0.14.1 (MIT, `LICENSE`) | `src/engine.rs`, `draw_quad`: a quad's drop shadow is computed once, kept (`Engine::shadows`, up to 16 MiB, keyed by everything that shapes it), and only its part inside the clip is blended in | Upstream computes every pixel of a shadow, and draws it unclipped, whenever the quad meets an area being redrawn. A keystroke in the command bar (a 64 px shadow) cost 42 ms on a laptop and 1.7 s in the VM; redrawing a small area must cost what the area costs, and a whole-screen redraw spent a sixth of its time on shadows that hadn't changed |

To update a copy: replace the directory with the new release from
`~/.cargo/registry/src/*/`, re-apply the patch (search for `Glasshouse
patch` in the old copy), drop `Cargo.lock` and `Cargo.toml.orig`, and keep
the license file.
