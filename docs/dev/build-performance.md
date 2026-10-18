# Keeping builds fast

Rust builds get slow when a workspace grows without anyone watching. These
are the numbers today, what keeps them low, and the rules that keep them that
way as the codebase grows.

## Where we are

Measured on a 4-core cloud container (no KVM), 16 crates, 137 build units:

| Build | Time |
|---|---|
| Clean debug build, whole workspace (`make build`) | 26 s |
| Rebuild after editing `glass-runtime` (debug) | 2.7 s |
| Rebuild after editing `glass-types`, the bottom crate (debug) | 1.8 s |
| All tests (`make test`) | 4–9 s |
| FreeBSD binaries, clean, `vm` profile | 47 s |
| **FreeBSD rebuild after an edit, `vm` profile** (the VM loop) | **3 s** |
| FreeBSD rebuild after an edit, `release` profile (thin LTO) | 25 s |
| Rebuild + deploy into the running VM (`make freebsd vm-deploy`) | ~12 s |

## What makes it fast

- **Profiles per job** (`Cargo.toml`):
  - `dev`: debug info for our crates only (dependencies get none), and
    `split-debuginfo = "unpacked"` so the linker doesn't copy debug info into
    every binary. Faster links, and `target/` shrank from 2.3 GB to 1.6 GB.
  - `vm`: what `make freebsd`, `make vm-deploy` and `make e2e-vm` use. Optimized
    (`opt-level = 1`) but incremental, without LTO, 256 codegen units. A
    one-crate edit rebuilds the FreeBSD binaries in about 3 s instead of 25 s.
  - `release`: thin LTO, stripped. Only for images people will run
    (`make image-release`).
- **LLD everywhere.** Rust ≥ 1.90 links Linux binaries with `rust-lld` by
  default; the FreeBSD wrapper passes `-fuse-ld=lld`.
- **Many small crates.** Cargo builds independent crates in parallel and
  rebuilds only what an edit touches. Editing a service crate never rebuilds
  the libraries below it.
- **Lean dependencies.** Pure-Rust only, no C build scripts, tokio with only
  the features we use. The slowest units in a clean build are `tokio`, our
  own `glass-runtime` and the `syn` proc-macro parser (`make timings` shows
  the full chart).

## Habits

- Use `make check` (no codegen) while editing; rust-analyzer does the same.
- Iterate in the VM with the default `vm` profile; never pass `--release`
  in the loop.
- Run one crate's tests while working on it: `cargo test -p glass-runtime`.

## Rules for new code

1. **Check the cost of a dependency before adding it.** Run `make timings`
   before and after; a new dependency that adds seconds to a clean build
   needs a reason in the PR. Prefer crates already in the workspace.
2. **Keep proc macros out of the bottom of the graph.** `glass-types` and
   `glass-api` use only `serde` derives; avoid adding derive-heavy crates
   there, because everything above waits for them.
3. **Split crates that grow past ~5k lines** or that mix concerns, so edits
   stay local. `glass-runtime` is the first candidate (the action pipeline
   and the APIs could become their own crates).
4. **No build scripts that compile C.** They serialize builds and complicate
   cross-compiling.
5. **Don't turn on LTO or `codegen-units = 1`** outside `release`.

## Known costs and options

- Two `syn` majors (2 and 3) are compiled because the newest `serde` and
  `clap` derive macros moved to `syn` 3 while other macro crates haven't yet.
  This resolves itself as crates update; `cargo deny` reports duplicates.
- `tracing-subscriber`'s `env-filter` pulls in `regex` (only for `glassd`).
  Replace it with a plain level filter if it ever matters.
- **CI**: cache `~/.cargo` and `target/` keyed on `Cargo.lock` (for example
  `Swatinem/rust-cache`), and consider `sccache` for a shared cache once
  several machines build the same commits.
- **Faster test runs**: `cargo-nextest` runs each test in its own process,
  in parallel, and is worth adding once the suite takes more than ~30 s.
- **mold** links faster than LLD on large binaries; add it per machine with
  `-C link-arg=-fuse-ld=mold` if link time becomes visible.
