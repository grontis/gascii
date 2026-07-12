//! Plain-text export: composited glyphs only, trailing whitespace trimmed per row.

use super::composite;
use crate::model::Document;

/// Composites `doc` and flattens it to a newline-joined string, trimming each row's trailing
/// whitespace by composited glyph (a colored-but-space cell at a line's end still trims — plain
/// text has nowhere to put the color anyway).
pub fn export_text(doc: &Document) -> String {
    composite(doc)
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>().trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Rgba};

    #[test]
    fn hand_built_doc_exports_expected_multiline_string() {
        let mut doc = Document::new(3, 2);
        doc.set_cell(0, 0, 0, Cell { ch: 'a', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 1, 0, Cell { ch: 'b', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 0, 1, Cell { ch: 'c', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        assert_eq!(export_text(&doc), "ab\nc");
    }

    #[test]
    fn trailing_colored_but_blank_cells_trim_to_last_glyph() {
        let mut doc = Document::new(4, 1);
        doc.set_cell(0, 0, 0, Cell { ch: 'x', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        // Colored bg but still a space glyph — trims away regardless of color.
        doc.set_cell(0, 1, 0, Cell { ch: ' ', fg: Rgba::WHITE, bg: Rgba(9, 9, 9, 255) });
        doc.set_cell(0, 2, 0, Cell { ch: ' ', fg: Rgba::WHITE, bg: Rgba(9, 9, 9, 255) });
        assert_eq!(export_text(&doc), "x");
    }

    #[test]
    fn all_blank_document_exports_to_empty_lines() {
        let doc = Document::new(3, 3);
        assert_eq!(export_text(&doc), "\n\n");
    }

    #[test]
    fn one_by_one_document() {
        let mut doc = Document::new(1, 1);
        doc.set_cell(0, 0, 0, Cell { ch: 'Q', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        assert_eq!(export_text(&doc), "Q");

        let blank = Document::new(1, 1);
        assert_eq!(export_text(&blank), "");
    }
}
