# Dependencies and licensing

Glasshouse is proprietary. Everything we ship links only permissively
licensed third-party code.

- **Allowed**: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode, CC0. The
  authoritative list is `deny.toml`.
- **Not allowed** in shipped code: GPL, LGPL, AGPL, MPL, EUPL, SSPL, or any
  license with source-disclosure or network-use terms. Ask before adding
  anything outside the allow list.
- **Servers we run** for customers, hosted or shipped to their own AWS
  account (ADR 0037): a third-party program beside our code, outside the
  workspace, is permissively licensed, or copyleft without network-use
  terms (EPL, LGPL, GPL-2.0) run unmodified as its own program with a
  source offer when it ships. Never AGPL, SSPL, or a license that forbids
  redistribution.
- **FreeBSD** is BSD-licensed; we build on unmodified release images and
  contribute fixes upstream (PA-5).

Before adding a crate:

1. Prefer the standard library, then a crate already in the workspace.
2. Check its license, maintenance and `unsafe` footprint.
3. Add it to `[workspace.dependencies]` with an exact minor version, never a
   wildcard, and depend on it with `.workspace = true`.
4. Run `make deny`.

Known problems in what we already depend on are a separate check, `make
advisories` (the RustSec database for the three Cargo workspaces, npm's for
the web apps). `.github/workflows/advisories.yml` runs it daily and when a
lockfile or `deny.toml` changes, apart from `ci`, so that news about
yesterday's dependencies doesn't fail an unrelated change. When it fails:

- a vulnerability or a yanked release: move to the fixed version (`cargo
  update -p <crate>` in that workspace), or replace the crate;
- a crate its author has retired, with no known vulnerability: if we depend
  on it ourselves, replace it. If something we depend on brings it, it
  leaves when that does: add it to `ignore` in that workspace's `deny.toml`
  with what brings it and what would remove it, and take the entry out when
  the crate is gone from the lockfile.

A failing advisories run is someone's to look at the day it fails: it is
not part of `ci`, so nothing else will stop for it.

The three workspaces each have a lockfile (`Cargo.lock`, `shell/Cargo.lock`,
`agents-app/Cargo.lock`), and the shell and the Agents app build on the main
workspace's crates: a dependency added there changes their lockfiles too.
`make lint` checks all three are current when a manifest or a lockfile
changes (`cargo update --workspace --locked`); to bring one up to date, run
any cargo command in that workspace, such as `cargo metadata
--manifest-path agents-app/Cargo.toml`, and commit the lockfile.

What ships is listed for users and reviewers from the build itself:
`tools/legal/notices.py` writes the image's `NOTICES.txt` (BR-2) during
staging and fails on a license outside the allow list; `make sbom` writes the
SBOM security teams get under NDA (BR-4). Neither is ever edited by hand.

Crates that compile C need the FreeBSD cross compiler; see
`os/freebsd/README.md`. We have none, and should keep it that way where we
can: pure-Rust dependencies cross-compile and audit more easily. The one C
library we link is the base system's OpenSSL, for the model gateway's TLS
(ADR 0018): `openssl-sys`'s build script only reads the version from the
sysroot's headers, so FreeBSD builds and the FreeBSD clippy need
`make freebsd-sysroot`, and host builds need `libssl-dev` and `pkg-config`.
Only `glass-gateway-providers` may depend on `native-tls`, `openssl` or
`httparse`; nothing else gets an HTTP or TLS stack. The meetings relay's
WebRTC library, str0m (ADR 0030), runs its DTLS on the same OpenSSL; only
`glass-media-relay` may depend on it, and it never ships in the image.
