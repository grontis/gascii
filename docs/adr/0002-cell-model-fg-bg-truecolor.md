# Per-cell fg+bg truecolor in the data model from day one

Each Cell stores `{ch, fg: Rgba, bg: Rgba}` (the REXPaint/ANSI-art model), even though the first build renders white-on-black. Retrofitting color into the model later is the painful part; carrying unused fields is cheap. Truecolor is stored internally; constrained palettes (ANSI 16, xterm-256, custom) exist only at the picker and at export-time quantization. Tools write per-Plane: any subset of (glyph, fg, bg) per stroke, which multiplies every tool (bg-only pencil = highlighter, fg-only = recolor brush).
