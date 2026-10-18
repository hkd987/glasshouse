#!/usr/bin/env python3
"""Glasshouse's terminal art: one source for every screen that shows it (BR-1).

Writes into the image overlay (os/freebsd/overlay/usr/local/share/glasshouse/
branding/):

  issue                the console's pre-login screen (/etc/issue)
  motd                 the message after every login (/etc/motd)
  gfx-glasshouse.lua   the boot loader's brand and logo (ASCII only: the
                       loader can't draw Unicode on a serial console)
  splash.png           the graphical boot splash while the kernel starts
  logo.png             the boot menu's logo on graphical consoles

    tools/branding/art.py            regenerate
    tools/branding/art.py --check    fail if the overlay is out of date (lint)
    tools/branding/art.py --preview  print the terminal art here

The product name is still an open question in the PRD, so the name lives
only here (WORD_*): change it, regenerate, done. The PNGs need Pillow; the
text files don't.
"""

import argparse
import io
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
OUT = REPO / "os/freebsd/overlay/usr/local/share/glasshouse/branding"

ESC = "\033["
RESET = f"{ESC}0m"


def fg(n: int, bold: bool = False) -> str:
    """A 256-color foreground. 256 colors (not 24-bit) because the console's
    terminal emulator maps them down sensibly; 24-bit it doesn't know."""
    return f"{ESC}{'1;' if bold else ''}38;5;{n}m"


# The palette, from the mockups' tokens: white frames, glass blues, the
# working-agent accent (#3D6BFF ~ 63) and the needs-you orange (#F08A24 ~ 208).
DIM = fg(245)
TEXT = fg(252)
ACCENT = fg(75, bold=True)
WORD_GRADIENT = [231, 195, 159, 123, 117, 81, 75]
WORD_SHADOW = fg(25)

# The mark is a window of four panes with the lower right one filled: the
# Boot mockup's 104-unit SVG, rasterized here so the console, the boot menu
# and the splash all draw the same shape. White on black, like the mockup.
PIXEL_COLORS = {
    "W": 255,  # the mark
}
MARK_UNITS = 104.0


def _in_rounded(x: int, y: int, lo: int, hi: int, radius: float) -> bool:
    """Whether pixel (x, y)'s center is inside the square lo..hi (inclusive)
    with corners rounded to `radius`."""
    if not (lo <= x <= hi and lo <= y <= hi):
        return False
    cx = min(max(x + 0.5, lo + radius), hi + 1 - radius)
    cy = min(max(y + 0.5, lo + radius), hi + 1 - radius)
    return (x + 0.5 - cx) ** 2 + (y + 0.5 - cy) ** 2 <= radius * radius


def mark_pixels(size: int = 22, stroke: int = 2, radius: int = 6) -> list[str]:
    """The mark as pixel art on a size x size grid ('.' is transparent),
    drawn on whole pixels so it stays crisp in a terminal: a rounded frame,
    a 2-pixel cross that stops a pixel short of it, and the lower right pane
    filled, a pixel clear of the cross, its outer corner rounded off."""
    mid = size // 2 - 1  # the cross is columns/rows mid and mid + 1
    near, far = stroke + 1, size - 2 - stroke  # the cross's ends
    rows = []
    for y in range(size):
        row = ""
        for x in range(size):
            frame = _in_rounded(x, y, 0, size - 1, radius) and not _in_rounded(
                x, y, stroke, size - 1 - stroke, max(radius - stroke, 1)
            )
            cross = (mid <= x <= mid + 1 and near <= y <= far) or (mid <= y <= mid + 1 and near <= x <= far)
            pane = mid + 3 <= x <= far and mid + 3 <= y <= far and (x, y) != (far, far)
            row += "W" if frame or cross or pane else "."
        rows.append(row)
    return rows


def pixels_to_cells(pixels: list[str]) -> list[str]:
    """Two pixel rows per text row: the upper half is the foreground of '▀'
    (or '▄' for the lower half alone), the other half the background."""
    if len(pixels) % 2:
        pixels = pixels + ["." * len(pixels[0])]
    out = []
    for top, bottom in zip(pixels[0::2], pixels[1::2]):
        line, state = "", (None, None)
        for t, b in zip(top, bottom):
            tc, bc = PIXEL_COLORS.get(t), PIXEL_COLORS.get(b)
            if tc is None and bc is None:
                want, ch = (None, None), " "
            elif tc == bc or bc is None:
                want, ch = (tc, None), "█" if tc == bc else "▀"
            elif tc is None:
                want, ch = (bc, None), "▄"
            else:
                want, ch = (tc, bc), "▀"
            if want != state:
                line += RESET if want == (None, None) else (
                    f"{ESC}0;38;5;{want[0]}" + (f";48;5;{want[1]}" if want[1] is not None else "") + "m"
                )
                state = want
            line += ch
        out.append(line.rstrip() + RESET)
    return out


# ASCII for the boot loader (serial consoles can't draw the blocks above).
MARK_ASCII = [
    " .--------+--------. ",
    " |        |        | ",
    " |        |        | ",
    " |        |        | ",
    " +--------+--------+ ",
    " |        |########| ",
    " |        |########| ",
    " |        |#######/  ",
    " '--------+------'   ",
]

WORD_BIG = [
    " ██████╗ ██╗      █████╗ ███████╗███████╗",
    "██╔════╝ ██║     ██╔══██╗██╔════╝██╔════╝",
    "██║  ███╗██║     ███████║███████╗███████╗",
    "██║   ██║██║     ██╔══██║╚════██║╚════██║",
    "╚██████╔╝███████╗██║  ██║███████║███████║",
    " ╚═════╝ ╚══════╝╚═╝  ╚═╝╚══════╝╚══════╝",
    "██╗  ██╗ ██████╗ ██╗   ██╗███████╗███████╗",
    "██║  ██║██╔═══██╗██║   ██║██╔════╝██╔════╝",
    "███████║██║   ██║██║   ██║███████╗█████╗  ",
    "██╔══██║██║   ██║██║   ██║╚════██║██╔══╝  ",
    "██║  ██║╚██████╔╝╚██████╔╝███████║███████╗",
    "╚═╝  ╚═╝ ╚═════╝  ╚═════╝ ╚══════╝╚══════╝",
]

# figlet "pagga": the compact wordmark for the login message.
WORD_SMALL = [
    "░█▀▀░█░░░█▀█░█▀▀░█▀▀░█░█░█▀█░█░█░█▀▀░█▀▀",
    "░█░█░█░░░█▀█░▀▀█░▀▀█░█▀█░█░█░█░█░▀▀█░█▀▀",
    "░▀▀▀░▀▀▀░▀░▀░▀▀▀░▀▀▀░▀░▀░▀▀▀░▀▀▀░▀▀▀░▀▀▀",
]

# figlet "slant" for the boot loader's brand.
WORD_ASCII = [
    r"          __                __",
    r"   ____ _/ /___ ___________/ /_  ____  __  __________",
    r"  / __ `/ / __ `/ ___/ ___/ __ \/ __ \/ / / / ___/ _ \ ",
    r" / /_/ / / /_/ (__  |__  ) / / / /_/ / /_/ (__  )  __/",
    r" \__, /_/\__,_/____/____/_/ /_/\____/\__,_/____/\___/",
    r"/____/",
]

TAGLINE = "Agents you can see, stop and undo."


def visible(s: str) -> int:
    return len(re.sub(r"\033\[[0-9;]*m", "", s))


def color_big_word(lines: list[str]) -> list[str]:
    """Solid blocks in a top-to-bottom gradient per word; the box-drawing
    shadow in deep blue, so the letters stand off the screen."""
    out = []
    for r, line in enumerate(lines):
        shade = fg(WORD_GRADIENT[min(r % 6, len(WORD_GRADIENT) - 1)], bold=True)
        s, last = "", None
        for ch in line:
            code = shade if ch == "█" else (WORD_SHADOW if ch != " " else last)
            if code != last and ch != " ":
                s += code
                last = code
            s += ch
        out.append(s + RESET)
    return out


def color_small_word(lines: list[str]) -> list[str]:
    """The pagga letters bright over a dim glass texture (its ░)."""
    out = []
    for r, line in enumerate(lines):
        shade = fg(WORD_GRADIENT[r * 2 + 1], bold=True)
        s, last = "", None
        for ch in line:
            code = DIM if ch == "░" else shade
            if code != last:
                s += code
                last = code
            s += ch
        out.append(s + RESET)
    return out


def beside(left: list[str], right: list[str], gap: int = 3, top: int = 0) -> list[str]:
    width = max(visible(l) for l in left)
    rows = max(len(left), len(right) + top)
    out = []
    for i in range(rows):
        l = left[i] if i < len(left) else ""
        r = right[i - top] if 0 <= i - top < len(right) else ""
        out.append((l + " " * (width - visible(l) + gap) + r).rstrip())
    return out


CLEAR = f"{ESC}H{ESC}2J"


def issue() -> str:
    """Before login on the console: a clean screen (no startup messages
    above it) and the full mark."""
    mark = pixels_to_cells(mark_pixels(22))
    word = color_big_word(WORD_BIG)
    art = beside(mark, word, gap=4)
    tag = " " * (max(visible(l) for l in mark) + 4) + DIM + TAGLINE + RESET
    return CLEAR + "\n" + "\n".join(art) + "\n" + tag + "\n\n"


def motd() -> str:
    """After every login: the compact mark, then where to start. Shows the
    breadth of the OS, not one workflow."""
    mark = pixels_to_cells(mark_pixels(14, stroke=1, radius=4))
    right = color_small_word(WORD_SMALL) + [
        "",
        TEXT + TAGLINE + RESET,
        "",
        f"{ACCENT}glassctl agents{RESET}   {DIM}agents you can hand work to{RESET}",
        f"{ACCENT}glassctl run{RESET}      {DIM}hand one a task, in plain words{RESET}",
        f"{ACCENT}glassctl tasks{RESET}    {DIM}watch them; pause, take over, undo{RESET}",
    ]
    art = beside(mark, right, gap=3)
    return "\n" + "\n".join(art) + "\n\n"


def loader_color(line: str, esc: str = "\\027[") -> str:
    """The loader understands 8-color ANSI: the mark in bright white."""
    out, last = "", None
    for ch in line:
        code = "1;37" if ch != " " else last
        if code != last:
            out += f"{esc}{code}m"
            last = code
        out += ch
    return out + f"{esc}0m"


def loader_preview() -> str:
    """How the boot menu's art looks on a color console."""
    logo = [loader_color(l, ESC) for l in MARK_ASCII]
    brand = [f"{ESC}1;36m{l}{RESET}" for l in WORD_ASCII]
    return "\n".join(brand) + "\n\n" + "\n".join(" " * 44 + l for l in logo) + "\n"


def loader_lua() -> str:
    # Lua strings: the art's backslashes escaped first, then the colors.
    brand = "\n".join(
        f'				"\\027[1;36m{l.replace(chr(92), chr(92) * 2)}\\027[0m",' for l in WORD_ASCII
    )
    logo = [loader_color(l.replace("\\", "\\\\")) for l in MARK_ASCII]
    return f"""-- Generated by tools/branding/art.py; edit that, not this.
-- The boot menu's brand and logo (BR-1), installed into /boot/lua by
-- post-install.sh and chosen with loader_brand and loader_logo. Neither sets
-- requires_color: without color the loader would swap in its own logo.
return {{
	brand = {{
		ascii = {{
			image = {{
{brand}
			}},
		}},
	}},
	logo = {{
		ascii = {{
			image = {{
{chr(10).join(f'				"{l}",' for l in logo)}
			}},
			shift = {{x = 3, y = 0}},
		}},
		fb = {{
			image = "/boot/images/glasshouse-logo.png",
			width = 22,
			shift = {{x = 3, y = 0}},
		}},
	}},
}}
"""


def mark_png(size: tuple[int, int], mark: int, background: tuple[int, int, int, int]) -> bytes:
    """The mark, antialiased, `mark` pixels wide and centered: the boot
    splash (white on black, as the Boot mockup) or the boot menu's logo."""
    from PIL import Image, ImageDraw

    ss = 4  # draw 4x and scale down for smooth edges
    big = Image.new("L", (mark * ss, mark * ss), 0)
    unit = mark * ss / MARK_UNITS
    draw = ImageDraw.Draw(big)
    draw.rounded_rectangle([3 * unit, 3 * unit, 101 * unit, 101 * unit], radius=27 * unit, fill=255)
    draw.rounded_rectangle([9 * unit, 9 * unit, 95 * unit, 95 * unit], radius=21 * unit, fill=0)
    draw.rectangle([49 * unit, 14 * unit, 55 * unit, 90 * unit], fill=255)
    draw.rectangle([14 * unit, 49 * unit, 90 * unit, 55 * unit], fill=255)
    draw.rectangle([58 * unit, 58 * unit, 70 * unit, 92 * unit], fill=255)
    draw.rectangle([58 * unit, 58 * unit, 86 * unit, 76 * unit], fill=255)
    draw.pieslice([54 * unit, 60 * unit, 86 * unit, 92 * unit], 0, 90, fill=255)
    alpha = big.resize((mark, mark), Image.Resampling.LANCZOS)
    img = Image.new("RGBA", size, background)
    ink = Image.new("RGBA", (mark, mark), (245, 247, 250, 255))
    img.paste(ink, ((size[0] - mark) // 2, (size[1] - mark) // 2), alpha)
    buf = io.BytesIO()
    img.save(buf, "PNG", optimize=True)
    return buf.getvalue()


def outputs(with_png: bool) -> dict[str, bytes]:
    files = {
        "issue": issue().encode(),
        "motd": motd().encode(),
        "gfx-glasshouse.lua": loader_lua().encode(),
    }
    if with_png:
        files["splash.png"] = mark_png((640, 400), 64, (0, 0, 0, 255))
        files["logo.png"] = mark_png((150, 138), 110, (0, 0, 0, 0))
    return files


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="fail if the overlay's text files are stale")
    ap.add_argument("--preview", action="store_true", help="print the terminal art")
    args = ap.parse_args()
    if args.preview:
        sys.stdout.write(issue() + motd() + "\n" + loader_preview())
        return 0
    if args.check:
        stale = [n for n, data in outputs(with_png=False).items()
                 if not (OUT / n).exists() or (OUT / n).read_bytes() != data]
        for name in stale:
            print(f"art: {OUT.relative_to(REPO)}/{name} is stale; run tools/branding/art.py", file=sys.stderr)
        return 1 if stale else 0
    OUT.mkdir(parents=True, exist_ok=True)
    for name, data in outputs(with_png=True).items():
        (OUT / name).write_bytes(data)
        print(f"art: wrote {OUT.relative_to(REPO)}/{name}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
