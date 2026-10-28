#!/usr/bin/env python3
"""Builds the walkthrough video from walkthrough.json and a folder of shots.

Each scene is narrated (piper, the en_US "joe" voice, trained on CC0 data),
shown as its shots (or a title card) with its title and where the picture
came from, and its narration burned in as subtitles for people watching
muted. Scenes fade in and out; the whole is H.264 and AAC in an MP4.

usage: tools/video/make.py [--shots DIR] [--out FILE] [--only ID,ID]

Needs ffmpeg, Pillow, and piper with its voice in build/tools/piper (see
tools/video/README.md). Narration is cached by its text in the work folder,
so changing one scene's words re-records only that scene.
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

REPO = Path(__file__).resolve().parents[2]
W, H = 1920, 1080
FPS = 30
FONTS = Path("/usr/share/fonts/opentype/inter")
BG_TOP, BG_BOTTOM = (11, 18, 32), (17, 24, 39)
ACCENT = (96, 165, 250)
TEXT, MUTED = (243, 244, 246), (156, 163, 175)
LEAD, TAIL = 0.5, 0.9  # silence before and after each scene's words


def font(weight: str, size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(FONTS / f"Inter-{weight}.otf"), size)


def background() -> Image.Image:
    img = Image.new("RGB", (W, H), BG_TOP)
    draw = ImageDraw.Draw(img)
    for y in range(H):
        t = y / (H - 1)
        draw.line([(0, y), (W, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(BG_TOP, BG_BOTTOM)))
    return img


def mark(draw: ImageDraw.ImageDraw) -> None:
    draw.rounded_rectangle([56, 40, 80, 64], radius=6, outline=ACCENT, width=3)
    draw.text((92, 36), "Glasshouse", font=font("SemiBold", 26), fill=TEXT)


def wrap(text: str, f: ImageFont.FreeTypeFont, width: int) -> list[str]:
    lines, line = [], ""
    for word in text.split():
        test = f"{line} {word}".strip()
        if f.getlength(test) <= width:
            line = test
        else:
            lines.append(line)
            line = word
    return lines + [line] if line else lines


def shot_frame(path: Path, title: str, tag: str) -> Image.Image:
    img = background()
    draw = ImageDraw.Draw(img)
    mark(draw)
    shot = Image.open(path).convert("RGB")
    box_w, box_h = 1600, 780
    scale = min(box_w / shot.width, box_h / shot.height)
    shot = shot.resize((round(shot.width * scale), round(shot.height * scale)), Image.LANCZOS)
    x, y = (W - shot.width) // 2, 92 + (box_h - shot.height) // 2
    shadow = Image.new("L", (shot.width + 80, shot.height + 80), 0)
    ImageDraw.Draw(shadow).rounded_rectangle([40, 40, shot.width + 40, shot.height + 40], radius=18, fill=150)
    img.paste((0, 0, 0), (x - 40, y - 28), shadow.filter(ImageFilter.GaussianBlur(22)))
    rounded = Image.new("L", shot.size, 0)
    ImageDraw.Draw(rounded).rounded_rectangle([0, 0, shot.width - 1, shot.height - 1], radius=14, fill=255)
    img.paste(shot, (x, y), rounded)
    title_font = font("SemiBold", 44)
    draw.text((W // 2, 905), title, font=title_font, fill=TEXT, anchor="mm")
    if tag:
        tag_font = font("Medium", 22)
        tw = tag_font.getlength(tag)
        draw.rounded_rectangle([W - 56 - tw - 32, 34, W - 56, 70], radius=18, outline=(55, 65, 81), width=2)
        draw.text((W - 56 - tw - 16, 52), tag, font=tag_font, fill=MUTED, anchor="lm")
    return img


def card_frame(scene: dict) -> Image.Image:
    img = background()
    draw = ImageDraw.Draw(img)
    mark(draw)
    bullets = scene.get("bullets", [])
    title_font = font("SemiBold", 88 if not bullets else 64)
    top = 400 if not bullets else 250
    for i, line in enumerate(wrap(scene["title"], title_font, 1500)):
        draw.text((W // 2, top + i * 92), line, font=title_font, fill=TEXT, anchor="mm")
    if scene.get("subtitle"):
        draw.text((W // 2, top + 110), scene["subtitle"], font=font("Regular", 42), fill=MUTED, anchor="mm")
    bullet_font = font("Regular", 40)
    for i, bullet in enumerate(bullets):
        y = top + 150 + i * 92
        draw.ellipse([300, y - 9, 318, y + 9], fill=ACCENT)
        head, _, rest = bullet.partition(": ")
        if rest:
            bold = font("SemiBold", 40)
            draw.text((350, y), f"{head}: ", font=bold, fill=TEXT, anchor="lm")
            draw.text((350 + bold.getlength(f"{head}: "), y), rest, font=bullet_font, fill=MUTED, anchor="lm")
        else:
            draw.text((350, y), bullet, font=bullet_font, fill=TEXT, anchor="lm")
    return img


def run(*args: str) -> str:
    done = subprocess.run(args, capture_output=True, text=True)
    if done.returncode != 0:
        sys.exit(f"{args[0]} failed:\n{done.stderr[-2000:]}")
    return done.stdout


def duration(path: Path) -> float:
    return float(run("ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)))


def narrate(text: str, work: Path, piper: Path, voice: Path) -> Path:
    out = work / f"say-{hashlib.sha256(text.encode()).hexdigest()[:16]}.wav"
    if not out.exists():
        done = subprocess.run(
            [str(piper), "--model", str(voice), "--output_file", str(out), "--sentence-silence", "0.25"],
            input=text, capture_output=True, text=True,
        )
        if done.returncode != 0 or not out.exists():
            sys.exit(f"piper failed: {done.stderr[-1000:]}")
    return out


def chunks(text: str, most: int = 84) -> list[str]:
    """The narration in subtitle-sized pieces, split at sentences, then commas."""
    out = []
    for sentence in re.split(r"(?<=[.!?:])\s+", text):
        while len(sentence) > most:
            cut = max(sentence.rfind(", ", 0, most), sentence.rfind(" ", 0, most))
            out.append(sentence[: cut + 1].strip())
            sentence = sentence[cut + 1 :].strip()
        if sentence:
            out.append(sentence)
    return out


def stamp(t: float) -> str:
    ms = round(t * 1000)
    return f"{ms // 3600000:02}:{ms // 60000 % 60:02}:{ms // 1000 % 60:02},{ms % 1000:03}"


def clip(frames: list[Path], audio: Path, length: float, out: Path) -> None:
    """One scene: its frames in turn, each slowly closer, with its words."""
    each = length / len(frames)
    parts = []
    inputs: list[str] = []
    for i, frame in enumerate(frames):
        n = max(1, round(each * FPS))
        inputs += ["-loop", "1", "-framerate", str(FPS), "-t", f"{each:.3f}", "-i", str(frame)]
        parts.append(
            f"[{i}:v]scale=2304:1296,zoompan=z='1+0.035*on/{n}':x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)'"
            f":d=1:s={W}x{H}:fps={FPS},trim=duration={each:.3f},setpts=PTS-STARTPTS[v{i}]"
        )
    joined = "".join(f"[v{i}]" for i in range(len(frames)))
    graph = ";".join(parts) + f";{joined}concat=n={len(frames)}:v=1:a=0,fade=t=in:st=0:d=0.4," \
        f"fade=t=out:st={length - 0.4:.3f}:d=0.4,format=yuv420p[v]"
    a = len(frames)
    graph += f";[{a}:a]adelay={int(LEAD * 1000)}:all=1,apad,atrim=duration={length:.3f},aresample=48000[a]"
    run("ffmpeg", "-y", "-v", "error", *inputs, "-i", str(audio), "-filter_complex", graph,
        "-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-preset", "medium", "-crf", "20",
        "-c:a", "aac", "-b:a", "160k", "-ac", "2", "-r", str(FPS), str(out))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--script", type=Path, default=Path(__file__).with_name("walkthrough.json"))
    parser.add_argument("--shots", type=Path, default=REPO / "build/video/shots")
    parser.add_argument("--out", type=Path, default=REPO / "build/video/glasshouse-walkthrough.mp4")
    parser.add_argument("--piper", type=Path, default=REPO / "build/tools/piper/bin/piper")
    parser.add_argument("--voice", type=Path, default=REPO / "build/tools/piper/voices/en_US-joe-medium.onnx")
    parser.add_argument("--only", help="scene ids to build, comma-separated (a preview)")
    args = parser.parse_args()

    script = json.loads(args.script.read_text())
    scenes = script["scenes"]
    if args.only:
        wanted = set(args.only.split(","))
        scenes = [s for s in scenes if s["id"] in wanted]
    work = args.out.parent / "work"
    work.mkdir(parents=True, exist_ok=True)
    missing = [args.shots / p for s in scenes for p in s.get("shots", []) if not (args.shots / p).exists()]
    if missing:
        sys.exit("missing shots:\n  " + "\n  ".join(map(str, missing)))

    clips, cues, at = [], [], 0.0
    for scene in scenes:
        audio = narrate(scene["say"], work, args.piper, args.voice)
        words = duration(audio)
        length = LEAD + words + TAIL
        frames = []
        if scene.get("kind") == "card":
            path = work / f"{scene['id']}.png"
            card_frame(scene).save(path)
            frames.append(path)
        else:
            for i, shot in enumerate(scene["shots"]):
                path = work / f"{scene['id']}-{i}.png"
                shot_frame(args.shots / shot, scene["title"], scene.get("tag", "")).save(path)
                frames.append(path)
        out = work / f"{scene['id']}.mp4"
        clip(frames, audio, length, out)
        clips.append(out)
        # Subtitles, each piece for its share of the words' time.
        pieces = chunks(scene["say"])
        total = sum(len(p) for p in pieces)
        t = at + LEAD
        for piece in pieces:
            span = words * len(piece) / total
            cues.append((t, t + span, piece))
            t += span
        at += length
        print(f"  {scene['id']:<10} {length:5.1f} s")

    listing = work / "clips.txt"
    listing.write_text("".join(f"file '{c}'\n" for c in clips))
    srt = args.out.with_suffix(".srt")
    srt.write_text("".join(f"{i}\n{stamp(a)} --> {stamp(b)}\n{text}\n\n" for i, (a, b, text) in enumerate(cues, 1)))
    joined = work / "joined.mp4"
    run("ffmpeg", "-y", "-v", "error", "-f", "concat", "-safe", "0", "-i", str(listing), "-c", "copy", str(joined))
    style = "FontName=Inter,FontSize=13,PrimaryColour=&H00F3F4F6,OutlineColour=&H80000000,BorderStyle=3,Outline=1,Shadow=0,MarginV=14"
    run("ffmpeg", "-y", "-v", "error", "-i", str(joined),
        "-vf", f"subtitles={srt}:fontsdir={FONTS}:force_style='{style}'",
        "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p",
        "-c:a", "copy", "-movflags", "+faststart", str(args.out))
    print(f"{args.out}: {at:.0f} s; subtitles in {srt}")


if __name__ == "__main__":
    main()
