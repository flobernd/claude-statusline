# Preview generation

Regenerates the SVG previews embedded in the top-level README
(`assets/statusline-*.svg`). The pipeline captures real output of the release
binary and renders it into terminal-window SVGs, so the previews never drift
from what the statusline actually prints.

`swatches.py` regenerates the palette chips in the README's color table
(`assets/swatch-*.svg`), reading the colours straight from `src/theme.rs`. It
needs no capture step and no build:

    tools/preview/swatches.py

## Pipeline

1. `setup.sh` builds scratch git repositories in `work/` (branch, dirty tree,
   stashes, ahead/behind counts, a linked worktree), writes statusline
   payloads, and captures the binary's ANSI output into `out/`.
2. `gen.py` parses the captured ANSI and writes the dark- and light-framed
   SVGs into `../../assets/`.

## Usage

    cargo build --release
    tools/preview/setup.sh
    tools/preview/gen.py

Requires `bash`, `git`, GNU coreutils (`setup.sh` uses GNU `date` flags),
and `python3`; no Python packages.

## Determinism

Every timestamp in the captures derives from one canonical instant,
2026-07-24T00:00:00Z, and `setup.sh` exports the same instant to the binary
as `CLAUDE_STATUSLINE_NOW_MS` (epoch milliseconds; unset or unparsable
falls back to the real clock). A fixed date rather than "now" keeps
calendar-derived output stable, because the spend reset renders the
distance to the next 1st of the month. Regeneration is therefore
byte-identical on any machine, any day, and CI enforces it: the `assets`
job regenerates all of `assets/` and fails when the committed files drift.

## Glyph outlines

Text in the SVGs rides the viewer's monospace font stack. The three glyphs
many monospace fonts lack (the branch, context, and home markers) are drawn
as vector overlays from `glyphs.json`. Regenerate that file only when the
glyph set changes:

    pip install fonttools
    tools/preview/extract_glyphs.py

`extract_glyphs.py` additionally needs fontconfig (`fc-match`) and the
DejaVu fonts.
