#!/usr/bin/env python3
"""Turn captured statusline ANSI output into terminal-window SVG previews.

Text rides the system monospace font stack; the three glyphs no common font
ships faithfully - the branch symbol, the context square, and the home
marker - are drawn as vector overlays so the previews are font-independent.
Each preview is emitted twice: a dark-framed and a light-framed file for a
<picture> swap. The terminal itself is always dark.
"""
import json
import re
import os

GEN_DIR = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(GEN_DIR, "out")
ASSETS = os.path.normpath(os.path.join(GEN_DIR, "..", "..", "assets"))

# Vector outlines for the statusline glyphs that common monospace fonts lack,
# keyed by the character and baked to JSON so generation needs no font tooling.
# Regenerate with extract_glyphs.py.
with open(os.path.join(GEN_DIR, "glyphs.json")) as _gf:
    OUTLINES = json.load(_gf)
GLYPH_H = 9.6  # target ink height (px) for the outline glyphs

FONT = ("ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, "
        "'Liberation Mono', monospace")
FS = 14.0        # font-size
ADV = 8.4        # monospace advance per char at FS
LH = 23.0        # line height
PAD_L, PAD_R, PAD_T, PAD_B = 22.0, 22.0, 15.0, 15.0
LABEL_H = 20.0   # room above each card for its caption
CARD_GAP = 18.0
MARGIN = 11.0    # canvas margin around the cards (shadow / border room)
TERM_BG = "#1a1b26"

# Overlay exactly the glyphs we have outlines for (context, branch, home - all
# absent from many monospace fonts); everything else rides the font stack.
GLYPHS = set(OUTLINES)

OSC8 = re.compile(r"\x1b\]8;;[^\x1b]*\x1b\\")
SGR = re.compile(r"\x1b\[([0-9;]*)m")
DEFAULT_FG = (0xc0, 0xca, 0xf5)


def parse_ansi(text):
    """One ANSI line -> list of runs: (text, (r,g,b), bold)."""
    text = OSC8.sub("", text)
    runs, color, bold, i = [], None, False, 0
    for m in SGR.finditer(text):
        chunk = text[i:m.start()]
        if chunk:
            runs.append((chunk, color or DEFAULT_FG, bold))
        for tok in _sgr_tokens(m.group(1)):
            if tok == "reset":
                color, bold = None, False
            elif tok == "bold":
                bold = True
            else:
                color = tok
        i = m.end()
    if text[i:]:
        runs.append((text[i:], color or DEFAULT_FG, bold))
    return runs


def _sgr_tokens(params):
    parts = params.split(";") if params else [""]
    out, k = [], 0
    while k < len(parts):
        p = parts[k]
        if p in ("", "0"):
            out.append("reset")
        elif p == "1":
            out.append("bold")
        elif p == "38" and parts[k + 1:k + 2] == ["2"]:
            out.append(tuple(int(x) for x in parts[k + 2:k + 5]))
            k += 4
        k += 1
    return out


def hexc(rgb):
    return "#%02x%02x%02x" % rgb


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def line_cols(runs):
    return sum(len(t) for t, _, _ in runs)


# --- vector glyphs, drawn in a cell whose left edge is x, baseline at y ------

def glyph_path(ch, x, y, color):
    o = OUTLINES.get(ch)
    if not o:
        return ""
    b = o["bounds"]                       # xMin, yMin, xMax, yMax in font units
    gw, gh = b[2] - b[0], b[3] - b[1]
    s = GLYPH_H / gh
    if gw * s > 1.35 * ADV:               # keep a wide proportional glyph near one cell
        s = 1.35 * ADV / gw
    # centre the ink in its one-cell slot, vertically on the text's optical middle
    tx = x + ADV / 2 - (b[0] + b[2]) / 2 * s
    ty = (y - GLYPH_H * 0.52) + (b[1] + b[3]) / 2 * s
    return (f'<g transform="translate({tx:.2f},{ty:.2f}) scale({s:.5f},{-s:.5f})">'
            f'<path d="{o["d"]}" fill="{hexc(color)}"/></g>')


# --- card / svg assembly -----------------------------------------------------

def build_text_and_overlays(lines, x0, base0):
    """lines: list of run-lists. Returns (text_elements, overlay_elements)."""
    texts, overlays = [], []
    for li, runs in enumerate(lines):
        by = base0 + li * LH
        col = 0
        spans = []
        for t, color, bold in runs:
            out_chars = []
            for ch in t:
                if ch in GLYPHS:
                    gx = x0 + col * ADV
                    overlays.append(glyph_path(ch, gx, by, color))
                    out_chars.append(" ")
                else:
                    out_chars.append(ch)
                col += 1
            w = ' font-weight="700"' if bold else ""
            spans.append(f'<tspan fill="{hexc(color)}"{w}>{esc("".join(out_chars))}</tspan>')
        # Pin the line to a fixed monospace grid so vector glyph overlays stay
        # aligned whatever advance the viewer's monospace font happens to have.
        tl = col * ADV
        texts.append(f'<text x="{x0:.1f}" y="{by:.1f}" textLength="{tl:.1f}" '
                     f'lengthAdjust="spacing" xml:space="preserve">'
                     + "".join(spans) + "</text>")
    return texts, overlays


def render_svg(cards, framing):
    """cards: list of (label, lines). framing: 'dark' | 'light'."""
    card_cols = max(line_cols(l) for _, lines in cards for l in lines)
    term_w = PAD_L + card_cols * ADV + PAD_R
    svg_w = term_w + 2 * MARGIN

    body, y = [], MARGIN
    for label, lines in cards:
        if label:
            y += LABEL_H
            body.append(
                f'<text x="{MARGIN+2:.1f}" y="{y-7:.1f}" font-size="11.5" '
                f'fill="#565f89" letter-spacing="0.3">{esc(label)}</text>')
        term_h = PAD_T + len(lines) * LH + PAD_B
        base0 = y + PAD_T + FS * 1.15
        if framing == "light":
            rect = (f'<rect x="{MARGIN:.1f}" y="{y:.1f}" width="{term_w:.1f}" '
                    f'height="{term_h:.1f}" rx="10" fill="{TERM_BG}" '
                    f'filter="url(#shadow)"/>')
        else:
            rect = (f'<rect x="{MARGIN:.1f}" y="{y:.1f}" width="{term_w:.1f}" '
                    f'height="{term_h:.1f}" rx="10" fill="{TERM_BG}" '
                    f'stroke="#313650" stroke-width="1"/>')
        body.append(rect)
        texts, overlays = build_text_and_overlays(lines, MARGIN + PAD_L, base0)
        body.extend(texts)
        body.extend(overlays)
        y += term_h + CARD_GAP
    svg_h = y - CARD_GAP + MARGIN

    defs = ""
    if framing == "light":
        defs = ('<defs><filter id="shadow" x="-20%" y="-20%" width="140%" '
                'height="160%"><feDropShadow dx="0" dy="2" stdDeviation="5" '
                'flood-color="#0b0d16" flood-opacity="0.22"/></filter></defs>')
    head = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{svg_w:.0f}" '
            f'height="{svg_h:.0f}" viewBox="0 0 {svg_w:.1f} {svg_h:.1f}" '
            f'font-family="{FONT}" font-size="{FS:.0f}" '
            f'role="img" aria-label="claude-statusline preview">')
    return head + defs + "".join(body) + "</svg>"


def load_main(name):
    with open(os.path.join(OUT, f"main-{name}.ansi")) as f:
        return [parse_ansi(l) for l in f.read().rstrip("\n").split("\n")]


def load_subagent():
    rows = []
    with open(os.path.join(OUT, "subagent.ndjson")) as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(parse_ansi(json.loads(line)["content"]))
    return rows


def write_pair(basename, cards):
    for framing in ("dark", "light"):
        svg = render_svg(cards, framing)
        path = os.path.join(ASSETS, f"{basename}-{framing}.svg")
        with open(path, "w") as f:
            f.write(svg)
        print(f"wrote {path} ({len(svg)} bytes)")


def main():
    main_cards = [
        ("directory", load_main("cwd")),
        ("repository", load_main("repo")),
        ("worktree", load_main("worktree")),
        ("usage limits (opt-in)", load_main("usage")),
    ]
    write_pair("statusline", main_cards)
    write_pair("statusline-subagent", [("subagent status line", load_subagent())])


if __name__ == "__main__":
    main()
