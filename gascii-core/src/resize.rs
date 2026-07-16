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

/// Where existing content lands on one axis when that axis's extent changes. `Start` is the
/// historical top-left behavior (grow pads at the end, shrink crops from the end).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AxisAnchor {
    #[default]
    Start,
    Center,
    End,
}

/// The 3x3 anchor a resize is performed against: independent per axis, so e.g. `h: Center, v:
/// Start` anchors horizontally-centered but top-aligned.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ResizeAnchor {
    pub h: AxisAnchor,
    pub v: AxisAnchor,
}

/// The signed offset applied to old-axis coordinates to place them in the new axis: `dst = src +
/// offset`. `Center`'s `(new - old) / 2` truncates toward zero in `i32` — a grow/shrink with an odd
/// difference biases the extra cell toward the end, matching `Start`'s and `Raw` footprint's own
/// existing right/down bias rather than introducing a third rounding convention.
fn axis_offset(anchor: AxisAnchor, old: u16, new: u16) -> i32 {
    match anchor {
        AxisAnchor::Start => 0,
        AxisAnchor::Center => (new as i32 - old as i32) / 2,
        AxisAnchor::End => new as i32 - old as i32,
    }
}

/// Builds the `Edit::Resize` for growing/shrinking `doc` to `new_width x new_height`, anchored per
/// `anchor` (grow pads Blank on the side(s) opposite the anchor; shrink crops from the side(s)
/// opposite the anchor). Validates the requested extent against `Document::MAX_WIDTH`/`MAX_HEIGHT`
/// *before* allocating anything sized by it — the same untrusted-size discipline the `.gascii`
/// loader and paste already apply, even though this particular size originates from the app's own
/// resize dialog (belt-and-suspenders). Returns `Ok(None)` for a same-size no-op (no empty undo
/// entry).
pub fn resize_document(
    doc: &Document,
    new_width: u16,
    new_height: u16,
    anchor: ResizeAnchor,
) -> Result<Option<Edit>, ResizeError> {
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
    let dx = axis_offset(anchor.h, doc.width, new_width);
    let dy = axis_offset(anchor.v, doc.height, new_height);
    let before = DocSnapshot { extent: doc.extent(), layers: doc.layers.clone() };
    let after_layers = doc
        .layers
        .iter()
        .map(|l| resize_layer(l, doc.width, doc.height, new_width, new_height, dx, dy))
        .collect();
    let after = DocSnapshot {
        extent: DocExtent { width: new_width, height: new_height },
        layers: after_layers,
    };
    Ok(Some(Edit::Resize { before, after }))
}

/// Blits `old`'s cells into a `new_w x new_h` buffer, each source `(x, y)` landing at `(x + dx, y +
/// dy)`. A source cell is copied only when both its source and destination coordinates are
/// in-bounds; every other destination cell stays `Cell::BLANK`.
fn resize_layer(old: &Layer, old_w: u16, old_h: u16, new_w: u16, new_h: u16, dx: i32, dy: i32) -> Layer {
    let mut cells = vec![Cell::BLANK; new_w as usize * new_h as usize];
    let old_cells = old.cells();
    for y in 0..old_h {
        let dst_y = y as i32 + dy;
        if dst_y < 0 || dst_y >= new_h as i32 {
            continue;
        }
        for x in 0..old_w {
            let dst_x = x as i32 + dx;
            if dst_x < 0 || dst_x >= new_w as i32 {
                continue;
            }
            let src = y as usize * old_w as usize + x as usize;
            let dst = dst_y as usize * new_w as usize + dst_x as usize;
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

    fn start() -> ResizeAnchor {
        ResizeAnchor::default()
    }

    #[test]
    fn grow_preserves_top_left_content_and_pads_the_rest_with_blank() {
        let mut doc = Document::new(3, 3);
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(0, 2, 2, cell('z'));
        let edit = resize_document(&doc, 5, 5, start()).unwrap().unwrap();
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
        let edit = resize_document(&doc, 2, 2, start()).unwrap().unwrap();
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
        let edit = resize_document(&doc, 6, 6, start()).unwrap().unwrap();
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
        let edit = resize_document(&doc, 2, 2, start()).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.width, 2);
        assert!(history.undo(&mut doc));
        assert_eq!(doc, before, "undo must resurrect the cropped-away cells exactly");
    }

    #[test]
    fn resize_to_the_same_extent_is_a_no_op_with_no_edit() {
        let doc = Document::new(10, 10);
        assert_eq!(resize_document(&doc, 10, 10, start()).unwrap(), None);
    }

    #[test]
    fn zero_width_or_height_is_rejected() {
        let doc = Document::new(10, 10);
        assert_eq!(resize_document(&doc, 0, 10, start()), Err(ResizeError::ZeroExtent));
        assert_eq!(resize_document(&doc, 10, 0, start()), Err(ResizeError::ZeroExtent));
    }

    #[test]
    fn over_cap_dimension_is_rejected_before_allocating_and_returns_promptly() {
        let doc = Document::new(10, 10);
        let started = std::time::Instant::now();
        let result = resize_document(&doc, u16::MAX, u16::MAX, start());
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
        assert!(resize_document(&doc, Document::MAX_WIDTH, Document::MAX_HEIGHT, start()).is_ok());
    }

    #[test]
    fn multi_layer_document_resizes_every_layer_consistently() {
        let mut doc = Document::new(3, 3);
        doc.layers.push(Layer::blank(3, 3));
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(1, 0, 0, cell('b'));
        let edit = resize_document(&doc, 5, 5, start()).unwrap().unwrap();
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
        let edit = resize_document(&doc, 2, 6, start()).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.width, 2);
        assert_eq!(doc.height, 6);
        assert_eq!(doc.cell(0, 0, 1), Some(&cell('k')));
        assert_eq!(doc.cell(0, 1, 5), Some(&Cell::BLANK));
    }

    // --- anchored resize ---

    /// All nine anchors on an asymmetric grow: content must land exactly where each anchor
    /// promises, not merely "somewhere that happens to still contain it".
    #[test]
    fn every_anchor_places_content_where_promised_on_an_asymmetric_grow() {
        // 2x2 doc, corners marked, growing to 6x4 (dx range 0..4, dy range 0..2).
        let mut doc = Document::new(2, 2);
        doc.set_cell(0, 0, 0, cell('a')); // top-left
        doc.set_cell(0, 1, 1, cell('z')); // bottom-right

        type Case = (AxisAnchor, AxisAnchor, (u16, u16), (u16, u16));
        let cases: [Case; 9] = [
            (AxisAnchor::Start, AxisAnchor::Start, (0, 0), (1, 1)),
            (AxisAnchor::Center, AxisAnchor::Start, (2, 0), (3, 1)),
            (AxisAnchor::End, AxisAnchor::Start, (4, 0), (5, 1)),
            (AxisAnchor::Start, AxisAnchor::Center, (0, 1), (1, 2)),
            (AxisAnchor::Center, AxisAnchor::Center, (2, 1), (3, 2)),
            (AxisAnchor::End, AxisAnchor::Center, (4, 1), (5, 2)),
            (AxisAnchor::Start, AxisAnchor::End, (0, 2), (1, 3)),
            (AxisAnchor::Center, AxisAnchor::End, (2, 2), (3, 3)),
            (AxisAnchor::End, AxisAnchor::End, (4, 2), (5, 3)),
        ];
        for (h, v, top_left, bottom_right) in cases {
            let anchor = ResizeAnchor { h, v };
            let mut d = doc.clone();
            let edit = resize_document(&d, 6, 4, anchor).unwrap().unwrap();
            let mut history = History::new();
            history.apply(&mut d, edit);
            assert_eq!(
                d.cell(0, top_left.0, top_left.1),
                Some(&cell('a')),
                "h={h:?} v={v:?}: 'a' should land at {top_left:?}"
            );
            assert_eq!(
                d.cell(0, bottom_right.0, bottom_right.1),
                Some(&cell('z')),
                "h={h:?} v={v:?}: 'z' should land at {bottom_right:?}"
            );
        }
    }

    #[test]
    fn end_anchored_shrink_crops_from_the_start_side() {
        // 5x5 doc, shrinking to 2x2 End/End must keep the bottom-right 2x2 block.
        let mut doc = Document::new(5, 5);
        doc.set_cell(0, 3, 3, cell('k'));
        doc.set_cell(0, 4, 4, cell('z'));
        doc.set_cell(0, 0, 0, cell('x')); // will be cropped away
        let anchor = ResizeAnchor { h: AxisAnchor::End, v: AxisAnchor::End };
        let edit = resize_document(&doc, 2, 2, anchor).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('k')));
        assert_eq!(doc.cell(0, 1, 1), Some(&cell('z')));
    }

    /// `Center`'s `(new - old) / 2` truncates toward zero: an odd-delta grow biases the extra cell
    /// toward the end, not the start. Pinned explicitly so the bias is a documented, tested
    /// behavior rather than an implicit consequence of integer division.
    #[test]
    fn center_anchor_with_an_odd_delta_biases_the_extra_cell_toward_the_end() {
        // 1x1 -> 4x1: delta 3, (3)/2 == 1 (truncated), so offset is 1: the single source cell lands
        // at x=1 out of [0,1,2,3], leaving 1 blank cell before it and 2 after — the extra cell is
        // on the end side.
        let mut doc = Document::new(1, 1);
        doc.set_cell(0, 0, 0, cell('m'));
        let anchor = ResizeAnchor { h: AxisAnchor::Center, v: AxisAnchor::Start };
        let edit = resize_document(&doc, 4, 1, anchor).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.cell(0, 1, 0), Some(&cell('m')), "content should land at the truncated offset 1");
        assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 2, 0), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 3, 0), Some(&Cell::BLANK));
    }

    #[test]
    fn grow_one_axis_shrink_the_other_with_a_non_start_anchor() {
        let mut doc = Document::new(4, 4);
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(0, 3, 3, cell('z')); // will be cropped on the shrinking axis
        let anchor = ResizeAnchor { h: AxisAnchor::End, v: AxisAnchor::Start };
        // width shrinks 4 -> 2 (End anchored: keep the rightmost 2 columns), height grows 4 -> 6.
        let edit = resize_document(&doc, 2, 6, anchor).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.width, 2);
        assert_eq!(doc.height, 6);
        // 'a' at old (0,0) is cropped away (End anchor keeps columns [2,3] of the old 4-wide doc).
        assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK));
    }

    #[test]
    fn undo_round_trips_a_non_start_anchored_resize() {
        let mut doc = Document::new(3, 3);
        doc.set_cell(0, 1, 1, cell('m'));
        let before = doc.clone();
        let anchor = ResizeAnchor { h: AxisAnchor::Center, v: AxisAnchor::End };
        let edit = resize_document(&doc, 7, 7, anchor).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_ne!(doc, before);
        assert!(history.undo(&mut doc));
        assert_eq!(doc, before);
    }
}
