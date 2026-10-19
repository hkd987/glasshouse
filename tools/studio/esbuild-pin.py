#!/usr/bin/env python3
"""Prints the esbuild the apps' lockfile pins for a platform: version, URL
and SHA-512 (hex), one per line, so the computer bundles people's apps with
the esbuild that builds ours (ADR 0032).

Usage: esbuild-pin.py apps/package-lock.json freebsd-x64
"""

import base64
import json
import sys


def main() -> int:
    lock_path, platform = sys.argv[1], sys.argv[2]
    with open(lock_path, encoding="utf-8") as f:
        packages = json.load(f)["packages"]
    pin = packages.get(f"node_modules/@esbuild/{platform}")
    main_version = packages.get("node_modules/esbuild", {}).get("version")
    if not pin or not pin.get("integrity", "").startswith("sha512-"):
        print(f"the lockfile pins no esbuild for {platform}", file=sys.stderr)
        return 1
    if pin["version"] != main_version:
        print(f"esbuild {main_version} but @esbuild/{platform} {pin['version']}", file=sys.stderr)
        return 1
    digest = base64.b64decode(pin["integrity"].removeprefix("sha512-")).hex()
    print(pin["version"])
    print(pin["resolved"])
    print(digest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
