#!/usr/bin/env python3
"""bootshots: reboot the dev VM and record what its screen shows, frame by
frame, until the system is up. For checking boot screens (the loader menu,
the splash, the login screen) the way a user sees them.

    tools/screenshot/bootshots.py --out <dir> [--interval 0.5] [--no-reboot]

Writes each distinct frame as <dir>/NNN-<seconds>s.png and a contact sheet,
<dir>/sheet.png. Uses `vm.sh screendump` (the VM's QEMU monitor); boot the VM
with VM_FIRMWARE=uefi to see what UEFI machines show. Needs Pillow.
"""

import argparse
import hashlib
import subprocess
import sys
import time
from pathlib import Path

from PIL import Image

REPO = Path(__file__).resolve().parents[2]
VM = REPO / "os/freebsd/scripts/vm.sh"


def vm(*args: str, check: bool = False) -> bool:
    return subprocess.run([str(VM), *args], capture_output=True).returncode == 0 or (
        check and sys.exit(f"bootshots: vm.sh {' '.join(args)} failed")
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--interval", type=float, default=0.5)
    ap.add_argument("--timeout", type=float, default=420)
    ap.add_argument("--no-reboot", action="store_true", help="record without rebooting first")
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)

    if not args.no_reboot:
        vm("run", "shutdown -r now >/dev/null 2>&1 &", check=True)
        time.sleep(2)
    start, last, frames, seen_down = time.monotonic(), None, [], args.no_reboot
    tmp = args.out / ".frame.png"
    while time.monotonic() - start < args.timeout:
        if vm("screendump", str(tmp)):
            digest = hashlib.sha256(Image.open(tmp).tobytes()).hexdigest()
            if digest != last:
                last = digest
                name = args.out / f"{len(frames):03d}-{time.monotonic() - start:05.1f}s.png"
                tmp.rename(name)
                frames.append(name)
        elapsed = time.monotonic() - start
        if elapsed > 20 and int(elapsed) % 10 == 0:
            up = vm("run", "test -S /var/run/glasshouse/control.sock")
            seen_down = seen_down or not up
            if up and seen_down and elapsed > 40:
                time.sleep(5)
                vm("screendump", str(args.out / f"{len(frames):03d}-up.png"))
                frames.append(args.out / f"{len(frames):03d}-up.png")
                break
        time.sleep(args.interval)
    tmp.unlink(missing_ok=True)

    thumbs = [Image.open(f).convert("RGB") for f in frames]
    if thumbs:
        w, h = 400, int(400 * thumbs[0].height / thumbs[0].width)
        cols = 3
        sheet = Image.new("RGB", (cols * (w + 8), ((len(thumbs) + cols - 1) // cols) * (h + 8)), (30, 30, 30))
        for i, t in enumerate(thumbs):
            sheet.paste(t.resize((w, h)), ((i % cols) * (w + 8), (i // cols) * (h + 8)))
        sheet.save(args.out / "sheet.png")
    print(f"bootshots: {len(frames)} distinct frames in {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
