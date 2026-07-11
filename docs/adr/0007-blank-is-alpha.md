# "Empty" is defined by alpha, not a null state

There is no `Option<Cell>` in the grid. The canonical Blank cell is a space glyph with fully transparent background; `Cell::is_blank()` is the single test. Eraser writes Blank (per its plane toggles), layer compositing uses the alpha already stored in `Rgba`, and export trimming checks blankness. Rejected: `Option<Cell>` (creates two blank-ish states — "never touched" vs "drew a space" — that every tool and exporter would have to reconcile) and always-opaque cells (would block layer compositing later).
