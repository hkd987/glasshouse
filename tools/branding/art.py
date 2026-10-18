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

# The house is pixel art: a glasshouse with a pitched roof, mullions, glass
# with reflections, and an agent at work inside, in plain sight. Two pixels
# per character cell (half blocks with a foreground and a background), so
# the roof is a clean 45 degrees in any terminal.
PIXEL_COLORS = {
    "W": 255,  # frame
    "M": 252,  # mullion
    "G": 25,  # glass
    "g": 32,  # glass, catching the light
    "A": 220,  # the agent (gold: stays yellow on 16-color consoles)
}


def house_pixels(roof: int = 11, body: int = 10) -> list[str]:
    """Rows of pixels, one character each ('.' is transparent)."""
    width = 2 * roof + 3
    mid = width // 2
    rows = []
    for r in range(roof):
        row = ["."] * width
        for x in range(mid - r, mid + r + 1):
            row[x] = "G"
        row[mid - r] = row[mid + r] = "W"
        if r >= 2:
            row[mid] = "M"
        rows.append(row)
    rows.append(["."] + ["W"] * (width - 2) + ["."])
    agent_row = len(rows) + body // 2 - 1
    for r in range(body):
        y = len(rows)
        row = ["."] + ["W"] + ["G"] * (width - 4) + ["W"] + ["."]
        for x in (mid - (width - 4) // 3, mid + (width - 4) // 3):
            row[x] = "M"
        if y in (agent_row - 1, agent_row + 1):
            row[mid] = "A"
        if y == agent_row:
            row[mid - 1 : mid + 2] = ["A"] * 3
        rows.append(row)
    rows.append(["."] + ["W"] * (width - 2) + ["."])
    # Reflections: diagonal streaks across the glass.
    for y, row in enumerate(rows):
        for x, px in enumerate(row):
            if px == "G" and (x + y) % 9 in (0, 1):
                row[x] = "g"
    return ["".join(r) for r in rows]


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
HOUSE_ASCII = [
    "          .          ",
    "         /|\\         ",
    "        /:|:\\        ",
    "       /::|::\\       ",
    "      /:::|:::\\      ",
    "     /::::|::::\\     ",
    "    /:::::|:::::\\    ",
    "   /______|______\\   ",
    "   |::|:::::::|::|   ",
    "   |::|:::@:::|::|   ",
    "   |::|:::::::|::|   ",
    "   |__|_______|__|   ",
]

# figlet "ANSI Shadow", stacked so it fits 80 columns beside the house.
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
    house = pixels_to_cells(house_pixels())
    word = color_big_word(WORD_BIG)
    art = beside(house, word, gap=4)
    tag = " " * (max(visible(l) for l in house) + 4) + DIM + TAGLINE + RESET
    return CLEAR + "\n" + "\n".join(art) + "\n" + tag + "\n\n"


def motd() -> str:
    """After every login: the compact mark, then where to start. Shows the
    breadth of the OS, not one workflow."""
    house = pixels_to_cells(house_pixels(roof=8, body=6))
    right = color_small_word(WORD_SMALL) + [
        "",
        TEXT + TAGLINE + RESET,
        "",
        f"{ACCENT}glassctl agents{RESET}   {DIM}agents you can hand work to{RESET}",
        f"{ACCENT}glassctl run{RESET}      {DIM}hand one a task, in plain words{RESET}",
        f"{ACCENT}glassctl tasks{RESET}    {DIM}watch them; pause, take over, undo{RESET}",
    ]
    art = beside(house, right, gap=3)
    return "\n" + "\n".join(art) + "\n\n"


def loader_color(line: str, esc: str = "\\027[") -> str:
    """The loader understands 8-color ANSI: white frame, blue glass, the
    agent in yellow."""
    out, last = "", None
    for ch in line:
        code = {":": "1;34", "@": "1;33"}.get(ch, "1;37") if ch != " " else last
        if code != last:
            out += f"{esc}{code}m"
            last = code
        out += ch
    return out + f"{esc}0m"


def loader_preview() -> str:
    """How the boot menu's art looks on a color console."""
    logo = [loader_color(l, ESC) for l in HOUSE_ASCII]
    brand = [f"{ESC}1;36m{l}{RESET}" for l in WORD_ASCII]
    return "\n".join(brand) + "\n\n" + "\n".join(" " * 44 + l for l in logo) + "\n"


def loader_lua() -> str:
    # Lua strings: the art's backslashes escaped first, then the colors.
    brand = "\n".join(
        f'				"\\027[1;36m{l.replace(chr(92), chr(92) * 2)}\\027[0m",' for l in WORD_ASCII
    )
    logo = [loader_color(l.replace("\\", "\\\\")) for l in HOUSE_ASCII]
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


def xterm_rgb(n: int) -> tuple[int, int, int]:
    if n >= 232:
        g = 8 + 10 * (n - 232)
        return (g, g, g)
    levels = [0, 95, 135, 175, 215, 255]
    n -= 16
    return (levels[n // 36], levels[n // 6 % 6], levels[n % 6])


def word_pixels() -> list[list[int | None]]:
    """The compact wordmark as square pixels: each cell is two pixels tall,
    exactly as the terminal draws its half blocks."""
    rows: list[list[int | None]] = []
    for r, line in enumerate(WORD_SMALL):
        color = WORD_GRADIENT[r * 2 + 1]
        top, bottom = [], []
        for ch in line:
            on = {"█": (1, 1), "▀": (1, 0), "▄": (0, 1)}.get(ch, (0, 0))
            top.append(color if on[0] else None)
            bottom.append(color if on[1] else None)
        rows += [top, bottom]
    return rows


def png(size: tuple[int, int], scale: int, word: bool) -> bytes:
    """The terminal art's pixels, scaled up: the boot splash (house and
    wordmark on the desktop's dark gray) or the boot menu's logo."""
    from PIL import Image

    img = Image.new("RGBA", size, (21, 24, 29, 255) if word else (0, 0, 0, 0))
    house = [[PIXEL_COLORS.get(px) for px in row] for row in house_pixels()]
    layers = [(house, scale)]
    if word:
        layers.append((word_pixels(), scale))
    heights = [len(rows) * sc for rows, sc in layers]
    gap = scale * 4
    y = (size[1] - sum(heights) - gap * (len(layers) - 1)) // 2
    for (rows, sc), height in zip(layers, heights):
        x0 = (size[0] - len(rows[0]) * sc) // 2
        for ry, row in enumerate(rows):
            for rx, color in enumerate(row):
                if color is not None:
                    box = (x0 + rx * sc, y + ry * sc, x0 + (rx + 1) * sc, y + (ry + 1) * sc)
                    img.paste(xterm_rgb(color) + (255,), box)
        y += height + gap
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
        files["splash.png"] = png((640, 400), 8, word=True)
        files["logo.png"] = png((150, 138), 6, word=False)
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
