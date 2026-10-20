#!/usr/bin/env python3
"""Converts a Claude Docs read of the PRD into Markdown, for reading and diffing.

The PRD is a Claude Docs document. Claude reads it with the Claude Docs
connector (`read` of the doc's content node); the result is JSON holding the
document as XML. Save that result to a file, then:

    tools/prd/prd2md.py <read-result.json> docs/prd/prd-snapshot.md
    git diff docs/prd/prd-snapshot.md          # what changed since last sync

Links keep their targets; embedded diagrams appear as `[embed …]` lines.
See .claude/skills/prd-sync.
"""

import json
import sys
import xml.etree.ElementTree as ET


def inline(el) -> str:
    if el.tag == "text":
        link = el.find("link")
        if link is not None:
            return f"[{''.join(link.itertext())}]({link.get('href', '')})"
        text = "".join(el.itertext())
        return f"**{text}**" if el.find("bold") is not None else text
    if el.tag == "mention":
        return "@" + el.get("name", "")
    if el.tag == "date":
        return el.get("value", "")
    if el.tag == "link":
        return f"[{''.join(el.itertext())}]({el.get('href', '')})"
    return "".join(el.itertext())


def para(el) -> str:
    return "".join(inline(child) for child in el)


def walk(el, out: list[str], depth: int = 0) -> None:
    for child in el:
        if child.tag == "paragraph":
            heading = child.get("heading")
            text = para(child)
            out.append(f"\n{'#' * int(heading)} {text}" if heading else "  " * depth + text)
        elif child.tag == "list":
            for index, item in enumerate(child):
                mark = f"{index + 1}. " if child.get("kind") == "ordered" else "- "
                first = True
                for sub in item:
                    if sub.tag == "paragraph":
                        out.append("  " * depth + (mark if first else "  ") + para(sub))
                        first = False
                    else:
                        wrapper = ET.Element("w")
                        wrapper.append(sub)
                        walk(wrapper, out, depth + 1)
        elif child.tag == "table":
            for row in child:
                cells = [" ".join(para(p) if p.tag == "paragraph" else "".join(p.itertext()) for p in cell) for cell in row]
                out.append("| " + " | ".join(cells) + " |")
            out.append("")
        elif child.tag == "embed":
            out.append(f"[embed: {child.get('caption', '')} ({child.get('ref', '')})]")
        else:
            out.append(f"[{child.tag}] " + "".join(child.itertext()))


def main() -> int:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    data = json.load(open(sys.argv[1]))
    # A saved read is either the result itself or a list of text parts, one
    # of them the result (when the connector adds a notice before it).
    if isinstance(data, list):
        data = next(
            json.loads(part["text"])
            for part in data
            if part.get("type") == "text" and part["text"].lstrip().startswith("{")
        )
    xml = data.get("data", data).get("xml") or data["value"]["xml"]
    out: list[str] = []
    walk(ET.fromstring(xml), out)
    with open(sys.argv[2], "w") as f:
        f.write("\n".join(out).lstrip() + "\n")
    print(f"{sys.argv[2]}: {len(out)} lines")
    return 0


if __name__ == "__main__":
    sys.exit(main())
