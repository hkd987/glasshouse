#!/usr/bin/env python3
"""third-party: keep an image's list of third-party files (os/freebsd/images/*.third-party) whole.

    third-party.py update <list> [--bump]
    third-party.py fetch <list>...

A list names, per FreeBSD package, the files an image takes from it (`<name>-<version> <path>...`,
stage-dist.sh copies them). Above the line `# generated below` a person writes what the image
needs; this fills in, below it, every shared library those files link, from the packages they come
from, until all that's left is the base system. A library a program loads at run time (NSS's
modules) isn't linked, so the list names it by hand and its own libraries are filled in too.

--bump first moves every hand-written line to the version the ports catalog has now. Packages are
fetched into the cache (ports.sh fetch) as needed; `fetch` fetches every package lists name that
the cache doesn't hold yet (stage-dist.sh runs it). The generated lines carry each package's
licenses as the catalog states them; notices.py reads the license files themselves.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
MARK = "# generated below"


def env():
    """release.env's values, as lib.sh sees them."""
    out = subprocess.run(
        ["bash", "-c", f'. "{HERE}/lib.sh"; echo "$CACHE_DIR"; echo "$FREEBSD_PORTS_REPO"'],
        capture_output=True, text=True, check=True).stdout.split("\n")
    return out[0], out[1]


def catalog(cache, repo):
    data = os.path.join(cache, "ports", repo, "data.pkg")
    if not os.path.exists(data):
        sys.exit(f"third-party: no ports catalog at {data} (ports.sh fetch <package> fetches it)")
    out = subprocess.run(["tar", "-xOf", data, "data"], capture_output=True, check=True).stdout
    return {p["name"]: p for p in json.loads(out)["packages"]}


def split(nv, by):
    """`libxkbcommon-1.13.2` -> (`libxkbcommon`, `1.13.2`): the longest name the catalog has."""
    parts = nv.split("-")
    for i in range(len(parts) - 1, 0, -1):
        name = "-".join(parts[:i])
        if name in by:
            return name, "-".join(parts[i:])
    sys.exit(f"third-party: no package for {nv} in the catalog")


class Packages:
    """Package files in the ports cache, fetched on demand, unpacked into a scratch folder."""

    def __init__(self, cache, repo, by, work):
        self.all = os.path.join(cache, "ports", repo, "All")
        self.by, self.work = by, work
        self.whole = set()

    def file(self, name, version):
        for _ in range(2):
            for root, _, files in os.walk(self.all):
                for f in files:
                    if f == f"{name}-{version}.pkg" or f.startswith(f"{name}-{version}~"):
                        return os.path.join(root, f)
            subprocess.run([os.path.join(HERE, "ports.sh"), "fetch", name], capture_output=True)
        sys.exit(f"third-party: couldn't fetch {name}-{version}")

    def unpacked(self, name, version, members):
        """The package's files under a scratch root, `members` (paths) at least."""
        root = os.path.join(self.work, f"{name}-{version}")
        os.makedirs(root, exist_ok=True)
        want = [m for m in members if not os.path.lexists(os.path.join(root, m))]
        if want:
            subprocess.run(["tar", "-xf", self.file(name, version), "-C", root,
                            *[f"/{m}" for m in want]], capture_output=True)
        return root

    def libs(self, name, version):
        """The package's usr/local/lib, unpacked (all of it, whatever `unpacked` took)."""
        root = os.path.join(self.work, f"{name}-{version}")
        if root not in self.whole:
            self.whole.add(root)
            os.makedirs(root, exist_ok=True)
            subprocess.run(["tar", "-xf", self.file(name, version), "-C", root, "/usr/local/lib"],
                           capture_output=True)
        return root


def needed(path):
    out = subprocess.run(["readelf", "-d", path], capture_output=True, text=True).stdout
    return re.findall(r"\(NEEDED\)\s+Shared library: \[([^\]]+)\]", out)


def is_elf(path):
    try:
        with open(path, "rb") as f:
            return f.read(4) == b"\x7fELF"
    except OSError:
        return False


def update(path, bump):
    cache, repo = env()
    by = catalog(cache, repo)
    text = open(path).read().splitlines()
    # Everything above the marker line; the marker is a line of its own.
    end = next((i for i, l in enumerate(text) if l.startswith(MARK)), len(text))
    head = "\n".join(text[:end]).rstrip("\n") + "\n"
    # The hand-written lines: package, then paths.
    lines = []
    out_head = []
    for line in head.splitlines():
        body = line.split("#")[0].split()
        if not body:
            out_head.append(line)
            continue
        name, version = split(body[0], by)
        if bump and by[name]["version"] != version:
            print(f"{name}: {version} -> {by[name]['version']}", file=sys.stderr)
            line = line.replace(body[0], f"{name}-{by[name]['version']}", 1)
            version = by[name]["version"]
        lines.append((name, version, body[1:]))
        out_head.append(line)
    # Libraries come from the run-time dependencies of what's listed: what pkg would install.
    tree, todo = set(), [n for n, _, _ in lines]
    while todo:
        n = todo.pop()
        if n not in tree:
            tree.add(n)
            todo.extend((by[n].get("deps") or {}).keys())
    provider = {}
    for n in sorted(tree):
        for so in by[n].get("shlibs_provided") or []:
            provider.setdefault(so, n)
    listed = {(n, p) for n, _, paths in lines for p in paths}
    work = tempfile.mkdtemp(prefix="third-party-")
    try:
        pkgs = Packages(cache, repo, by, work)
        queue = []
        for name, version, paths in lines:
            root = pkgs.unpacked(name, version, paths)
            for p in paths:
                full = os.path.join(root, p)
                if os.path.isdir(full):
                    queue += [os.path.join(d, f) for d, _, fs in os.walk(full) for f in fs]
                else:
                    queue.append(full)
        found, base, seen = {}, set(), set()
        while queue:
            f = queue.pop()
            if os.path.islink(f) or not is_elf(f):
                continue
            for so in needed(f):
                if so in seen:
                    continue
                seen.add(so)
                name = provider.get(so)
                if name is None:
                    base.add(so)
                    continue
                version = by[name]["version"]
                root = pkgs.libs(name, version)
                link = os.path.join(root, "usr/local/lib", so)
                if not os.path.lexists(link):
                    sys.exit(f"third-party: {name}-{version} has no usr/local/lib/{so}")
                real = os.path.realpath(link)
                for p in {link, real}:
                    rel = os.path.relpath(p, root)
                    if (name, rel) not in listed:
                        found.setdefault((name, version), set()).add(rel)
                queue.append(real)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    out = out_head + ["", MARK + " (third-party.py update): the libraries the lines above link."]
    out.append(f"# From the base system: {' '.join(sorted(base))}")
    for (name, version), paths in sorted(found.items()):
        licenses = " ".join(by[name].get("licenses") or ["?"])
        logic = by[name].get("licenselogic", "single")
        note = f"  # {licenses}" if logic == "single" else f"  # {logic}: {licenses}"
        out.append(f"{name}-{version} {' '.join(sorted(paths))}{note}")
    open(path, "w").write("\n".join(out) + "\n")
    print(f"{path}: {len(found)} packages' libraries", file=sys.stderr)


def fetch(paths):
    cache, repo = env()
    by = catalog(cache, repo)
    pkgs = Packages(cache, repo, by, None)
    for path in paths:
        for line in open(path).read().splitlines():
            body = line.split("#")[0].split()
            if body:
                pkgs.file(*split(body[0], by))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    up = sub.add_parser("update", help="fill in the libraries a list's files link")
    up.add_argument("list")
    up.add_argument("--bump", action="store_true", help="move hand-written lines to the catalog's versions")
    fe = sub.add_parser("fetch", help="cache every package the lists name")
    fe.add_argument("lists", nargs="+")
    args = ap.parse_args()
    if args.cmd == "fetch":
        fetch(args.lists)
    else:
        update(args.list, args.bump)


if __name__ == "__main__":
    main()
