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
