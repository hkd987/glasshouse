# Vendored crates

Crates the desktop app builds from a patched copy instead of crates.io,
through `[patch.crates-io]` in `agents-app/Cargo.toml`, as the shell does
(`shell/vendor/README.md`). Each patch is marked `Glasshouse patch`.

| Crate | Version | Patch | Why |
|---|---|---|---|
| `dirs-sys` | 0.5.0 (MIT OR Apache-2.0, `LICENSE-MIT`, `LICENSE-APACHE`) | No `option-ext`: its one use, `Option::contains` in `src/xdg_user_dirs.rs`, is a plain comparison | `option-ext` is MPL-2.0, which nothing we ship may carry (`deny.toml`); wry and tray-icon reach it through `dirs` |

To update the copy: replace the directory with the new release from
`~/.cargo/registry/src/*/`, re-apply the patch (search for `Glasshouse
patch` in the old copy), drop `Cargo.lock`, `Cargo.toml.orig` and
`.cargo_vcs_info.json`, and keep the license files.
