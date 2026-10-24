# Keeping builds fast

Rust builds get slow when a workspace grows without anyone watching. These
are the numbers today, what keeps them low, and the rules that keep them that
way as the codebase grows.

## Where we are

Measured on a 4-core cloud container (no KVM), 22 crates, 143 build units.
"After an edit" means a real code change, not a `touch`:

| Build | Time |
|---|---|
| Clean debug build, whole workspace (`make build`) | 27 s; about 31 s with the model gateway's TLS stack (+15%, see Known costs) |
| Rebuild after editing an app crate (`glass-app-files`) | 1.5 s |
| Rebuild after editing `glass-runtime` | 1.6 s |
| Rebuild after editing `glass-types`, the bottom crate | 2.4 s |
| All tests (`make test`) | 4–9 s |
| FreeBSD binaries, clean, `vm` profile | 57 s |
| **FreeBSD rebuild after an edit, `vm` profile** (the VM loop) | **1.3–1.8 s** |
| FreeBSD rebuild after an edit, `release` profile (thin LTO) | 25 s |
| Rebuild + deploy into the running VM (`make freebsd vm-deploy`) | ~12 s |
| The runtime's integration tests (one binary, `tests/it`): rebuilt after a runtime change, then run | 17 s, 24 s (were 23 binaries: 33 s, 37 s; ADR 0052) |
| Shipped FreeBSD binaries from clean, `release` profile | 390 s; 41 MB (glassd 13.3 MB) |
| Every TLA+ model (`make tla`, four at once) | about 2 min |

The workspace has grown to 49 crates since the first rows were measured;
`make test` now builds 92 test binaries.

## What makes it fast

- **Profiles per job** (`Cargo.toml`):
  - `dev`: debug info for our crates only (dependencies get none), and
    `split-debuginfo = "unpacked"` so the linker doesn't copy debug info into
    every binary. Faster links, and `target/` shrank from 2.3 GB to 1.6 GB.
  - `vm`: what `make freebsd`, `make vm-deploy` and `make e2e-vm` use. Optimized
    (`opt-level = 1`) but incremental, without LTO, 256 codegen units. A
    one-crate edit rebuilds the FreeBSD binaries in about 3 s instead of 25 s.
  - `release`: thin LTO, one codegen unit, no symbols. Only for images
    people will run (`make image-release`). Without symbols each binary is
    a quarter smaller, and nothing ships with backtraces on (a panic still
    says what and where); one codegen unit took 18% more off and built as
    fast, the dependencies being most of the time. 41 MB for what ships,
    against 65 MB before (ADR 0052).
- **One test binary per crate.** Each integration test file is a binary
  that links its crate and everything under it again: the runtime's 23
  were 70 MB and a link apiece. They're the modules of `tests/it` now,
  sharing `common`, and their tests run in one process.
- **LLD everywhere.** Rust ≥ 1.90 links Linux binaries with `rust-lld` by
  default; the FreeBSD wrapper passes `-fuse-ld=lld`.
- **Many small crates.** Cargo builds independent crates in parallel and
  rebuilds only what an edit touches. Editing a service crate never rebuilds
  the libraries below it, and each app's actions are their own crate
  (`crates/apps`, ADR 0013), so editing one app rebuilds that app and the
  binaries, not the other apps or the runtime.
- **Lean dependencies.** Pure Rust and no C build scripts, tokio with only
  the features we use, no `regex` (`make timings` shows the full chart). The
  one C library is the base system's OpenSSL, which the model gateway links
  for TLS; `openssl-sys` only reads its headers (ADR 0018).

## Linting

`make lint` checks only what changed since the merge base with
`origin/main` (committed, staged, unstaged and untracked; `LINT_BASE=<rev>`
moves the base), runs its checks in parallel and skips the ones whose
inputs didn't change (`tools/lint.py`):

| Check | Runs when |
|---|---|
| rustfmt | on the changed `.rs` files (all, if `rustfmt.toml` or the toolchain changed) |
| clippy, host and FreeBSD at once | anything under `crates/` or `tests/`, or a root Cargo, clippy or toolchain file. The FreeBSD run needs the sysroot (`make freebsd-sysroot`), because openssl-sys reads its OpenSSL headers; worktrees use the main checkout's |
| layering | any `Cargo.toml` |
| generated art | `tools/branding/` or the overlay's branding outputs |
| shellcheck | the changed shell scripts |
| Python | the changed `.py` files compile |

`make lint-all` runs every check on the whole tree; CI (`make ci`) uses it.

Clippy always covers the whole workspace. Cargo already re-lints only the
changed crates and the crates that depend on them; a `-p` subset changes
feature unification (`-Z feature-unification` is still unstable), which
rechecks dependencies such as `serde`, `syn` and `libc` with other features
and made scoped runs slower, not faster.

Measured on the same 4-core container under the same load, old lint vs
`make lint`:

| Situation | Before | After |
|---|---|---|
| Nothing new to lint (docs, scripts, or already linted) | 1.1 s | 0.1 s |
| One-line edit in `glassctl` | 1.7 s | 1.1–1.6 s |
| One-line edit in an app crate (`glass-app-files`) | 3.0–3.4 s | 2.7 s |
| One-line edit in `glass-runtime` | 2.9–3.7 s | 2.1 s |
| One-line edit in `glass-types` (everything above re-lints) | 6.7–8.1 s | 6.1–6.9 s |
| `make lint-all`, warm | — | 1.3 s |

The remaining time after an edit is clippy re-checking the edited crate and
its dependents, which no lint scope can skip. A fresh checkout or worktree
pays about 25 s once to check the dependencies.

## Habits

- Use `make check` (no codegen) while editing; rust-analyzer does the same.
- Iterate in the VM with the default `vm` profile; never pass `--release`
  in the loop.
- Run one crate's tests while working on it: `cargo test -p glass-runtime`;
  one of the runtime's test modules: `cargo test -p glass-runtime --test it
  chat::`.
- A worktree building into the main checkout's target dir (to save disk)
  writes the same files as main: cargo names a workspace crate's build by
  its path within the workspace, so the two overwrite each other and one
  side compiles against the other's older crate ("no method named …" for
  a method that is right there). `touch` the changed crates' sources and
  rebuild; better, merge main into the branch first.

## Rules for new code

1. **Check the cost of a dependency before adding it.** Run `make timings`
   before and after; a new dependency that adds seconds to a clean build
   needs a reason in the PR. Prefer crates already in the workspace.
2. **Keep proc macros out of the bottom of the graph.** `glass-types` and
   `glass-api` use only `serde` derives; avoid adding derive-heavy crates
   there, because everything above waits for them.
3. **Split crates that grow past ~5k lines** or that mix concerns, so edits
   stay local. New features with their own state (tailoring layers, the
   gateway) start as their own crate rather than another module in
   `glass-runtime`.
4. **A new app is a new crate** in `crates/apps`, depending on lib crates
   only (`add-action` skill).
5. **A new integration test is a module**, not a file of its own: in
   glass-runtime, `tests/it/<name>.rs` and a `mod` line in `tests/it/main.rs`.
   Elsewhere, start a crate's `tests/it` the same way once it needs a
   second file.
6. **No build scripts that compile C.** They serialize builds and complicate
   cross-compiling.
7. **Don't turn on LTO or `codegen-units = 1`** outside `release`.
8. **HTTP and TLS live in `glass-gateway-providers` only**, which only
   `glassd` links (ADR 0018). Another crate that needs the network asks the
   gateway, or makes the case for moving code there.

## Known costs and options

- Two `syn` majors (2 and 3) are compiled because the newest `serde` and
  `clap` derive macros moved to `syn` 3 while other macro crates haven't yet.
  This resolves itself as crates update; `cargo deny` reports duplicates.
- The slowest units in a clean FreeBSD build are `tokio` (19 s),
  `glass-runtime` (14 s), and the `glassctl` and `glass-e2e` binaries
  (12–13 s each); `glass-runtime` sits on the critical path to `glassd`.
  `tracing-subscriber`'s `env-filter` used to add `regex` (19 s of CPU);
  `glassd` now filters logs with `filter::Targets`, same `GLASS_LOG` syntax.
- **The model gateway's TLS stack** (ADR 0018), measured by building the
  commit before it and the change alternately under the same load (another
  VM was running on the 4-core container): a clean debug build of the
  workspace went from 34.9 s to 40.2 s, +5.3 s (+15%). The new units are
  `openssl` (4.9 s), `glass-gateway-providers` (5.0 s), `glass-gateway`
  (2.6 s), `cc` (1.9 s, for openssl-sys's build script) and nine more under
  1 s each; they're off the critical path (`glass-runtime`, then `glassd`)
  but compete for cores. A clean FreeBSD `vm` build of `glassd` went from a
  median 61 s to 75 s under the same noisy load (40 s before on an idle
  machine), and `glassd` from 6.7 to 7.7 MB. The rustls and ring
  alternative cost more: alone, its stack built in 21 s for FreeBSD against
  14 s for OpenSSL's, and compiles C.
- **CI**: cache `~/.cargo` and `target/` keyed on `Cargo.lock` (for example
  `Swatinem/rust-cache`), and consider `sccache` for a shared cache once
  several machines build the same commits.
- **Faster test runs**: `cargo-nextest` runs each test in its own process,
  in parallel. Most of `make test`'s time is building and linking test
  binaries, not running them, so fewer binaries (above) came first.
- **Crates built in two versions**: `rand` 0.9 and 0.10, `getrandom`,
  `hashbrown` 0.14, `io-lifetimes` 2, `toml`'s parsers. They come from
  boa, yrs, str0m and cap-std; `cargo update` resolves none yet.
- **glassctl** links every app for `glassctl policy check`'s warnings: its
  own code is 0.5 MB of its 9.6. Asking glassd for them instead would make
  it a few MB.
- **mold** links faster than LLD on large binaries; add it per machine with
  `-C link-arg=-fuse-ld=mold` if link time becomes visible.
