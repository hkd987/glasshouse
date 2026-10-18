#!/usr/bin/env python3
"""Open-source notices (BR-2) and the software bill of materials (BR-4).

Walks the dependency graph of the binaries we ship (normal and build
dependencies, for the FreeBSD target) and writes:

  --notices FILE   NOTICES.txt for the image: the source offer for the base
                   system's copyleft components (BR-3), every third-party
                   crate with its license and the license texts it ships,
                   and the base system's copyright notice and imported
                   components' licenses (os/freebsd/legal, written by
                   base_notices.py). The Settings "Legal notices" page shows
                   this file.
  --sbom FILE      A CycloneDX 1.5 JSON SBOM. Admins and customer security
                   teams only (BR-4): never stage it into the image or
                   publish it.

    tools/legal/notices.py --package glassd --package glassctl \\
        --notices build/legal/NOTICES.txt --sbom build/sbom/glasshouse.cdx.json

Exits non-zero if a shipped crate's license is missing or outside the
allow list in deny.toml, so a bad dependency fails the build, not legal
review.
"""

import argparse
import json
import re
import subprocess
import sys
import tarfile
import tomllib
import uuid
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BASE_LEGAL = REPO / "os/freebsd/legal"
TARGET = "x86_64-unknown-freebsd"
LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE|COPYRIGHT)", re.I)


def metadata() -> dict:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET],
        cwd=REPO, check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


def shipped_closure(meta: dict, roots: list[str]) -> list[dict]:
    """Third-party packages reachable from `roots` through non-dev edges."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    members = set(meta["workspace_members"])
    by_name = {packages[m]["name"]: m for m in members}
    missing = [r for r in roots if r not in by_name]
    if missing:
        sys.exit(f"notices: not workspace packages: {', '.join(missing)}")
    seen, stack = set(), [by_name[r] for r in roots]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    return sorted(
        (packages[p] for p in seen if p not in members),
        key=lambda p: (p["name"], p["version"]),
    )


def allowed_licenses() -> set[str]:
    with open(REPO / "deny.toml", "rb") as f:
        return set(tomllib.load(f)["licenses"]["allow"])


def license_ids(expression: str) -> set[str]:
    return set(re.findall(r"[A-Za-z0-9.\-+]+", expression)) - {"AND", "OR", "WITH"}


def license_texts(package: dict) -> list[tuple[str, str]]:
    root = Path(package["manifest_path"]).parent
    files = sorted(p for p in root.iterdir() if p.is_file() and LICENSE_FILE.match(p.name))
    if package.get("license_file"):
        files.append(root / package["license_file"])
    texts, seen = [], set()
    for path in files:
        if path.resolve() in seen or not path.exists():
            continue
        seen.add(path.resolve())
        texts.append((path.name, path.read_text(errors="replace").strip()))
    return texts


def base_copyright(base_txz: Path | None) -> str:
    """The base system's COPYRIGHT, cached beside base.txz after one read
    (finding it means decompressing the whole archive)."""
    if base_txz is None or not base_txz.exists():
        return ""
    cached = base_txz.with_name("COPYRIGHT")
    if not cached.exists() or cached.stat().st_mtime < base_txz.stat().st_mtime:
        with tarfile.open(base_txz, mode="r|xz") as tar:
            for member in tar:
                if member.name in ("./COPYRIGHT", "COPYRIGHT"):
                    cached.write_bytes(tar.extractfile(member).read())
                    break
            else:
                sys.exit(f"notices: no COPYRIGHT in {base_txz}")
    return cached.read_text(errors="replace").strip()


def base_legal(name: str) -> str:
    path = BASE_LEGAL / name
    return path.read_text().strip() if path.exists() else ""


def write_notices(path: Path, packages: list[dict], base: str) -> list[str]:
    """Writes the notices file; identical license texts (most Apache-2.0
    copies) are printed once and referenced. Returns crates that ship no
    license file of their own."""
    bare, texts, refs = [], [], {}
    rule = "=" * 78
    out = [
        "Glasshouse open-source notices",
        "",
        "Glasshouse includes open-source software. Each component's license",
        "is reproduced below as that license requires.",
        "",
    ]
    offer = base_legal("source-offer.txt")
    if offer:
        out += [rule, offer, ""]
    if base:
        out += [rule, "Base system", rule, "", base, ""]
    imported = base_legal("base-notices.txt")
    if imported:
        out += [rule, imported, ""]
    out += [rule, "Third-party components", rule, ""]
    for p in packages:
        out += [f"{p['name']} {p['version']}", f"  License: {p.get('license') or 'see below'}"]
        if p.get("repository"):
            out.append(f"  Source: {p['repository']}")
        files = license_texts(p)
        if not files:
            bare.append(f"{p['name']} {p['version']}")
            out.append("  (No license file in the package; licensed as stated above.)")
        for name, text in files:
            if text not in refs:
                texts.append(text)
                refs[text] = f"[{len(texts)}]"
            out.append(f"  {name}: license text {refs[text]}")
        out.append("")
    out += [rule, "License texts", rule, ""]
    for i, text in enumerate(texts, 1):
        out += [f"[{i}]", "", text, "", "-" * 78, ""]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(out) + "\n")
    return bare


def write_sbom(path: Path, packages: list[dict], roots: list[str], version: str) -> None:
    components = [
        {
            "type": "library",
            "name": p["name"],
            "version": p["version"],
            "purl": f"pkg:cargo/{p['name']}@{p['version']}",
            "licenses": [{"expression": p["license"]}] if p.get("license") else [],
        }
        for p in packages
    ]
    sbom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid4()}",
        "version": 1,
        "metadata": {
            "timestamp": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "component": {"type": "operating-system", "name": "Glasshouse", "version": version},
            "properties": [{"name": "glasshouse:shipped-binaries", "value": " ".join(roots)}],
        },
        "components": components,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(sbom, indent=2) + "\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--package", action="append", required=True, help="a shipped workspace binary")
    ap.add_argument("--notices", type=Path)
    ap.add_argument("--sbom", type=Path)
    ap.add_argument("--base-txz", type=Path, help="FreeBSD base.txz, for the base COPYRIGHT")
    args = ap.parse_args()

    meta = metadata()
    packages = shipped_closure(meta, args.package)
    allowed = allowed_licenses()
    problems = [
        f"{p['name']} {p['version']}: license {p.get('license')!r} is not on the allow list"
        for p in packages
        if not p.get("license") or not license_ids(p["license"]) & allowed
    ]
    for problem in problems:
        print(f"notices: {problem}", file=sys.stderr)

    if args.notices:
        bare = write_notices(args.notices, packages, base_copyright(args.base_txz))
        print(f"notices: {len(packages)} crates -> {args.notices}", file=sys.stderr)
        for name in bare:
            print(f"notices: {name} ships no license file; listed by SPDX id only", file=sys.stderr)
    if args.sbom:
        version = next(p["version"] for p in meta["packages"] if p["id"] in meta["workspace_members"])
        write_sbom(args.sbom, packages, args.package, version)
        print(f"notices: SBOM -> {args.sbom}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
