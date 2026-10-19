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
  --assets DIR     Also reproduce the licenses of non-crate assets shipped
                   from DIR (fonts: shell/assets/fonts), one section per
                   license file. Repeatable.
  --sbom FILE      A CycloneDX 1.5 JSON SBOM. Admins and customer security
                   teams only (BR-4): never stage it into the image or
                   publish it.
  --third-party LIST
                   Also list the third-party libraries and data staged from
                   packages (os/freebsd/images/desktop.third-party): in the
                   notices with their licenses (from --third-party-licenses,
                   the staged usr/local/share/licenses), and in the SBOM.
                   Their licenses must be on deny.toml's allow list too.
  --base-packages FILE
                   Also list the image's base packages in the SBOM (BS-6):
                   the inventory a package-list image build writes beside
                   the image (`<name> <version>` per line; `make
                   image-pkgbase`, ADR 0022).

    tools/legal/notices.py --package glassd --package glassctl \\
        --shell-package glass-greeter --assets shell/assets/fonts \\
        --notices build/legal/NOTICES.txt --sbom build/sbom/glasshouse.cdx.json

`--shell-package` names a binary of the desktop shell's own workspace
(shell/Cargo.toml); its crates join the root workspace's in one list.

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
# Asset licenses also include font licenses (OFL-Geist.txt).
ASSET_LICENSE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|OFL)", re.I)


def metadata(manifest: Path = REPO / "Cargo.toml") -> dict:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET,
         "--manifest-path", str(manifest)],
        cwd=REPO, check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


def shipped_closure(meta: dict, roots: list[str], manifest: Path = REPO / "Cargo.toml") -> list[dict]:
    """Third-party packages the `roots` binaries link for FreeBSD: their
    normal and build dependencies with the features actually enabled, as
    `cargo tree` resolves them (`cargo metadata`'s graph also lists optional
    dependencies only named by weak features, such as iced's wgpu backend).
    Path dependencies are ours, except vendored crates (shell/vendor)."""
    packages = {p["id"]: p for p in meta["packages"]}
    members = {packages[m]["name"] for m in meta["workspace_members"]}
    # A root may name a binary rather than its package (glass-hello is a
    # binary of glass-compositor): the package is what cargo tree takes.
    binaries = {
        t["name"]: packages[m]["name"]
        for m in meta["workspace_members"]
        for t in packages[m]["targets"]
        if "bin" in t["kind"]
    }
    roots = sorted({r if r in members else binaries.get(r, r) for r in roots})
    missing = [r for r in roots if r not in members]
    if missing:
        sys.exit(f"notices: not workspace packages or binaries: {', '.join(missing)}")
    out = subprocess.run(
        ["cargo", "tree", "--locked", "--manifest-path", str(manifest), "--target", TARGET,
         "-e", "normal,build", "--prefix", "none", "--format", "{p}",
         *[arg for r in roots for arg in ("-p", r)]],
        cwd=REPO, check=True, capture_output=True, text=True,
    ).stdout
    linked = set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].startswith("v"):
            linked.add((parts[0], parts[1][1:]))

    def ours(p: dict) -> bool:
        return p["source"] is None and "/vendor/" not in p["manifest_path"]

    return sorted(
        (p for p in packages.values() if (p["name"], p["version"]) in linked and not ours(p)),
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


def pkgbase_source(catalog: Path, sha256: str) -> list[str]:
    """For an image built from base packages (ADR 0022): the source
    packages in the same pinned catalog, which hold the complete
    corresponding source of every base package in it (BR-3). A base package
    gets a new version only when its files change, so the catalog's newest
    source tree is the one each package was built from."""
    data = subprocess.run(["tar", "-xOf", str(catalog), "data"], check=True, capture_output=True).stdout
    found = {p["name"]: p for p in json.loads(data)["packages"] if p["name"] in ("FreeBSD-src", "FreeBSD-src-sys")}
    if len(found) != 2:
        sys.exit(f"notices: {catalog} has no FreeBSD-src and FreeBSD-src-sys packages")
    lines = [f"  {name} {p['version']}, SHA-256 {p['sum']}" for name, p in sorted(found.items())]
    return lines + [f"  in the base package catalog with SHA-256 {sha256}"]


def source_offer(pkgbase: list[str] | None) -> str:
    """The source offer: for the release's src.txz, or for the source
    packages an image built from base packages came with."""
    offer = base_legal("source-offer.txt")
    if not offer or not pkgbase:
        return offer
    built_from = "is the unmodified source release\nthis software was built from:"
    txz = re.compile(r"^  \S+ src\.txz, SHA-256 [0-9a-f]{64}$", re.M)
    if offer.count(built_from) != 1 or len(txz.findall(offer)) != 1:
        sys.exit("notices: os/freebsd/legal/source-offer.txt changed shape; update source_offer()")
    offer = offer.replace(built_from, "is in the unmodified source packages\nthe base system's packages were built from:")
    return txz.sub(lambda _: "\n".join(pkgbase), offer)


def asset_licenses(dirs: list[Path]) -> list[tuple[str, str]]:
    """(label, text) for each license file in the asset directories."""
    found = []
    for d in dirs:
        if not d.is_dir():
            sys.exit(f"notices: no asset directory {d}")
        for f in sorted(d.iterdir()):
            if f.is_file() and ASSET_LICENSE.match(f.name):
                label = f.resolve().relative_to(REPO) if f.resolve().is_relative_to(REPO) else f
                found.append((str(label), f.read_text(errors="replace").strip()))
    return found


def third_party(list_path: Path, licenses: Path | None) -> list[dict]:
    """The staged third-party packages: name, version, license and the
    license texts each package installs (catalog.mk names the license;
    LICENSE only points at the texts)."""
    found = []
    for n, line in enumerate(list_path.read_text().splitlines(), 1):
        fields = line.split("#", 1)[0].split()
        if not fields:
            continue
        name, _, version = fields[0].rpartition("-")
        if not name or len(fields) < 2:
            sys.exit(f"notices: {list_path}:{n}: expected '<name>-<version> <path>...'")
        entry = {"name": name, "version": version, "license": None, "texts": []}
        folder = licenses / fields[0] if licenses else None
        if folder and folder.is_dir():
            catalog = (folder / "catalog.mk").read_text() if (folder / "catalog.mk").exists() else ""
            ids = re.search(r"^_LICENSE=(.*)$", catalog, re.M)
            entry["license"] = ids.group(1).strip() if ids else None
            entry["texts"] = [
                (f.name, f.read_text(errors="replace").strip())
                for f in sorted(folder.iterdir())
                if f.is_file() and f.name not in ("catalog.mk", "LICENSE")
            ]
        elif licenses:
            sys.exit(f"notices: no license files for {fields[0]} in {licenses}")
        found.append(entry)
    return found


def write_notices(path: Path, packages: list[dict], base: str, assets: list[tuple[str, str]],
                  libraries: list[dict], offer: str) -> list[str]:
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
    if libraries:
        out += [rule, "Third-party libraries and data", rule, ""]
        for lib in libraries:
            out += [f"{lib['name']} {lib['version']}", f"  License: {lib['license'] or 'see below'}"]
            for name, text in lib["texts"]:
                if text not in refs:
                    texts.append(text)
                    refs[text] = f"[{len(texts)}]"
                out.append(f"  {name}: license text {refs[text]}")
            out.append("")
    if assets:
        out += [rule, "Fonts and other assets", rule, ""]
        for label, text in assets:
            out += [label, "", text, "", "-" * 78, ""]
    out += [rule, "License texts", rule, ""]
    for i, text in enumerate(texts, 1):
        out += [f"[{i}]", "", text, "", "-" * 78, ""]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(out) + "\n")
    return bare


def base_packages(path: Path) -> list[tuple[str, str]]:
    """The image's base packages, by name and version, from its inventory."""
    found = []
    for n, line in enumerate(path.read_text().splitlines(), 1):
        fields = line.split()
        if len(fields) != 2:
            sys.exit(f"notices: {path}:{n}: expected '<package> <version>'")
        found.append((fields[0], fields[1]))
    return found


def write_sbom(path: Path, packages: list[dict], roots: list[str], version: str,
               base: list[tuple[str, str]], libraries: list[dict]) -> None:
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
    # The base system, one component per package as the image installed it.
    components += [
        {
            "type": "operating-system" if name.startswith("FreeBSD-kernel") else "application",
            "name": name,
            "version": pkg_version,
            "purl": f"pkg:generic/{name}@{pkg_version}",
            "properties": [{"name": "glasshouse:layer", "value": "base"}],
        }
        for name, pkg_version in base
    ]
    components += [
        {
            "type": "library",
            "name": lib["name"],
            "version": lib["version"],
            "purl": f"pkg:generic/{lib['name']}@{lib['version']}",
            "licenses": [{"expression": lib["license"]}] if lib["license"] else [],
            "properties": [{"name": "glasshouse:layer", "value": "third-party"}],
        }
        for lib in libraries
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
    ap.add_argument("--shell-package", action="append", default=[],
                    help="a shipped binary of the shell workspace (shell/Cargo.toml)")
    ap.add_argument("--notices", type=Path)
    ap.add_argument("--assets", type=Path, action="append", default=[], help="a directory of shipped assets")
    ap.add_argument("--sbom", type=Path)
    ap.add_argument("--base-txz", type=Path, help="FreeBSD base.txz, for the base COPYRIGHT")
    ap.add_argument("--base-packages", type=Path, help="the image's base package inventory, for the SBOM")
    ap.add_argument("--third-party", type=Path, help="the staged third-party list (desktop.third-party)")
    ap.add_argument("--third-party-licenses", type=Path,
                    help="their staged license files (usr/local/share/licenses)")
    ap.add_argument("--pkgbase-catalog", type=Path,
                    help="for an image built from base packages: the pinned catalog (data.pkg), whose "
                    "source packages the source offer names")
    ap.add_argument("--pkgbase-catalog-sha256", help="that catalog's pinned SHA-256")
    args = ap.parse_args()
    if bool(args.pkgbase_catalog) != bool(args.pkgbase_catalog_sha256):
        ap.error("--pkgbase-catalog and --pkgbase-catalog-sha256 go together")

    meta = metadata()
    packages = shipped_closure(meta, args.package)
    if args.shell_package:
        manifest = REPO / "shell/Cargo.toml"
        shell = shipped_closure(metadata(manifest), args.shell_package, manifest)
        ours = {(p["name"], p["version"]) for p in packages}
        packages = sorted(
            packages + [p for p in shell if (p["name"], p["version"]) not in ours],
            key=lambda p: (p["name"], p["version"]),
        )
    allowed = allowed_licenses()
    problems = [
        f"{p['name']} {p['version']}: license {p.get('license')!r} is not on the allow list"
        for p in packages
        if not p.get("license") or not license_ids(p["license"]) & allowed
    ]
    libraries = third_party(args.third_party, args.third_party_licenses) if args.third_party else []
    problems += [
        f"{lib['name']} {lib['version']}: license {lib['license']!r} is not on the allow list"
        for lib in libraries
        if lib["license"] is not None and not license_ids(lib["license"]) & allowed
    ]
    for problem in problems:
        print(f"notices: {problem}", file=sys.stderr)

    if args.notices:
        assets = asset_licenses(args.assets)
        pkgbase = (pkgbase_source(args.pkgbase_catalog, args.pkgbase_catalog_sha256)
                   if args.pkgbase_catalog else None)
        bare = write_notices(args.notices, packages, base_copyright(args.base_txz), assets, libraries,
                             source_offer(pkgbase))
        print(f"notices: {len(packages)} crates -> {args.notices}", file=sys.stderr)
        for name in bare:
            print(f"notices: {name} ships no license file; listed by SPDX id only", file=sys.stderr)
    if args.sbom:
        version = next(p["version"] for p in meta["packages"] if p["id"] in meta["workspace_members"])
        base = base_packages(args.base_packages) if args.base_packages else []
        write_sbom(args.sbom, packages, args.package + args.shell_package, version, base, libraries)
        print(f"notices: SBOM -> {args.sbom} ({len(packages)} crates, {len(base)} base packages, "
              f"{len(libraries)} third-party)", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
