//! `Document`/`Layer` behavior at scale, exercised only through the crate's public API —
//! deliberately outside `model.rs` so nothing here can pass by relying on private internals.

use gascii_core::{Cell, DocExtent, Document, Rgba};

fn glyph(ch: char) -> Cell {
    Cell {
        ch,
        fg: Rgba::WHITE,
        bg: Rgba::TRANSPARENT,
    }
}

#[test]
fn large_document_all_four_corners_round_trip_independently() {
    let mut doc = Document::new(1024, 1024);
    let corners = [
        (0u16, 0u16, 'A'),
        (1023, 0, 'B'),
        (0, 1023, 'C'),
        (1023, 1023, 'D'),
    ];
    for &(x, y, ch) in &corners {
        assert!(doc.set_cell(0, x, y, glyph(ch)), "set_cell should succeed at ({x},{y})");
    }
    for &(x, y, ch) in &corners {
        assert_eq!(doc.cell(0, x, y), Some(&glyph(ch)), "readback mismatch at ({x},{y})");
    }

    // Cells adjacent to each corner (but not corners themselves) must remain untouched.
    assert_eq!(doc.cell(0, 1, 0), Some(&Cell::BLANK));
    assert_eq!(doc.cell(0, 1022, 0), Some(&Cell::BLANK));
    assert_eq!(doc.cell(0, 1, 1023), Some(&Cell::BLANK));
    assert_eq!(doc.cell(0, 1022, 1023), Some(&Cell::BLANK));
    assert_eq!(doc.cell(0, 0, 1), Some(&Cell::BLANK));
    assert_eq!(doc.cell(0, 0, 1022), Some(&Cell::BLANK));
    assert_eq!(doc.cell(0, 1023, 1), Some(&Cell::BLANK));
    assert_eq!(doc.cell(0, 1023, 1022), Some(&Cell::BLANK));
}

#[test]
fn row_major_index_does_not_bleed_across_row_boundary() {
    let mut doc = Document::new(1024, 3);
    assert!(doc.set_cell(0, 1023, 0, glyph('X'))); // last cell of row 0
    assert!(doc.set_cell(0, 0, 2, glyph('Y'))); // first cell of row 2

    assert_eq!(doc.cell(0, 1023, 0), Some(&glyph('X')));
    assert_eq!(doc.cell(0, 0, 1), Some(&Cell::BLANK), "row 0's last cell must not bleed into row 1");
    assert_eq!(doc.cell(0, 0, 2), Some(&glyph('Y')));
    assert_eq!(doc.cell(0, 1023, 1), Some(&Cell::BLANK), "row 2's first cell must not bleed into row 1");
}

#[test]
fn full_grid_fill_and_readback_at_nontrivial_scale() {
    // Per-cell-unique values so any two coordinates aliasing to the same index would mismatch.
    let (w, h) = (64u16, 64u16);
    let mut doc = Document::new(w, h);
    let expected = |x: u16, y: u16| -> char {
        char::from_u32(('a' as u32) + ((x as u32 + y as u32 * 3) % 26)).unwrap()
    };
    for y in 0..h {
        for x in 0..w {
            assert!(doc.set_cell(0, x, y, glyph(expected(x, y))));
        }
    }
    for y in 0..h {
        for x in 0..w {
            assert_eq!(
                doc.cell(0, x, y).map(|c| c.ch),
                Some(expected(x, y)),
                "mismatch at ({x},{y})"
            );
        }
    }
}

#[test]
fn serde_round_trip_at_1024x1024_scale() {
    let mut doc = Document::new(1024, 1024);
    assert!(doc.set_cell(0, 0, 0, glyph('A')));
    assert!(doc.set_cell(0, 1023, 1023, glyph('Z')));

    let json = serde_json::to_string(&doc).expect("serialize");
    let back: Document = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(doc, back);
    assert_eq!(back.cell(0, 0, 0), Some(&glyph('A')));
    assert_eq!(back.cell(0, 1023, 1023), Some(&glyph('Z')));
    assert_eq!(back.extent(), DocExtent { width: 1024, height: 1024 });
}

#[test]
fn doc_extent_matches_construction_dimensions_for_non_square_docs() {
    let doc = Document::new(37, 91);
    assert_eq!(doc.extent(), DocExtent { width: 37, height: 91 });
    assert!(doc.in_bounds(36, 90));
    assert!(!doc.in_bounds(37, 90));
    assert!(!doc.in_bounds(36, 91));
}

#[test]
fn out_of_bounds_layer_index_degrades_gracefully_not_panics() {
    let mut doc = Document::new(10, 10);
    assert_eq!(doc.cell(1, 0, 0), None);
    assert_eq!(doc.cell(99, 5, 5), None);
    assert!(!doc.set_cell(1, 0, 0, glyph('x')));
    assert!(!doc.set_cell(99, 5, 5, glyph('x')));
    assert!(doc.cell(0, 0, 0).unwrap().is_blank());
    assert!(doc.layers[0].cells().iter().all(Cell::is_blank));
}

#[test]
fn default_document_and_new_1x1_document_are_both_well_formed() {
    let default_doc = Document::default_document();
    assert_eq!(default_doc.extent(), DocExtent { width: 80, height: 25 });
    assert_eq!(default_doc.layers.len(), 1);

    let mut tiny = Document::new(1, 1);
    assert_eq!(tiny.extent(), DocExtent { width: 1, height: 1 });
    assert!(tiny.in_bounds(0, 0));
    assert!(!tiny.in_bounds(1, 0));
    assert!(!tiny.in_bounds(0, 1));
    assert!(tiny.set_cell(0, 0, 0, glyph('!')));
    assert_eq!(tiny.cell(0, 0, 0), Some(&glyph('!')));
}
