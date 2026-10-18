#!/usr/bin/env python3
"""Enforces the workspace's crate layering and manifest conventions.

Layers (by directory), and what each may depend on inside the workspace:

    crates/lib    lib
    crates/apps   lib             (apps never depend on each other)
    crates/svc    lib, apps, svc
    crates/bin    lib, apps, svc  (binaries never depend on each other)
    tests/*       lib, apps, svc

Every workspace crate must also inherit the workspace lints, license and
`publish = false`, so no crate quietly opts out of the shared rules.

FreeBSD-specific code lives only in glass-platform (PL-1, PL-2, ADR 0022):
no `target_os = "freebsd"` anywhere else, in this workspace or the shell's.

See docs/repo-layout.md. Run via `make check-layers` (part of `make lint`).
"""

import json
import subprocess
import sys
import tomllib
from pathlib import Path

ALLOWED = {
    "lib": {"lib"},
    "apps": {"lib"},
    "svc": {"lib", "apps", "svc"},
    "bin": {"lib", "apps", "svc"},
    "tests": {"lib", "apps", "svc"},
}


def layer_of(manifest: Path, root: Path) -> str | None:
    parts = manifest.relative_to(root).parts
    if parts[0] == "crates" and len(parts) > 2:
        return parts[1]
    if parts[0] == "tests":
        return "tests"
    return None


def main() -> int:
    meta = json.loads(
        subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"])
    )
    root = Path(meta["workspace_root"])
    packages = {p["name"]: p for p in meta["packages"]}
    layers = {name: layer_of(Path(p["manifest_path"]), root) for name, p in packages.items()}
    problems = []

    for name, package in sorted(packages.items()):
        layer = layers[name]
        if layer not in ALLOWED:
            problems.append(f"{name}: lives outside crates/{{lib,apps,svc,bin}} and tests/")
            continue
        for dep in package["dependencies"]:
            target = dep["name"]
            if target not in layers or dep["kind"] == "dev":
                continue
            if layers[target] not in ALLOWED[layer]:
                problems.append(f"{name} ({layer}) depends on {target} ({layers[target]})")

        manifest = tomllib.loads(Path(package["manifest_path"]).read_text())
        pkg = manifest.get("package", {})
        if manifest.get("lints", {}).get("workspace") is not True:
            problems.append(f"{name}: add `[lints] workspace = true`")
        for key in ("license-file", "publish", "edition", "version"):
            value = pkg.get(key)
            if not (isinstance(value, dict) and value.get("workspace") is True):
                problems.append(f"{name}: set `{key}.workspace = true`")
        for dep_name, spec in manifest.get("dependencies", {}).items():
            if not (isinstance(spec, dict) and spec.get("workspace") is True):
                problems.append(f"{name}: depend on {dep_name} through the workspace (`{dep_name}.workspace = true`)")

    for path in sorted(root.glob("crates/**/*.rs")) + sorted(root.glob("tests/**/*.rs")) + sorted(
        root.glob("shell/crates/**/*.rs")
    ):
        if "crates/lib/platform/" in path.as_posix() or "/target/" in path.as_posix():
            continue
        if 'target_os = "freebsd"' in path.read_text(errors="replace"):
            problems.append(
                f"{path.relative_to(root)}: FreeBSD-specific code belongs in glass-platform (PL-2)"
            )

    if problems:
        print("layering check failed:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1
    print(f"layering ok: {len(packages)} crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())
