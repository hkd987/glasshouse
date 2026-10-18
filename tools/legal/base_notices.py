#!/usr/bin/env python3
"""Notices and the source offer for the base system's imported code
(BR-2, BR-3).

The base system's top-level COPYRIGHT covers its own code. The code it
imports (contrib/, crypto/, cddl/, sys/contrib/ and the like) carries its
own licenses, and some of them, such as the CDDL (OpenZFS, DTrace) and the
GNU licenses, require the source to be offered with the binaries. This
reads the release's src.txz and writes:

  os/freebsd/legal/base-notices.txt   every imported component's license
                                      texts, identical texts once
  os/freebsd/legal/source-offer.txt   the components under copyleft
                                      licenses, and how to get the source

Both are checked in, so building an image doesn't need the 250 MB source
archive. When the base release changes, `make legal-base` fetches src.txz
(checked against its pin in os/freebsd/release.env) and runs this with no
arguments: the release and the archive's cached path come from
release.env.

`--check` (used by `make image-release`) fails while the source offer still
has no legal contact, so no shippable image goes out without one.
"""

import argparse
import hashlib
import re
import sys
import tarfile
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RELEASE_ENV = REPO / "os/freebsd/release.env"
OUT = REPO / "os/freebsd/legal"
NOTICES = OUT / "base-notices.txt"
OFFER = OUT / "source-offer.txt"
CONTACT_PLACEHOLDER = "[LEGAL CONTACT: to be set before the first pilot image]"

LICENSE_FILE = re.compile(
    r"^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|OPENSOLARIS\.LICENSE)([._-][\w.-]*)?$", re.I
)
SKIP_DIRS = {"tests", "test", "testdata", "testsuite", "unittests", "examples"}
# Where imported code lives; the next path segment names the component.
IMPORT_ROOTS = (
    "contrib/",
    "crypto/",
    "cddl/contrib/",
    "sys/contrib/",
    "sys/cddl/contrib/",
    "sys/contrib/dev/",
)
# Most specific first: a text mentioning several picks the first match.
FAMILIES = [
    ("CDDL-1.0", re.compile(r"COMMON DEVELOPMENT AND DISTRIBUTION LICENSE", re.I)),
    ("LGPL", re.compile(r"GNU (LESSER|LIBRARY) GENERAL PUBLIC LICENSE", re.I)),
    ("GPL-3.0", re.compile(r"GNU GENERAL PUBLIC LICENSE\s+Version 3", re.I)),
    ("GPL-2.0", re.compile(r"GNU GENERAL PUBLIC LICENSE", re.I)),
    ("MPL", re.compile(r"Mozilla Public License", re.I)),
    ("EPL", re.compile(r"Eclipse Public License", re.I)),
    ("Apache-2.0", re.compile(r"Apache License,?\s+Version 2\.0", re.I)),
]
COPYLEFT = {"CDDL-1.0", "LGPL", "GPL-3.0", "GPL-2.0", "MPL", "EPL"}
# A GPL text next to a permissive one usually means "either, at your
# choice"; legal review confirms which copyleft entries really apply.
DUAL = re.compile(r"(alternatively|at your (option|choice)|dual[- ]licen[cs]ed|either)", re.I)


def component(rel: str) -> str:
    """The imported component a file belongs to, as a path under the source
    tree, such as `sys/contrib/openzfs`."""
    for root in sorted(IMPORT_ROOTS, key=len, reverse=True):
        if rel.startswith(root):
            rest = rel[len(root):].split("/")
            return root + rest[0] if len(rest) > 1 else root.rstrip("/")
    return str(Path(rel).parent)


def family(text: str) -> str:
    for name, pattern in FAMILIES:
        if pattern.search(text):
            return name
    return "permissive or other"


def collect(src_txz: Path) -> dict[str, list[tuple[str, str]]]:
    """Component → [(file under it, text)], read in one pass over the
    archive."""
    found: dict[str, list[tuple[str, str]]] = defaultdict(list)
    with tarfile.open(src_txz, mode="r|xz") as tar:
        for member in tar:
            if not member.isfile():
                continue
            parts = member.name.lstrip("./").split("/")
            if parts[:2] != ["usr", "src"] or len(parts) < 4:
                continue
            rel = "/".join(parts[2:])
            if not LICENSE_FILE.match(parts[-1]) or SKIP_DIRS & set(parts[2:-1]):
                continue
            data = tar.extractfile(member).read()
            text = data.decode("utf-8", errors="replace").strip()
            if text:
                comp = component(rel)
                found[comp].append((rel[len(comp) + 1:] if rel.startswith(comp + "/") else rel, text))
    return dict(sorted(found.items()))


def write_notices(found: dict, release: str) -> None:
    rule = "=" * 78
    texts: list[str] = []
    refs: dict[str, str] = {}
    out = [
        "Base system: imported components",
        "",
        f"From the {release} source release. Each component's license",
        "texts are reproduced as its license requires; identical texts are",
        "printed once and referenced by number.",
        "",
    ]
    for comp, files in found.items():
        names = sorted({family(t) for _, t in files})
        out.append(f"{comp}  ({', '.join(names)})")
        for name, text in files:
            if text not in refs:
                texts.append(text)
                refs[text] = f"[B{len(texts)}]"
            out.append(f"  {name}: license text {refs[text]}")
        out.append("")
    out += [rule, "Base system license texts", rule, ""]
    for i, text in enumerate(texts, 1):
        out += [f"[B{i}]", "", text, "", "-" * 78, ""]
    NOTICES.write_text("\n".join(out) + "\n")


def write_offer(found: dict, release: str, sha256: str) -> list[tuple[str, str, bool]]:
    rows = []
    for comp, files in found.items():
        families = {family(t) for _, t in files}
        copyleft = sorted(families & COPYLEFT)
        if copyleft:
            texts = " ".join(t for _, t in files)
            dual = bool(DUAL.search(texts)) and "permissive or other" in families
            rows.append((comp, ", ".join(copyleft), dual))
    width = max(len(c) for c, _, _ in rows)
    out = [
        "Source code offer",
        "",
        "The base system includes components under licenses that require",
        "their source code to be available with the binaries, such as the",
        "Common Development and Distribution License (CDDL) and the GNU",
        "licenses. The complete corresponding source of the base system,",
        "including every component below, is the unmodified source release",
        "this software was built from:",
        "",
        f"  {release} src.txz, SHA-256 {sha256}",
        "",
        "For at least three years from the date you received this software,",
        "the source is available on request, on a physical medium or as a",
        "download, for no more than the cost of providing it. Write to:",
        "",
        f"  {CONTACT_PLACEHOLDER}",
        "",
        "Components (* = the license text also offers a permissive choice):",
        "",
    ]
    out += [f"  {c:<{width}}  {lic}{' *' if dual else ''}" for c, lic, dual in rows]
    OFFER.write_text("\n".join(out) + "\n")
    return rows


def pinned_release() -> str:
    for line in RELEASE_ENV.read_text().splitlines():
        if line.startswith("FREEBSD_RELEASE="):
            return line.split("=", 1)[1].strip()
    sys.exit(f"base-notices: no FREEBSD_RELEASE in {RELEASE_ENV}")


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("src_txz", type=Path, nargs="?",
                    help="default: the cached src.txz of the pinned release")
    ap.add_argument("--manifest", type=Path, help="the release MANIFEST, to check src.txz")
    ap.add_argument("--release", help="default: FREEBSD_RELEASE from release.env")
    ap.add_argument("--check", action="store_true",
                    help="fail if the notices are missing or the offer has no contact")
    args = ap.parse_args()

    if args.check:
        problems = [f"{p.relative_to(REPO)} is missing" for p in (NOTICES, OFFER) if not p.exists()]
        if OFFER.exists() and CONTACT_PLACEHOLDER in OFFER.read_text():
            problems.append(f"{OFFER.relative_to(REPO)} has no legal contact yet (BR-3)")
        for problem in problems:
            print(f"base-notices: {problem}", file=sys.stderr)
        return 1 if problems else 0

    args.release = args.release or pinned_release()
    if args.src_txz is None:
        args.src_txz = REPO / "build/cache/freebsd" / args.release / "src.txz"
    if not args.src_txz.exists():
        sys.exit(f"base-notices: no {args.src_txz}; run os/freebsd/scripts/fetch.sh src")
    sha256 = sha256_of(args.src_txz)
    if args.manifest:
        listed = next((line.split("\t")[1] for line in args.manifest.read_text().splitlines()
                       if line.startswith("src.txz\t")), None)
        if listed != sha256:
            sys.exit(f"base-notices: {args.src_txz} doesn't match the MANIFEST ({listed})")
    found = collect(args.src_txz)
    OUT.mkdir(parents=True, exist_ok=True)
    write_notices(found, args.release)
    rows = write_offer(found, args.release, sha256)
    texts = sum(len(f) for f in found.values())
    print(f"base-notices: {len(found)} components, {texts} license files, "
          f"{len(rows)} under copyleft licenses ({sum(d for *_, d in rows)} with a permissive choice)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
