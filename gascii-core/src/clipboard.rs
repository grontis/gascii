//! In-memory colored cell patches: capture a document region for the internal clipboard/move
//! float, flatten to plain text for the system clipboard, and reconstruct a patch from pasted
//! external text. Never touches the OS clipboard itself — that's the app crate's job.

use crate::model::{Cell, DocSettings, Document, Rgba};
use crate::palette::allowed_in;
use crate::tools::CellRect;

/// A rectangular block of cells at no fixed document position, row-major, `width*height` long.
/// Backs both the floating-selection stamp (a lifted region) and paste (external or internal).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CellPatch {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<Cell>,
}

impl CellPatch {
    /// Captures `rect`'s cells from `doc`, unmasked (every plane, exactly as stored).
    pub fn from_region(doc: &Document, rect: CellRect, layer: usize) -> CellPatch {
        let width = rect.width();
        let height = rect.height();
        let mut cells = Vec::with_capacity(width as usize * height as usize);
        for y in rect.y0..=rect.y1 {
            for x in rect.x0..=rect.x1 {
                cells.push(doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK));
            }
        }
        CellPatch { width, height, cells }
    }

    /// Flattens to plain text: one glyph per cell, rows newline-joined, each row's trailing
    /// whitespace trimmed — the same convention `export_text` uses, so a copy round-trips through
    /// the system clipboard identically to a whole-document export.
    pub fn to_text(&self) -> String {
        (0..self.height as usize)
            .map(|row| {
                let start = row * self.width as usize;
                let end = start + self.width as usize;
                self.cells[start..end].iter().map(|c| c.ch).collect::<String>().trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Builds a patch from pasted plain text: rows split on `\n`, each character routed through
    /// `allowed_in` (a rejected character is dropped, counted, and leaves that cell Blank), short
    /// rows padded with Blank out to the widest row. Returns the patch plus the number of rejected
    /// characters, so the caller can surface a warning.
    ///
    /// Pasted text is untrusted external input, exactly like a loaded `.gascii` file: its line
    /// count and per-line character count arrive as unbounded `usize` values with no relation to
    /// `u16`. Both are clamped against `Document::MAX_HEIGHT`/`MAX_WIDTH` *before* `cells` is
    /// allocated or indexed — never truncated via a bare `as u16` cast, which would wrap silently
    /// and then panic (or under-allocate) once the loop below indexes past the wrapped bound.
    /// Anything clamped away is folded into the same `dropped` count already used for
    /// rejected characters, so the caller's existing "N character(s) rejected" warning covers it
    /// too, rather than discarding it silently.
    pub fn from_external_text(text: &str, fg: Rgba, bg: Rgba, settings: &DocSettings) -> (CellPatch, usize) {
        let all_lines: Vec<&str> = text.split('\n').collect();
        let max_height = Document::MAX_HEIGHT as usize;
        let max_width = Document::MAX_WIDTH as usize;
        let mut dropped = 0usize;

        // Clamp height before anything else is computed from `all_lines`: extra lines are dropped
        // wholesale (every one of their characters counts toward `dropped`) rather than read at
        // all.
        let lines = if all_lines.len() > max_height {
            dropped += all_lines[max_height..].iter().map(|l| l.chars().count()).sum::<usize>();
            &all_lines[..max_height]
        } else {
            &all_lines[..]
        };
        let height = lines.len() as u16; // safe: capped at MAX_HEIGHT, well within u16

        // Clamp width the same way, before allocating `cells`: each line's character count is
        // capped at MAX_WIDTH before it can influence the buffer's size.
        let width = lines.iter().map(|l| l.chars().count().min(max_width)).max().unwrap_or(0) as u16;

        let mut cells = vec![Cell::BLANK; width as usize * height as usize];
        for (row, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                if col >= width as usize {
                    dropped += 1; // this row is wider than the clamped width: excess chars dropped
                    continue;
                }
                if allowed_in(ch, settings).is_err() {
                    dropped += 1;
                    continue;
                }
                cells[row * width as usize + col] = Cell { ch, fg, bg };
            }
        }
        (CellPatch { width, height, cells }, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DocSettings;

    fn settings(strict_ascii: bool) -> DocSettings {
        DocSettings { strict_ascii }
    }

    fn cell(ch: char, fg: Rgba, bg: Rgba) -> Cell {
        Cell { ch, fg, bg }
    }

    #[test]
    fn from_region_captures_exactly_the_regions_cells() {
        let mut doc = Document::new(10, 10);
        doc.set_cell(0, 2, 3, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));
        doc.set_cell(0, 3, 3, cell('b', Rgba::WHITE, Rgba::TRANSPARENT));
        doc.set_cell(0, 2, 4, cell('c', Rgba::WHITE, Rgba::TRANSPARENT));
        doc.set_cell(0, 3, 4, cell('d', Rgba::WHITE, Rgba::TRANSPARENT));

        let rect = CellRect { x0: 2, y0: 3, x1: 3, y1: 4 };
        let patch = CellPatch::from_region(&doc, rect, 0);
        assert_eq!(patch.width, 2);
        assert_eq!(patch.height, 2);
        assert_eq!(patch.cells.iter().map(|c| c.ch).collect::<Vec<_>>(), vec!['a', 'b', 'c', 'd']);
    }

    #[test]
    fn round_trips_region_to_patch_to_cells_for_a_1x1_region() {
        let mut doc = Document::new(5, 5);
        doc.set_cell(0, 1, 1, cell('Q', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255)));
        let rect = CellRect { x0: 1, y0: 1, x1: 1, y1: 1 };
        let patch = CellPatch::from_region(&doc, rect, 0);
        assert_eq!(patch.width, 1);
        assert_eq!(patch.height, 1);
        assert_eq!(patch.cells, vec![cell('Q', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255))]);
    }

    #[test]
    fn to_text_trims_trailing_whitespace_per_row() {
        let patch = CellPatch {
            width: 3,
            height: 2,
            cells: vec![
                cell('x', Rgba::WHITE, Rgba::TRANSPARENT),
                Cell::BLANK,
                Cell::BLANK,
                cell('a', Rgba::WHITE, Rgba::TRANSPARENT),
                cell('b', Rgba::WHITE, Rgba::TRANSPARENT),
                Cell::BLANK,
            ],
        };
        assert_eq!(patch.to_text(), "x\nab");
    }

    #[test]
    fn to_text_of_an_all_blank_patch_is_empty_lines() {
        let patch = CellPatch { width: 3, height: 2, cells: vec![Cell::BLANK; 6] };
        assert_eq!(patch.to_text(), "\n");
    }

    #[test]
    fn from_external_text_splits_lines_and_pads_short_rows_with_blank() {
        let (patch, dropped) = CellPatch::from_external_text("ab\nc", Rgba::WHITE, Rgba::TRANSPARENT, &settings(false));
        assert_eq!(dropped, 0);
        assert_eq!(patch.width, 2);
        assert_eq!(patch.height, 2);
        assert_eq!(patch.cells[0].ch, 'a');
        assert_eq!(patch.cells[1].ch, 'b');
        assert_eq!(patch.cells[2].ch, 'c');
        assert_eq!(patch.cells[3], Cell::BLANK, "short row padded with Blank");
    }

    #[test]
    fn from_external_text_uses_the_given_fg_and_bg_for_accepted_chars() {
        let fg = Rgba(1, 2, 3, 255);
        let bg = Rgba(4, 5, 6, 255);
        let (patch, _) = CellPatch::from_external_text("x", fg, bg, &settings(false));
        assert_eq!(patch.cells[0], Cell { ch: 'x', fg, bg });
    }

    #[test]
    fn from_external_text_drops_wide_and_combining_characters_and_counts_them() {
        let (patch, dropped) =
            CellPatch::from_external_text("a😀b\u{0301}c", Rgba::WHITE, Rgba::TRANSPARENT, &settings(false));
        assert_eq!(dropped, 2, "the emoji and the combining mark must both be rejected");
        let chars: Vec<char> = patch.cells.iter().map(|c| c.ch).collect();
        assert_eq!(chars, vec!['a', ' ', 'b', ' ', 'c'], "rejected chars leave their cell Blank");
    }

    #[test]
    fn from_external_text_rejects_non_ascii_under_strict_ascii() {
        let (patch, dropped) = CellPatch::from_external_text("│", Rgba::WHITE, Rgba::TRANSPARENT, &settings(true));
        assert_eq!(dropped, 1);
        assert_eq!(patch.cells[0], Cell::BLANK);
    }

    #[test]
    fn from_external_text_accepts_non_ascii_when_not_strict() {
        let (patch, dropped) = CellPatch::from_external_text("│", Rgba::WHITE, Rgba::TRANSPARENT, &settings(false));
        assert_eq!(dropped, 0);
        assert_eq!(patch.cells[0].ch, '│');
    }

    #[test]
    fn from_external_text_clamps_a_line_over_65535_chars_without_panicking() {
        // Regression for the u16-truncation panic: a single line long enough to wrap a naive
        // `as u16` cast to a small remainder while `chars().count()` in the write loop still runs
        // over the real, untruncated length.
        let long_line = "a".repeat(70_000);
        let (patch, dropped) = CellPatch::from_external_text(&long_line, Rgba::WHITE, Rgba::TRANSPARENT, &settings(false));
        assert_eq!(patch.width, Document::MAX_WIDTH, "width must clamp to the document's max, not wrap");
        assert_eq!(patch.height, 1);
        assert_eq!(patch.cells.len(), Document::MAX_WIDTH as usize, "buffer must be sized to the clamped width");
        assert_eq!(dropped, 70_000 - Document::MAX_WIDTH as usize, "every char beyond the clamp is counted as dropped");
    }

    #[test]
    fn from_external_text_clamps_more_than_max_height_lines_without_panicking() {
        // Regression for the same truncation class on line count: pasting far more than
        // Document::MAX_HEIGHT lines must clamp before allocating, not panic or under-allocate.
        let many_lines = "a\n".repeat(2000);
        let text = many_lines.trim_end_matches('\n');
        let (patch, dropped) = CellPatch::from_external_text(text, Rgba::WHITE, Rgba::TRANSPARENT, &settings(false));
        assert_eq!(patch.height, Document::MAX_HEIGHT, "height must clamp to the document's max, not wrap");
        assert_eq!(patch.width, 1);
        assert_eq!(dropped, 2000 - Document::MAX_HEIGHT as usize, "every dropped line's char(s) are counted");
    }

    #[test]
    fn from_external_text_clamps_both_dimensions_at_once_with_no_oversized_allocation() {
        // A "paste bomb": large in both directions at once. The clamped buffer must never exceed
        // MAX_WIDTH * MAX_HEIGHT cells, regardless of how large the pasted text claims to be.
        let bomb: String =
            std::iter::repeat_with(|| "a".repeat(5000)).take(3000).collect::<Vec<_>>().join("\n");
        let (patch, _dropped) = CellPatch::from_external_text(&bomb, Rgba::WHITE, Rgba::TRANSPARENT, &settings(false));
        assert_eq!(patch.width, Document::MAX_WIDTH);
        assert_eq!(patch.height, Document::MAX_HEIGHT);
        assert_eq!(patch.cells.len(), Document::MAX_WIDTH as usize * Document::MAX_HEIGHT as usize);
    }

    #[test]
    fn from_external_text_of_an_empty_string_yields_a_zero_width_single_line_patch() {
        let (patch, dropped) = CellPatch::from_external_text("", Rgba::WHITE, Rgba::TRANSPARENT, &settings(false));
        assert_eq!(dropped, 0);
        assert_eq!(patch.width, 0);
        assert_eq!(patch.height, 1);
        assert!(patch.cells.is_empty());
    }

    #[test]
    fn round_trip_region_to_text_matches_composited_glyphs() {
        let mut doc = Document::new(4, 2);
        doc.set_cell(0, 0, 0, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));
        doc.set_cell(0, 1, 0, cell('b', Rgba::WHITE, Rgba::TRANSPARENT));
        doc.set_cell(0, 0, 1, cell('c', Rgba::WHITE, Rgba::TRANSPARENT));
        let rect = CellRect { x0: 0, y0: 0, x1: 3, y1: 1 };
        let patch = CellPatch::from_region(&doc, rect, 0);
        assert_eq!(patch.to_text(), "ab\nc");
    }
}
