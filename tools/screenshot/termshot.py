#!/usr/bin/env python3
"""termshot: run terminal commands and save what they printed as a PNG.

Used for manual testing, so reviewers can see exactly what was run and what
came back (see .claude/skills/manual-test). Each step is a shell command; its
colored output is captured and drawn as a terminal session.

    tools/screenshot/termshot.py --out build/screenshots/approve.png \\
        --title "Approving a batched send" --vm --user demo -- \\
        "glassctl approvals" "glassctl approve 1a2b3c4d"

  --vm           run the steps in the FreeBSD dev VM (os/freebsd/scripts/vm.sh)
  --user NAME    in the VM, run as this user (default: root)
  --width PX     image width (default 1100)

Needs a Chromium headless shell (Playwright's, or set CHROME) and, for
trimming, Pillow. Output is what the commands really printed; nothing is
edited after the fact.
"""

import argparse
import base64
import glob
import html
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
VM = REPO / "os/freebsd/scripts/vm.sh"

# 16-color palette tuned for a dark background.
COLORS = {
    30: "#5c6370", 31: "#ef6b73", 32: "#6fd39a", 33: "#f0a24a", 34: "#7aa2ff",
    35: "#c792ea", 36: "#56c8d8", 37: "#d6dbe1",
    90: "#7f8794", 91: "#ff8c94", 92: "#8ee6b0", 93: "#ffc06b", 94: "#9db4ff",
    95: "#dcb0ff", 96: "#7fdde8", 97: "#ffffff",
}
SGR = re.compile(r"\x1b\[([0-9;]*)m")
OTHER_ESCAPES = re.compile(r"\x1b\[[0-9;?]*[A-Za-ln-z]")


def chrome() -> str:
    candidates = [os.environ.get("CHROME", "")]
    candidates += sorted(glob.glob("/opt/pw-browsers/chromium_headless_shell-*/*/headless_shell"))
    candidates += ["headless_shell", "chromium-headless-shell", "chromium", "google-chrome"]
    for candidate in filter(None, candidates):
        path = shutil.which(candidate) or (candidate if Path(candidate).is_file() else None)
        if path:
            return path
    sys.exit("termshot: no Chromium found; set CHROME to a headless_shell or chromium binary")


def xterm256(n: int) -> str:
    """The xterm 256-color palette: 16 base colors, a 6x6x6 cube, grays."""
    if n < 8:
        return COLORS[30 + n]
    if n < 16:
        return COLORS[90 + n - 8]
    if n < 232:
        levels = [0, 95, 135, 175, 215, 255]
        n -= 16
        return "#{:02x}{:02x}{:02x}".format(levels[n // 36], levels[n // 6 % 6], levels[n % 6])
    gray = 8 + 10 * (n - 232)
    return f"#{gray:02x}{gray:02x}{gray:02x}"


# Terminals draw block elements as solid cells; fonts leave hairline seams
# between them. Paint the common ones as backgrounds instead.
BLOCK_FILLS = {
    "█": "{c}",
    "▀": "linear-gradient({c} 50%, {b} 50%)",
    "▄": "linear-gradient({b} 50%, {c} 50%)",
    "▌": "linear-gradient(90deg, {c} 50%, {b} 50%)",
    "▐": "linear-gradient(90deg, {b} 50%, {c} 50%)",
}


def blocks_to_cells(text: str, color: str, background: str) -> str:
    return "".join(
        f'<span class="cell" style="background:{BLOCK_FILLS[ch].format(c=color, b=background)}"></span>'
        for ch in text
    )


def ansi_to_html(text: str) -> str:
    """Converts SGR color/weight codes to spans; drops other escapes."""
    text = OTHER_ESCAPES.sub("", text).replace("\r", "")
    out, style = [], {"fg": None, "bg": None, "bold": False, "dim": False}
    pos = 0

    def emit(chunk: str) -> None:
        if not chunk:
            return
        fg_color = style["fg"] or "#d6dbe1"
        for run in re.findall(f"[{''.join(BLOCK_FILLS)}]+|[^{''.join(BLOCK_FILLS)}]+", chunk):
            if run[0] in BLOCK_FILLS:
                # Cells carry their own colors, background included.
                out.append(blocks_to_cells(run, fg_color, style["bg"] or "transparent"))
                continue
            css = []
            if style["fg"]:
                css.append(f"color:{style['fg']}")
            if style["bg"]:
                css.append(f"background:{style['bg']}")
            if style["bold"]:
                css.append("font-weight:700")
            if style["dim"]:
                css.append("opacity:.6")
            escaped = html.escape(run)
            out.append(f'<span style="{";".join(css)}">{escaped}</span>' if css else escaped)

    for match in SGR.finditer(text):
        emit(text[pos : match.start()])
        pos = match.end()
        codes = [int(c) for c in match.group(1).split(";") if c] or [0]
        while codes:
            code = codes.pop(0)
            if code in (38, 48) and codes[:1] == [5] and len(codes) >= 2:
                style["fg" if code == 38 else "bg"] = xterm256(codes[1])
                del codes[:2]
            elif code in (38, 48) and codes[:1] == [2] and len(codes) >= 4:
                style["fg" if code == 38 else "bg"] = "#{:02x}{:02x}{:02x}".format(*codes[1:4])
                del codes[:4]
            elif code == 49:
                style["bg"] = None
            elif code == 0:
                style = {"fg": None, "bg": None, "bold": False, "dim": False}
            elif code == 1:
                style["bold"] = True
            elif code == 2:
                style["dim"] = True
            elif code == 22:
                style["bold"] = style["dim"] = False
            elif code == 39:
                style["fg"] = None
            elif code in COLORS:
                style["fg"] = COLORS[code]
    emit(text[pos:])
    return "".join(out)


def run_step(command: str, vm: bool, user: str) -> str:
    # Exported, so every command in a compound step (a; b | c) sees it.
    env_prefix = "export CLICOLOR_FORCE=1 COLUMNS=110;"
    if not vm:
        result = subprocess.run(
            ["bash", "-c", f"{env_prefix} {command}"],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, cwd=REPO,
        )
        return result.stdout
    script = base64.b64encode(f"{env_prefix} {command}\n".encode()).decode()
    if user == "root":
        remote = f"printf %s '{script}' | b64decode -r | sh -s 2>&1"
    else:
        remote = f"printf %s '{script}' | b64decode -r | su {user} -c 'cd && sh -s' 2>&1"
    result = subprocess.run([str(VM), "run", remote], capture_output=True, text=True)
    return result.stdout + (result.stderr if result.returncode and not result.stdout else "")


def render(title: str, steps: list[tuple[str, str]], prompt: str, width: int, out: Path) -> None:
    blocks = []
    for command, output in steps:
        blocks.append(
            f'<div class="cmd"><span class="prompt">{html.escape(prompt)}</span> {html.escape(command)}</div>'
            f'<div class="out">{ansi_to_html(output.rstrip())}</div>'
        )
    lines = sum(2 + output.count("\n") + len(output) // 110 for _, output in steps)
    page = f"""<!doctype html><html><head><meta charset="utf-8"><style>
      body {{ margin:0; background:#0b0d10; }}
      .win {{ margin:24px; background:#15181d; border-radius:12px; box-shadow:0 12px 40px rgba(0,0,0,.45); overflow:hidden; }}
      .bar {{ height:38px; background:#1e2228; display:flex; align-items:center; padding:0 14px; gap:8px;
              font:600 13px 'DejaVu Sans', sans-serif; color:#a9b1bc; }}
      .dot {{ width:11px; height:11px; border-radius:50%; background:#3a414b; }}
      .title {{ margin-left:10px; }}
      .term {{ padding:16px 20px 20px; font:15px/1.164 'DejaVu Sans Mono', 'Liberation Mono', monospace;
               color:#d6dbe1; white-space:pre-wrap; word-break:break-word; }}
      .cell {{ display:inline-block; width:1ch; height:1.164em; vertical-align:top; }}
      .cmd {{ color:#ffffff; margin-top:6px; }}
      .prompt {{ color:#6fd39a; }}
      .out {{ margin-bottom:12px; }}
    </style></head><body><div class="win"><div class="bar"><span class="dot"></span><span class="dot"></span>
    <span class="dot"></span><span class="title">{html.escape(title)}</span></div>
    <div class="term">{''.join(blocks)}</div></div></body></html>"""
    height = 140 + int(lines * 17.5)
    with tempfile.TemporaryDirectory() as tmp:
        page_path = Path(tmp) / "shot.html"
        page_path.write_text(page)
        out.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [chrome(), "--headless", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
             f"--window-size={width},{height}", f"--screenshot={out}", page_path.as_uri()],
            check=True, capture_output=True,
        )
    trim(out)


def trim(png: Path) -> None:
    """Crops empty background below the window, when Pillow is available."""
    try:
        from PIL import Image
    except ImportError:
        return
    image = Image.open(png).convert("RGB")
    background = image.getpixel((2, image.height - 2))
    bottom = image.height
    for y in range(image.height - 1, 0, -1):
        row = [image.getpixel((x, y)) for x in range(0, image.width, 7)]
        if any(px != background for px in row):
            bottom = min(image.height, y + 24)
            break
    image.crop((0, 0, image.width, bottom)).save(png)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--title", default="glasshouse")
    parser.add_argument("--vm", action="store_true")
    parser.add_argument("--user", default="root")
    parser.add_argument("--width", type=int, default=1100)
    parser.add_argument("steps", nargs="+")
    args = parser.parse_args()
    where = "glasshouse" if args.vm else "dev-host"
    prompt = f"{args.user if args.vm else os.environ.get('USER', 'dev')}@{where} $"
    steps = [(command, run_step(command, args.vm, args.user)) for command in args.steps]
    render(args.title, steps, prompt, args.width, args.out)
    print(args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
