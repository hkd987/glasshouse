# Dependencies and licensing

Glasshouse is proprietary. Everything we ship links only permissively
licensed third-party code.

- **Allowed**: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode, CC0. The
  authoritative list is `deny.toml`.
- **Not allowed** in shipped code: GPL, LGPL, AGPL, MPL, EUPL, SSPL, or any
  license with source-disclosure or network-use terms. Ask before adding
  anything outside the allow list.
- **FreeBSD** is BSD-licensed; we build on unmodified release images and
  contribute fixes upstream (PA-5).

Before adding a crate:

1. Prefer the standard library, then a crate already in the workspace.
2. Check its license, maintenance and `unsafe` footprint.
3. Add it to `[workspace.dependencies]` with an exact minor version, never a
   wildcard, and depend on it with `.workspace = true`.
4. Run `make deny`.

Crates that compile C need the FreeBSD cross compiler; see
`os/freebsd/README.md`. We currently have none, and should keep it that way
where we can: pure-Rust dependencies cross-compile and audit more easily.
