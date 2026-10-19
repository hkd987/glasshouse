#!/usr/bin/env python3
"""Lint what changed (`make lint`), or everything (`make lint-all`, CI).

"Changed" is every file that differs from the merge base with origin/main
(override with LINT_BASE, such as LINT_BASE=HEAD~3), plus staged, unstaged
and untracked files. Each check runs only when its inputs changed:

  rustfmt      the changed .rs files (all of them if rustfmt.toml or the
               toolchain changed)
  clippy       host and FreeBSD, at the same time, when anything under
               crates/ or tests/ or a root Cargo, clippy or toolchain file
               changed. Always the whole workspace: cargo already re-lints
               only the changed crates and the ones that depend on them, and
               a `-p` subset changes feature unification, which recompiles
               dependencies (docs/dev/build-performance.md)
  layering     any Cargo.toml or tools/check-layers.py
  art          tools/branding/ or the overlay's branding outputs
  clippy       the desktop shell's workspace, when anything under shell/
               changed
  shellcheck   the changed shell scripts
  python       the changed .py files compile
  apps         `make apps-lint` (types, manifests, npm licenses) when anything
               under apps/ or tools/legal/npm_licenses.py changed
  console      `make console-lint` (clippy, types, BR-1) when the admin
               console's server or web app changed

Nothing changed: done in well under a second.
"""

import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
FREEBSD = "x86_64-unknown-freebsd"
# Changing any of these can change every crate's lints.
RUST_GLOBAL = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "clippy.toml",
    ".cargo/config.toml",
}
FMT_GLOBAL = {"rustfmt.toml", "rust-toolchain.toml"}
ART_INPUTS = ("tools/branding/", "os/freebsd/overlay/usr/local/share/glasshouse/branding/")
# The admin console (ADR 0023): its server and its web app.
CONSOLE = ("crates/bin/glass-console/", "apps/console/")
# Third-party code kept as upstream wrote it, apart from marked patches.
VENDORED = ("shell/vendor/",)


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO, check=True, capture_output=True, text=True
    ).stdout


def base() -> str:
    if os.environ.get("LINT_BASE"):
        return os.environ["LINT_BASE"]
    try:
        return git("merge-base", "HEAD", "origin/main").strip()
    except subprocess.CalledProcessError:
        return "HEAD"


def changed_files(since: str) -> set[str]:
    """Committed since `since`, staged, unstaged and untracked."""
    files = set(git("diff", "--name-only", since).split())
    files |= set(git("ls-files", "--others", "--exclude-standard").split())
    return files


def all_files() -> set[str]:
    return set(git("ls-files", "--cached", "--others", "--exclude-standard").split())


def is_shell(path: str) -> bool:
    if path.endswith(".sh"):
        return True
    try:
        with open(REPO / path, "rb") as f:
            first = f.readline(80)
    except OSError:
        return False
    return first.startswith(b"#!") and (b"/sh" in first or b"bash" in first)


def plan(files: set[str], everything: bool) -> list[tuple[str, list[str]]]:
    """The commands to run, as (label, argv)."""
    existing = {f for f in files if (REPO / f).is_file() and not f.startswith(VENDORED)}
    steps: list[tuple[str, list[str]]] = []

    rust = sorted(f for f in existing if f.endswith(".rs"))
    if everything or files & FMT_GLOBAL:
        steps.append(("rustfmt", ["cargo", "fmt", "--all", "--check"]))
    elif rust:
        steps.append(("rustfmt", ["rustfmt", "--check", "--edition", "2024", *rust]))

    if everything or files & RUST_GLOBAL or any(f.startswith(("crates/", "tests/")) for f in files):
        steps.append(("clippy (host)",
                      ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]))
        steps.append(("clippy (FreeBSD)",
                      ["cargo", "clippy", "--workspace", "--target", FREEBSD, "--", "-D", "warnings"]))

    # The desktop shell is its own workspace (shell/README.md); `make
    # shell-lint` covers it in full, so `--all` leaves it to that.
    if not everything and any(f.startswith("shell/") for f in files):
        steps.append(("clippy (shell)",
                      ["cargo", "clippy", "--manifest-path", "shell/Cargo.toml", "--workspace",
                       "--all-targets", "--", "-D", "warnings"]))
        steps.append(("clippy (shell, FreeBSD)",
                      ["cargo", "clippy", "--manifest-path", "shell/Cargo.toml", "--workspace",
                       "--all-targets", "--target", FREEBSD, "--", "-D", "warnings"]))

    if everything or any(f.endswith(("Cargo.toml", ".rs")) or f == "tools/check-layers.py" for f in files):
        steps.append(("layering", ["tools/check-layers.py"]))

    if everything or any(f.startswith(ART_INPUTS) for f in files):
        steps.append(("art", ["tools/branding/art.py", "--check"]))

    shell = sorted(f for f in existing if is_shell(f))
    if shell:
        steps.append(("shellcheck", ["shellcheck", "--external-sources", *shell]))

    if everything or any(f.startswith("apps/") or f == "tools/legal/npm_licenses.py" for f in files):
        steps.append(("apps", ["make", "--no-print-directory", "apps-lint"]))

    if everything or any(f.startswith(CONSOLE) for f in files):
        steps.append(("console", ["make", "--no-print-directory", "console-lint"]))

    python = sorted(f for f in existing if f.endswith(".py"))
    if python:
        steps.append(("python", [sys.executable, "-m", "py_compile", *python]))
    return steps


def run(steps: list[tuple[str, list[str]]]) -> int:
    """Runs every step at once (cargo serializes on its own lock where it
    must) and reports each with its own time. Output is shown for failures."""
    started = time.monotonic()

    def one(step: tuple[str, list[str]]) -> tuple[str, float, subprocess.CompletedProcess]:
        label, argv = step
        began = time.monotonic()
        done = subprocess.run(argv, cwd=REPO, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True)
        return label, time.monotonic() - began, done

    with ThreadPoolExecutor(max_workers=len(steps)) as pool:
        results = list(pool.map(one, steps))
    failed = 0
    for label, took, done in results:
        if done.returncode == 0:
            print(f"  ok    {label:<23} {took:5.1f}s")
        else:
            failed += 1
            print(f"  FAIL  {label:<23} {took:5.1f}s\n{done.stdout.rstrip()}\n")
    print(f"lint: {len(steps) - failed} of {len(steps)} checks passed "
          f"in {time.monotonic() - started:.1f}s")
    return 1 if failed else 0


def main() -> int:
    everything = "--all" in sys.argv[1:]
    if everything:
        files = all_files()
        print("lint: everything")
    else:
        since = base()
        files = changed_files(since)
        if not files:
            print(f"lint: nothing changed since {since[:12]}")
            return 0
        print(f"lint: {len(files)} changed file{'s' if len(files) != 1 else ''} "
              f"since {since[:12]} (make lint-all checks everything)")
    steps = plan(files, everything)
    if not steps:
        print("lint: nothing to check for these files")
        return 0
    return run(steps)


if __name__ == "__main__":
    sys.exit(main())
