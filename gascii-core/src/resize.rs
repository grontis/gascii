//! Document resize: top-left anchored grow (pad with Blank) or shrink (crop). Produces an
//! `Edit::Resize` through the normal `History` choke point — resize is an undoable document
//! mutation like any other, so it goes through the same `Edit`/`History` path every tool's stroke
//! already uses rather than mutating `Document` directly.

use crate::edit::{DocSnapshot, Edit};
use crate::model::{Cell, Document, DocExtent, Layer};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResizeError {
    ZeroExtent,
    TooLarge { width: u16, height: u16, max_width: u16, max_height: u16 },
}

/// Builds the `Edit::Resize` for growing/shrinking `doc` to `new_width x new_height`, anchored
/// top-left (grow pads Blank at bottom/right; shrink crops from bottom/right). Validates the
/// requested extent against `Document::MAX_WIDTH`/`MAX_HEIGHT` *before* allocating anything sized
/// by it — the same untrusted-size discipline the `.gascii` loader and paste already apply, even
/// though this particular size originates from the app's own resize dialog (belt-and-suspenders).
/// Returns `Ok(None)` for a same-size no-op (no empty undo entry).
pub fn resize_document(doc: &Document, new_width: u16, new_height: u16) -> Result<Option<Edit>, ResizeError> {
    if new_width == 0 || new_height == 0 {
        return Err(ResizeError::ZeroExtent);
    }
    if new_width > Document::MAX_WIDTH || new_height > Document::MAX_HEIGHT {
        return Err(ResizeError::TooLarge {
            width: new_width,
            height: new_height,
            max_width: Document::MAX_WIDTH,
            max_height: Document::MAX_HEIGHT,
        });
    }
    if new_width == doc.width && new_height == doc.height {
        return Ok(None);
    }
    let before = DocSnapshot { extent: doc.extent(), layers: doc.layers.clone() };
    let after_layers = doc
        .layers
        .iter()
        .map(|l| resize_layer(l, doc.width, doc.height, new_width, new_height))
        .collect();
    let after = DocSnapshot {
        extent: DocExtent { width: new_width, height: new_height },
        layers: after_layers,
    };
    Ok(Some(Edit::Resize { before, after }))
}

fn resize_layer(old: &Layer, old_w: u16, old_h: u16, new_w: u16, new_h: u16) -> Layer {
    let mut cells = vec![Cell::BLANK; new_w as usize * new_h as usize];
    let old_cells = old.cells();
    for y in 0..old_h.min(new_h) {
        for x in 0..old_w.min(new_w) {
            let src = y as usize * old_w as usize + x as usize;
            let dst = y as usize * new_w as usize + x as usize;
            cells[dst] = old_cells[src];
        }
    }
    Layer::from_cells(cells)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::History;
    use crate::model::Rgba;

    fn cell(ch: char) -> Cell {
        Cell { ch, fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }
    }

    #[test]
    fn grow_preserves_top_left_content_and_pads_the_rest_with_blank() {
        let mut doc = Document::new(3, 3);
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(0, 2, 2, cell('z'));
        let edit = resize_document(&doc, 5, 5).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.width, 5);
        assert_eq!(doc.height, 5);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));
        assert_eq!(doc.cell(0, 2, 2), Some(&cell('z')));
        // Newly padded region is Blank.
        for y in 0..5u16 {
            for x in 0..5u16 {
                if (x, y) != (0, 0) && (x, y) != (2, 2) {
                    assert_eq!(doc.cell(0, x, y), Some(&Cell::BLANK));
                }
            }
        }
    }

    #[test]
    fn shrink_crops_from_bottom_right_preserving_top_left_content() {
        let mut doc = Document::new(5, 5);
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(0, 1, 1, cell('b'));
        doc.set_cell(0, 4, 4, cell('z')); // will be cropped away
        let edit = resize_document(&doc, 2, 2).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.width, 2);
        assert_eq!(doc.height, 2);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));
        assert_eq!(doc.cell(0, 1, 1), Some(&cell('b')));
    }

    #[test]
    fn grow_then_undo_restores_the_exact_prior_extent_and_content() {
        let mut doc = Document::new(3, 3);
        doc.set_cell(0, 1, 1, cell('m'));
        let before = doc.clone();
        let edit = resize_document(&doc, 6, 6).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_ne!(doc, before);
        assert!(history.undo(&mut doc));
        assert_eq!(doc, before);
    }

    #[test]
    fn shrink_then_undo_restores_the_cropped_away_cells_byte_exact() {
        let mut doc = Document::new(5, 5);
        for y in 0..5u16 {
            for x in 0..5u16 {
                doc.set_cell(0, x, y, cell((b'a' + (x + y * 5) as u8) as char));
            }
        }
        let before = doc.clone();
        let edit = resize_document(&doc, 2, 2).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.width, 2);
        assert!(history.undo(&mut doc));
        assert_eq!(doc, before, "undo must resurrect the cropped-away cells exactly");
    }

    #[test]
    fn resize_to_the_same_extent_is_a_no_op_with_no_edit() {
        let doc = Document::new(10, 10);
        assert_eq!(resize_document(&doc, 10, 10).unwrap(), None);
    }

    #[test]
    fn zero_width_or_height_is_rejected() {
        let doc = Document::new(10, 10);
        assert_eq!(resize_document(&doc, 0, 10), Err(ResizeError::ZeroExtent));
        assert_eq!(resize_document(&doc, 10, 0), Err(ResizeError::ZeroExtent));
    }

    #[test]
    fn over_cap_dimension_is_rejected_before_allocating_and_returns_promptly() {
        let doc = Document::new(10, 10);
        let started = std::time::Instant::now();
        let result = resize_document(&doc, u16::MAX, u16::MAX);
        assert!(started.elapsed() < std::time::Duration::from_millis(200), "must reject before allocating, not after");
        assert_eq!(
            result,
            Err(ResizeError::TooLarge {
                width: u16::MAX,
                height: u16::MAX,
                max_width: Document::MAX_WIDTH,
                max_height: Document::MAX_HEIGHT,
            })
        );
    }

    #[test]
    fn width_or_height_exactly_at_the_cap_is_accepted() {
        let doc = Document::new(10, 10);
        assert!(resize_document(&doc, Document::MAX_WIDTH, Document::MAX_HEIGHT).is_ok());
    }

    #[test]
    fn multi_layer_document_resizes_every_layer_consistently() {
        let mut doc = Document::new(3, 3);
        doc.layers.push(Layer::blank(3, 3));
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(1, 0, 0, cell('b'));
        let edit = resize_document(&doc, 5, 5).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));
        assert_eq!(doc.cell(1, 0, 0), Some(&cell('b')));
        assert_eq!(doc.layers[0].cells().len(), 25);
        assert_eq!(doc.layers[1].cells().len(), 25);
    }

    #[test]
    fn grow_in_one_dimension_and_shrink_in_the_other_at_once() {
        let mut doc = Document::new(4, 2);
        doc.set_cell(0, 3, 1, cell('e')); // will be cropped away (x=3 is out of a width-2 result)
        doc.set_cell(0, 0, 1, cell('k'));
        let edit = resize_document(&doc, 2, 6).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.width, 2);
        assert_eq!(doc.height, 6);
        assert_eq!(doc.cell(0, 0, 1), Some(&cell('k')));
        assert_eq!(doc.cell(0, 1, 5), Some(&Cell::BLANK));
    }
}
