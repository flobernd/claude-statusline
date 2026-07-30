#!/usr/bin/env python3
"""Render the palette swatches embedded in the top-level README's color table.

One small terminal-window chip per palette entry, showing the hex code in the
colour it names on the same window background the previews and the logo use.
Colours are read from src/theme.rs so the swatches cannot drift from what the
binary paints. Each swatch is emitted twice, dark- and light-framed, for a
<picture> swap; the chip itself is always dark, like the previews.
"""
import os
import re

GEN_DIR = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(GEN_DIR, "..", ".."))
ASSETS = os.path.join(ROOT, "assets")
THEME = os.path.join(ROOT, "src", "theme.rs")

FONT = ("ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, "
        "'Liberation Mono', monospace")
FS = 14.0        # font-size, matching the previews
ADV = 8.4        # monospace advance per char at FS
PAD_X, PAD_Y = 12.0, 6.0
LINE_H = 20.0
MARGIN = 7.0     # canvas margin around the chip (shadow / border room)
TERM_BG = "#1a1b26"

CONST = re.compile(
    r"^pub const (\w+): Rgb = Rgb\(0x([0-9a-f]{2}), 0x([0-9a-f]{2}), "
    r"0x([0-9a-f]{2})\);", re.M)


def palette():
    """(name, hex) for every colour constant, in declaration order."""
    with open(THEME) as f:
        return [(m.group(1).lower(), "#" + m.group(2) + m.group(3) + m.group(4))
                for m in CONST.finditer(f.read())]


def render_svg(hex_code, framing):
    text_w = len(hex_code) * ADV
    chip_w = PAD_X * 2 + text_w
    chip_h = PAD_Y * 2 + LINE_H
    svg_w, svg_h = chip_w + 2 * MARGIN, chip_h + 2 * MARGIN
    baseline = MARGIN + PAD_Y + FS * 1.15

    if framing == "light":
        defs = ('<defs><filter id="shadow" x="-25%" y="-25%" width="150%" '
                'height="170%"><feDropShadow dx="0" dy="1.5" stdDeviation="3" '
                'flood-color="#0b0d16" flood-opacity="0.22"/></filter></defs>')
        chip = (f'<rect x="{MARGIN:.1f}" y="{MARGIN:.1f}" width="{chip_w:.1f}" '
                f'height="{chip_h:.1f}" rx="7" fill="{TERM_BG}" '
                f'filter="url(#shadow)"/>')
    else:
        defs = ""
        chip = (f'<rect x="{MARGIN:.1f}" y="{MARGIN:.1f}" width="{chip_w:.1f}" '
                f'height="{chip_h:.1f}" rx="7" fill="{TERM_BG}" '
                f'stroke="#313650" stroke-width="1"/>')
    # Pin the text to the monospace grid, as the previews do, so the chip keeps
    # its width whatever advance the viewer's monospace font happens to have.
    label = (f'<text x="{MARGIN + PAD_X:.1f}" y="{baseline:.1f}" '
             f'textLength="{text_w:.1f}" lengthAdjust="spacing" '
             f'xml:space="preserve" fill="{hex_code}">{hex_code}</text>')
    head = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{svg_w:.0f}" '
            f'height="{svg_h:.0f}" viewBox="0 0 {svg_w:.1f} {svg_h:.1f}" '
            f'font-family="{FONT}" font-size="{FS:.0f}" '
            f'role="img" aria-label="{hex_code}">')
    return head + defs + chip + label + "</svg>"


def main():
    for name, hex_code in palette():
        for framing in ("dark", "light"):
            path = os.path.join(ASSETS, f"swatch-{name}-{framing}.svg")
            with open(path, "w") as f:
                f.write(render_svg(hex_code, framing))
            print(f"wrote {path}")


if __name__ == "__main__":
    main()
