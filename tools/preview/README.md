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

## Glyph outlines

Text in the SVGs rides the viewer's monospace font stack. The three glyphs
many monospace fonts lack (the branch, context, and home markers) are drawn
as vector overlays from `glyphs.json`. Regenerate that file only when the
glyph set changes:

    pip install fonttools
    tools/preview/extract_glyphs.py

`extract_glyphs.py` additionally needs fontconfig (`fc-match`) and the
DejaVu fonts.
