#!/usr/bin/env python3
"""Renders Design-canvas artboards (`*.dc.html`) to PNG with headless Chromium.

The mockups live in a Claude Design canvas (linked from the PRD). Fetch the
artboards with Claude's Artifact tool (see .claude/skills/prd-sync), then:

    tools/design/render-dc.py <dir-with-project/> <out-dir>

Each artboard's `{{prop}}` holes are filled with the defaults its
`data-props` declares; the editor runtime is dropped. Interactive behavior is
not rendered: these are pictures of the boards, for docs and reviews.
"""

import glob
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

def chrome() -> str:
    """Chromium's headless shell sizes the viewport exactly to --window-size;
    the full browser loses ~90px to window chrome, so prefer the shell."""
    candidates = [os.environ.get("CHROME", "")]
    candidates += sorted(glob.glob("/opt/pw-browsers/chromium_headless_shell-*/*/headless_shell"))
    candidates += ["headless_shell", "chromium-headless-shell", "chromium", "google-chrome"]
    for candidate in filter(None, candidates):
        path = shutil.which(candidate) or (candidate if Path(candidate).is_file() else None)
        if path:
            return path
    sys.exit("render-dc: no Chromium found; set CHROME to a headless_shell or chromium binary")


def to_html(source: str) -> tuple[str, int, int]:
    props = {}
    match = re.search(r"data-props='([^']*)'", source)
    if match:
        props = json.loads(match.group(1))
    width = props.get("$preview", {}).get("width", 1440)
    height = props.get("$preview", {}).get("height", 900)
    defaults = {k: v.get("default") for k, v in props.items() if isinstance(v, dict) and "default" in v}
    body = source[source.index("<x-dc>") + len("<x-dc>") : source.index("</x-dc>")]
    helmet = ""
    found = re.search(r"<helmet>(.*?)</helmet>", body, flags=re.S)
    if found:
        helmet = found.group(1)
        body = body.replace(found.group(0), "")
    body = re.sub(r"\{\{\s*([\w.]+)\s*\}\}", lambda m: str(defaults.get(m.group(1), "")), body)
    page = f"<!doctype html><html><head><meta charset='utf-8'>{helmet}</head><body style='margin:0'>{body}</body></html>"
    return page, width, height


def main() -> int:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    boards = sorted((src / "project").glob("*.dc.html")) or sorted(src.glob("*.dc.html"))
    with tempfile.TemporaryDirectory() as tmp:
        for board in boards:
            page, width, height = to_html(board.read_text())
            html_path = Path(tmp) / (board.stem + ".html")
            html_path.write_text(page)
            png = out / (board.name.removesuffix(".dc.html").lower() + ".png")
            subprocess.run(
                [chrome(), "--headless", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
                 f"--window-size={width},{height}", f"--screenshot={png}", html_path.as_uri()],
                check=True, capture_output=True,
            )
            print(f"{board.name} -> {png}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
