# 0036. Developer tools come from the Store into the developer jail

Status: **Proposed** (2026-10-20; PRD DW-7 to DW-9, proposed the same day).
PRD DW-1, DW-2, DW-10 to DW-12, DW-20, EP-2, DV-11, DV-22. Builds on ADRs
0022 (pkgbase), 0023 (catalog by role) and 0026 (signed software).

## Context
The owner wants developer software in the Store: VS Code, git, a desktop
git client such as GitHub Desktop, and the core languages with their
version managers (nvm, Go, and the rest), each installed from the Store.

Two requirements pull against each other. EP-2: only signed software runs
on the system. But a language toolchain exists to build and download code
nobody signed: `npm install` runs package scripts, `go build` makes a
binary, `cargo` compiles build scripts. DW-1 already settles where that
may happen: in a developer workspace, granted by role (DW-10), limited to
approved package sources (DW-11, DW-12) and audited from the host (DW-20).

What exists, checked 2026-10-20 against the FreeBSD 15 quarterly package
set we build images from (`build/cache/freebsd/15.1-RELEASE/ports`):

| Need | Package (version) |
|---|---|
| VS Code | `vscode` 1.138 (Code - OSS, extensions from Open VSX since 2023) |
| git, GitHub | `git` 2.56, `gh` 2.83 |
| Git clients | `git-cola` 4.14 (Qt), `qgit` 2.13 (Qt), `lazygit` 0.65, `gitui` 0.28 and `tig` 2.6 (terminal) |
| Node.js | `node20`, `node22`, `node24`, `npm`, `yarn` |
| Go | `go126` 1.26 (Go itself also publishes freebsd-amd64 builds) |
| Python | `python311` to `python314` |
| Rust | `rust` 1.98 |
| Java | `openjdk17`, `openjdk21`, `openjdk25` |
| .NET | `dotnet8` 8.0, `dotnet` 9.0 |
| Others | `ruby` 3.4, `php83`/`php84`, `swift6`, `zig`, `kotlin`, `elixir`, `podman` |

What doesn't exist natively:
- GitHub Desktop. GitHub ships only macOS and Windows builds, and a
  community fork for Linux.
- Microsoft's own VS Code build and its marketplace. Microsoft's terms
  limit the marketplace to Microsoft's builds, so Pylance and Remote
  Development aren't available to Code - OSS.
- nvm as people know it. It downloads Node from nodejs.org, which publishes
  no FreeBSD binaries, so on FreeBSD it compiles every version from source.

The Node versions also conflict with one another when installed into one
root (they all install `bin/node`).

## Decision
1. **A kind of catalog item: a developer tool.** It names packages from
   the org's mirror (DW-12), the versions it offers, and the project files
   that pin a version (`.nvmrc` and `.node-version`, `go.mod`'s
   `toolchain`, `.python-version`, `rust-toolchain.toml`, `global.json`).
   The Store shows developer tools only to people with developer mode
   (DW-10). Like any item, IT can require, offer or block each one per
   group (DV-11). Installing or removing one is the person's own action
   (Notify, undoable), audited with name, version and source (DW-20).
2. **Installed into the person's developer jail, never the base.** Each
   person with developer mode has one persistent developer jail on its own
   dataset. Packages are installed into it with `pkg --rootdir`, verified
   against our repository key as every package is. This is the place EP-2
   relaxes. The base system's verified execution (ADR 0026) never trusts
   files under the jail, so nothing built there runs outside it.
3. **Versions side by side.** Each version gets its own root inside the
   jail (`/tools/node/22`, `/tools/python/3.13`). A small shim on the
   jail's `PATH` (`node`, `npm`, `npx`, `python3`, `go`, …) walks up from
   the working directory to the nearest pin file and runs that version,
   else the person's default. This is what nvm, pyenv and rustup do,
   without downloading unsigned toolchains. When a project pins a version
   the person hasn't installed, the shim says which one and offers the
   Store, rather than fetching it.
4. **Windows on the desktop.** VS Code and the Qt git clients run in the
   jail. Their windows reach the desktop through the compositor like other
   native windows (ADR 0025). The jail gets the session's Wayland socket
   and the person's projects folder, and nothing else of their home.
5. **Linux-only tools wait for the Linux workspace** (DW-2, P1):
   Microsoft's VS Code with its marketplace, the Linux fork of GitHub
   Desktop, nvm itself, and JetBrains IDEs. Until then the Store lists
   them as needing the workspace, so people see that they're coming.
6. **Agents** reach developer tools only through the workspace, under the
   harness (DW-5), never through the base system's agent jails.

## The git client
The owner chose GitHub Desktop (2026-10-20). It has no FreeBSD build, so it
comes with the Linux workspace (DW-2), from GitHub's Linux fork. Until then
developers have VS Code's built-in source control and the GitHub CLI
(`gh`), both native.

## Consequences
- The org's mirror carries these packages, a few GB per language
  generation. The jail reaches only the mirror and approved endpoints
  (DW-11).
- A new runtime surface: the developer jail, mounts that include a
  Wayland socket, and a pkg root it can write. Building it needs the
  `containment-review` checklist and probe checks that agent jails can't
  reach it and the base can't execute from it.
- The VM can test all of it: packages come from the local cache, and the
  e2e can install `node22`, check that `.nvmrc` picks it, and check that
  the base refuses to execute it once veriexec lands.
