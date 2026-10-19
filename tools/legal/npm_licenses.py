#!/usr/bin/env python3
"""Check that every npm package a JavaScript workspace uses is permissively
licensed, by the same allow list `cargo deny` uses for crates (deny.toml).

usage: tools/legal/npm_licenses.py <package dir> [--list]

The package directory holds package.json and package-lock.json (apps/, and
console/ can use it too). Every package in the lockfile counts, development
tools included: they run on our machines and in CI. Our own workspace
packages must be private, so they are never published. Licenses come from
the lockfile, or from the installed package.json when the lockfile lacks
one; an SPDX expression passes when it allows a permissive choice ("MIT OR
GPL-3.0" passes, "MIT AND GPL-3.0" doesn't).
"""

import json
import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def allowed_licenses() -> set[str]:
    with open(REPO / "deny.toml", "rb") as f:
        return set(tomllib.load(f)["licenses"]["allow"])


def permits(expression: str, allowed: set[str]) -> bool:
    """Whether an SPDX expression can be satisfied by allowed licenses."""
    tokens = re.findall(r"\(|\)|[^\s()]+(?:\s+WITH\s+[^\s()]+)?", expression)
    pos = 0

    def peek() -> str | None:
        return tokens[pos] if pos < len(tokens) else None

    def take() -> str:
        nonlocal pos
        pos += 1
        return tokens[pos - 1]

    def atom() -> bool:
        token = take()
        if token == "(":
            value = either()
            if take() != ")":
                raise ValueError("unbalanced parentheses")
            return value
        return token in allowed

    def both() -> bool:
        value = atom()
        while peek() == "AND":
            take()
            value = atom() and value
        return value

    def either() -> bool:
        value = both()
        while peek() == "OR":
            take()
            value = both() or value
        return value

    try:
        result = either()
        return result and pos == len(tokens)
    except (IndexError, ValueError):
        return False


def license_of(entry: dict, installed: Path) -> str | None:
    if isinstance(entry.get("license"), str):
        return entry["license"]
    try:
        manifest = json.loads((installed / "package.json").read_text())
    except (OSError, ValueError):
        return None
    license = manifest.get("license")
    if isinstance(license, dict):
        license = license.get("type")
    if isinstance(license, str):
        return license
    legacy = manifest.get("licenses")
    if isinstance(legacy, list) and legacy:
        return " OR ".join(str(item.get("type", item)) for item in legacy)
    return None


def check(root: Path, show: bool) -> int:
    lock_path = root / "package-lock.json"
    try:
        lock = json.loads(lock_path.read_text())
    except OSError:
        print(f"npm licenses: no {lock_path}; run npm install there first", file=sys.stderr)
        return 1
    if lock.get("lockfileVersion", 1) < 2:
        print(f"npm licenses: {lock_path} is lockfile version 1; use npm 7 or later", file=sys.stderr)
        return 1
    allowed = allowed_licenses()
    problems: list[str] = []
    checked = 0
    for path, entry in sorted(lock.get("packages", {}).items()):
        if entry.get("link"):
            continue  # a workspace package, checked as itself below
        if "node_modules/" not in path:
            # Ours: the root or a workspace. Never published.
            manifest = root / path / "package.json"
            try:
                private = json.loads(manifest.read_text()).get("private") is True
            except (OSError, ValueError):
                private = False
            if not private:
                problems.append(f"{manifest.relative_to(REPO)}: our own packages must be \"private\": true")
            continue
        name = path.rsplit("node_modules/", 1)[1]
        version = entry.get("version", "?")
        license = license_of(entry, root / path)
        checked += 1
        if show:
            print(f"  {name}@{version}  {license or 'UNKNOWN'}")
        if license is None:
            problems.append(f"{name}@{version}: no license declared")
        elif not permits(license, allowed):
            problems.append(f"{name}@{version}: {license} is not on the allow list (deny.toml)")
    for problem in problems:
        print(f"  {problem}", file=sys.stderr)
    where = root.resolve().relative_to(REPO) if root.resolve().is_relative_to(REPO) else root
    if problems:
        print(f"npm licenses: {len(problems)} problem(s) in {where}", file=sys.stderr)
        return 1
    print(f"npm licenses: {checked} packages in {where}, all permissive")
    return 0


def self_test() -> None:
    allowed = {"MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception"}
    assert permits("MIT", allowed)
    assert permits("(MIT OR GPL-3.0-only)", allowed)
    assert not permits("MIT AND GPL-3.0-only", allowed)
    assert permits("Apache-2.0 WITH LLVM-exception", allowed)
    assert not permits("GPL-2.0 WITH Classpath-exception-2.0", allowed)
    assert not permits("(MIT", allowed)
    assert not permits("", allowed)


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if "--self-test" in sys.argv:
        self_test()
        print("npm licenses: self-test passed")
        return 0
    if len(args) != 1:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    return check(Path(args[0]), "--list" in sys.argv)


if __name__ == "__main__":
    sys.exit(main())
