//! Plain-text export: composited glyphs only, trailing whitespace trimmed per row.

use super::{composite, composite_frame};
use crate::model::{Cell, Document};

/// Flattens a composited sheet to a newline-joined string, trimming each row's trailing whitespace
/// by composited glyph (a colored-but-space cell at a line's end still trims — plain text has
/// nowhere to put the color anyway). Shared by `export_text` and `export_text_frames`.
fn flatten_trimmed(cells: &[Vec<Cell>]) -> String {
    cells
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>().trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Composites `doc` and flattens it to a newline-joined string, trailing whitespace trimmed per row.
pub fn export_text(doc: &Document) -> String {
    flatten_trimmed(&composite(doc))
}

/// Multi-frame generalization of `export_text`: every frame's own composited, trimmed text, in
/// document order, each preceded by a `--- frame N (Dms) ---` header (1-based index matching the
/// timeline UI's own "N/total" display; `D` is the frame's `resolved_frame_duration_ms`), frames
/// separated by a blank line.
pub fn export_text_frames(doc: &Document) -> String {
    (0..doc.frame_count())
        .map(|i| {
            let body = flatten_trimmed(&composite_frame(doc, i).expect("i is always in 0..frame_count()"));
            let dur = doc.resolved_frame_duration_ms(i).expect("i is always in 0..frame_count()");
            format!("--- frame {} ({dur}ms) ---\n{body}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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

    // `export_text_frames` tests.

    #[test]
    fn a_single_frame_document_produces_one_headered_frame_matching_export_text() {
        let mut doc = Document::new(2, 1);
        doc.set_cell(0, 0, 0, Cell { ch: 'a', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        assert_eq!(
            export_text_frames(&doc),
            format!("--- frame 1 ({}ms) ---\n{}", Document::DEFAULT_FRAME_DURATION_MS, export_text(&doc))
        );
    }

    #[test]
    fn a_multi_frame_document_with_a_duration_override_produces_the_expected_headered_string() {
        use crate::edit::History;
        use crate::frame_ops::{add_frame, set_frame_duration};
        use crate::model::Frame;

        let mut doc = Document::new(2, 1);
        doc.set_cell(0, 0, 0, Cell { ch: 'a', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        // Trailing colored-but-blank cell — pins that per-frame trim still applies inside each body.
        doc.set_cell(0, 1, 0, Cell { ch: ' ', fg: Rgba::WHITE, bg: Rgba(1, 1, 1, 255) });

        let mut history = History::new();
        let edit = add_frame(&doc, 1, Frame::blank(2, 1)).unwrap();
        history.apply(&mut doc, edit);
        assert!(doc.set_active_frame(1));
        doc.set_cell(0, 0, 0, Cell { ch: 'b', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        assert!(doc.set_active_frame(0));

        let edit = set_frame_duration(&doc, 1, Some(250)).unwrap().unwrap();
        history.apply(&mut doc, edit);

        let expected = format!(
            "--- frame 1 ({}ms) ---\na\n\n--- frame 2 (250ms) ---\nb",
            Document::DEFAULT_FRAME_DURATION_MS
        );
        assert_eq!(export_text_frames(&doc), expected);
    }

    /// Integration seam between `export_text_frames` and `export_text`: every frame's own body
    /// inside the combined multi-frame dump must equal `export_text` called on a document whose
    /// active frame is that same index in isolation -- not just a hand-computed expected string
    /// (the test above), but the *actual other function* this one is documented to generalize.
    /// Covers the trim rule too, since frame 1's trailing cell is colored-but-blank.
    #[test]
    fn every_frame_segment_matches_export_text_of_that_frame_taken_in_isolation() {
        use crate::edit::History;
        use crate::frame_ops::add_frame;
        use crate::model::Frame;

        let mut doc = Document::new(3, 1);
        doc.set_cell(0, 0, 0, Cell { ch: 'x', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });

        let mut history = History::new();
        for _ in 1..3 {
            let edit = add_frame(&doc, doc.frame_count(), Frame::blank(3, 1)).unwrap();
            history.apply(&mut doc, edit);
        }
        doc.set_active_frame(1);
        doc.set_cell(0, 0, 0, Cell { ch: 'y', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 1, 0, Cell { ch: ' ', fg: Rgba::WHITE, bg: Rgba(3, 3, 3, 255) }); // trims away
        doc.set_active_frame(2);
        doc.set_cell(0, 2, 0, Cell { ch: 'z', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_active_frame(0);

        let combined = export_text_frames(&doc);
        for i in 0..doc.frame_count() {
            let header = format!("--- frame {} ({}ms) ---", i + 1, Document::DEFAULT_FRAME_DURATION_MS);
            let mut isolated = doc.clone();
            isolated.set_active_frame(i);
            let expected_body = export_text(&isolated);
            let segment = combined
                .split("\n\n")
                .nth(i)
                .unwrap_or_else(|| panic!("segment {i} must exist in the combined dump"));
            assert_eq!(segment, format!("{header}\n{expected_body}"), "frame {i}'s segment must match export_text in isolation");
        }
    }
}
