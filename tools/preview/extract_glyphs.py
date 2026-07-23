#!/usr/bin/env python3
"""Extract vector outlines for the statusline glyphs that common monospace fonts
lack, into glyphs.json for gen.py to overlay. Run only when those glyphs change.

Requires fontTools (`pip install fonttools`) and fontconfig (`fc-match`). gen.py
itself needs neither - it just reads the baked glyphs.json.
"""
import json
import os
import subprocess

from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.boundsPen import BoundsPen

HERE = os.path.dirname(os.path.abspath(__file__))

# Statusline glyph -> a font family that draws it well. The context (U+2338) and
# home (U+2302) markers are in the mono face and stay a single cell; the branch
# marker (U+2387) is absent from the mono face, so its outline comes from the
# proportional face and gen.py caps its width.
SPEC = {
    "⌁": "DejaVu Sans Mono",  # usage limits, electric arrow
    "⌸": "DejaVu Sans Mono",  # context, APL quad-equal (window with lines)
    "⌂": "DejaVu Sans Mono",  # home
    "⎇": "DejaVu Sans",       # branch, alternative-key symbol
}


def font_file(family):
    return subprocess.check_output(
        ["fc-match", "-f", "%{file}", family]
    ).decode().strip()


def main():
    out = {}
    for ch, family in SPEC.items():
        font = TTFont(font_file(family))
        glyph_set = font.getGlyphSet()
        name = font.getBestCmap()[ord(ch)]
        path = SVGPathPen(glyph_set)
        glyph_set[name].draw(path)
        bounds = BoundsPen(glyph_set)
        glyph_set[name].draw(bounds)
        out[ch] = {
            "d": path.getCommands(),
            "bounds": [round(v, 1) for v in bounds.bounds],
            "upm": font["head"].unitsPerEm,
        }
    with open(os.path.join(HERE, "glyphs.json"), "w") as f:
        json.dump(out, f, ensure_ascii=False)
    print("wrote glyphs.json:", ", ".join(f"U+{ord(c):04X}" for c in out))


if __name__ == "__main__":
    main()
