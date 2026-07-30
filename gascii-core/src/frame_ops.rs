//! Frame-collection structural edits: produces an `Edit` through the same pure-fn-returns-Edit
//! contract `resize_document`/`clear_document` already use, so `History` stays the sole place that
//! ever actually mutates `Document`'s frame collection.

use crate::edit::Edit;
use crate::model::{Document, Frame};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameOpError {
    IndexOutOfBounds { index: usize, frame_count: usize },
    TooManyFrames { found: usize, max: usize },
    /// `remove_frame` refuses to remove a document's only frame — a document with zero frames
    /// must stay unreachable.
    LastFrame,
    TotalCellBudgetExceeded { total_cells: u128, max: usize },
    /// A single inserted frame's own `layers.len()` exceeds `Document::MAX_LAYERS` — independent of
    /// the joint budget above, which only bounds the sum across every frame.
    TooManyLayers { found: usize, max: usize },
}

/// How `active` shifts when a new element lands at index `at` in a `Vec::insert`.
fn shift_for_insert(active: usize, at: usize) -> usize {
    if at <= active {
        active + 1
    } else {
        active
    }
}

/// How `active` shifts when the element at `removed` is taken out via `Vec::remove`, given the
/// vec's `new_len` (post-removal).
fn shift_for_remove(active: usize, removed: usize, new_len: usize) -> usize {
    use std::cmp::Ordering;
    match active.cmp(&removed) {
        Ordering::Less => active,
        Ordering::Equal => active.min(new_len.saturating_sub(1)),
        Ordering::Greater => active - 1,
    }
}

/// How `active` shifts when the element at `from` is moved to `to` (a `Vec::remove` immediately
/// followed by a `Vec::insert` at the new position, as `ReorderFrame` performs it).
fn shift_for_move(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < to && active > from && active <= to {
        active - 1
    } else if to < from && active >= to && active < from {
        active + 1
    } else {
        active
    }
}

/// Sum of every frame's layer count once `extra` frames of `layers_per_extra` layers each are
/// added, times `width x height` — the joint budget `Document::MAX_TOTAL_CELLS` bounds. `u128` to
/// stay overflow-free against worst-case `u16` extents and `usize` counts multiplied together.
fn total_cells(doc: &Document, extra_layers_total: usize) -> u128 {
    let existing_layers: usize = (0..doc.frame_count())
        .map(|i| doc.frame(i).map(|f| f.layers.len()).unwrap_or(0))
        .sum();
    (existing_layers as u128 + extra_layers_total as u128)
        * doc.width as u128
        * doc.height as u128
}

fn check_caps(
    doc: &Document,
    new_frame_count: usize,
    extra_layers_total: usize,
    inserted_frame_layers: usize,
) -> Result<(), FrameOpError> {
    if new_frame_count > Document::MAX_FRAMES {
        return Err(FrameOpError::TooManyFrames { found: new_frame_count, max: Document::MAX_FRAMES });
    }
    if inserted_frame_layers > Document::MAX_LAYERS {
        return Err(FrameOpError::TooManyLayers { found: inserted_frame_layers, max: Document::MAX_LAYERS });
    }
    let total = total_cells(doc, extra_layers_total);
    if total > Document::MAX_TOTAL_CELLS as u128 {
        return Err(FrameOpError::TotalCellBudgetExceeded { total_cells: total, max: Document::MAX_TOTAL_CELLS });
    }
    Ok(())
}

/// Inserts `frame` at `at` (clamped nowhere within this crate — every in-crate caller is expected
/// to pass a valid index, and this function itself still panics on one that isn't, exactly like
/// `Vec::insert`; `at == doc.frame_count()` appends). Validates `MAX_FRAMES`/the inserted frame's
/// own `MAX_LAYERS`/`MAX_TOTAL_CELLS` before returning the `Edit` — defense in depth, mirroring
/// `resize_document`'s own belt-and-suspenders validation regardless of where the size originated.
///
/// The `Edit` this returns is still checked again at the one boundary that actually needs to
/// survive a bad index without panicking: `History::apply` rejects a structural edit whose index
/// no longer matches the document's shape as a silent no-op (see `edit.rs`'s
/// `structural_edit_is_valid`) — that's the defense against an `Edit::AddFrame`/etc. arriving from
/// outside this crate (e.g. a plugin's `PanelOutcome.edits`), not this function's own contract.
pub fn add_frame(doc: &Document, at: usize, frame: Frame) -> Result<Edit, FrameOpError> {
    check_caps(doc, doc.frame_count() + 1, frame.layers.len(), frame.layers.len())?;
    let active_before = doc.active_frame();
    let active_after = shift_for_insert(active_before, at);
    Ok(Edit::AddFrame { index: at, frame, active_frame_before: active_before, active_frame_after: active_after })
}

/// Clones frame `index` and inserts the clone immediately after it.
pub fn duplicate_frame(doc: &Document, index: usize) -> Result<Edit, FrameOpError> {
    let frame_count = doc.frame_count();
    let source = doc.frame(index).ok_or(FrameOpError::IndexOutOfBounds { index, frame_count })?;
    add_frame(doc, index + 1, source.clone())
}

/// Removes the frame at `index`. Errs `LastFrame` if `doc` has only one frame — a document with
/// zero frames must stay unreachable.
pub fn remove_frame(doc: &Document, index: usize) -> Result<Edit, FrameOpError> {
    let frame_count = doc.frame_count();
    if frame_count <= 1 {
        return Err(FrameOpError::LastFrame);
    }
    let frame = doc.frame(index).ok_or(FrameOpError::IndexOutOfBounds { index, frame_count })?.clone();
    let active_before = doc.active_frame();
    let active_after = shift_for_remove(active_before, index, frame_count - 1);
    Ok(Edit::RemoveFrame { index, frame, active_frame_before: active_before, active_frame_after: active_after })
}

/// Moves the frame at `from` to `to`. `Ok(None)` for a `from == to` no-op (no empty undo entry).
pub fn reorder_frame(doc: &Document, from: usize, to: usize) -> Result<Option<Edit>, FrameOpError> {
    let frame_count = doc.frame_count();
    if from >= frame_count {
        return Err(FrameOpError::IndexOutOfBounds { index: from, frame_count });
    }
    if to >= frame_count {
        return Err(FrameOpError::IndexOutOfBounds { index: to, frame_count });
    }
    if from == to {
        return Ok(None);
    }
    let active_before = doc.active_frame();
    let active_after = shift_for_move(active_before, from, to);
    Ok(Some(Edit::ReorderFrame { from, to, active_frame_before: active_before, active_frame_after: active_after }))
}

/// Sets frame `index`'s duration override. `Ok(None)` if `duration_ms` already matches the
/// current override (no empty undo entry).
pub fn set_frame_duration(doc: &Document, index: usize, duration_ms: Option<u32>) -> Result<Option<Edit>, FrameOpError> {
    let frame_count = doc.frame_count();
    let current = doc.frame(index).ok_or(FrameOpError::IndexOutOfBounds { index, frame_count })?.duration_override;
    if current == duration_ms {
        return Ok(None);
    }
    Ok(Some(Edit::SetFrameDuration { index, before: current, after: duration_ms }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::History;
    use crate::model::{Cell, Layer};

    fn blank(w: u16, h: u16) -> Frame {
        Frame::blank(w, h)
    }

    #[test]
    fn add_frame_at_zero_shifts_a_lower_active_frame_forward() {
        let doc = Document::new(3, 3);
        let edit = add_frame(&doc, 0, blank(3, 3)).unwrap();
        let Edit::AddFrame { active_frame_before, active_frame_after, .. } = edit else { panic!("expected AddFrame") };
        assert_eq!(active_frame_before, 0);
        assert_eq!(active_frame_after, 1, "inserting at/before the active index shifts it forward");
    }

    #[test]
    fn add_frame_after_active_frame_leaves_it_unchanged() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_frame(&doc, 1, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // now 2 frames, active still 0
        let edit = add_frame(&doc, 2, blank(3, 3)).unwrap();
        let Edit::AddFrame { active_frame_after, .. } = edit else { panic!("expected AddFrame") };
        assert_eq!(active_frame_after, 0, "inserting after the active index leaves it unchanged");
    }

    #[test]
    fn remove_frame_before_active_frame_shifts_it_back_by_one() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_frame(&doc, 0, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // 2 frames, active now 1
        let edit = add_frame(&doc, 0, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // 3 frames, active now 2
        assert_eq!(doc.active_frame(), 2);

        let edit = remove_frame(&doc, 0).unwrap();
        let Edit::RemoveFrame { active_frame_after, .. } = edit else { panic!("expected RemoveFrame") };
        assert_eq!(active_frame_after, 1, "removing a frame before the active index shifts it back by one");
    }

    #[test]
    fn remove_frame_at_active_frame_clamps_to_the_new_last_valid_index() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_frame(&doc, 1, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // 2 frames, active 0
        assert_eq!(doc.frame_count(), 2);
        assert_eq!(doc.active_frame(), 0);

        let edit = remove_frame(&doc, 0).unwrap();
        let Edit::RemoveFrame { active_frame_after, .. } = edit else { panic!("expected RemoveFrame") };
        assert_eq!(active_frame_after, 0, "removing the active frame clamps to the new last valid index");
    }

    #[test]
    fn remove_frame_refuses_to_remove_a_documents_only_frame() {
        let doc = Document::new(3, 3);
        assert_eq!(remove_frame(&doc, 0), Err(FrameOpError::LastFrame));
    }

    #[test]
    fn reorder_frame_moving_the_active_frame_itself_follows_it_to_the_destination() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_frame(&doc, 1, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // 2 frames, active 0
        let edit = add_frame(&doc, 2, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // 3 frames, active 0

        let edit = reorder_frame(&doc, 0, 2).unwrap().unwrap();
        let Edit::ReorderFrame { active_frame_after, .. } = edit else { panic!("expected ReorderFrame") };
        assert_eq!(active_frame_after, 2, "moving the active frame itself follows it to the destination");
    }

    #[test]
    fn reorder_frame_shifting_frames_between_from_and_to_adjusts_an_uninvolved_active_frame() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_frame(&doc, 1, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // 2 frames, active 0
        let edit = add_frame(&doc, 2, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // 3 frames, active 0
        let edit = add_frame(&doc, 3, blank(3, 3)).unwrap();
        history.apply(&mut doc, edit); // 4 frames, active 0

        // Move active frame 0 aside first so the active index (now 1, after the moves below) sits
        // strictly between `from` and `to` for the case under test.
        let edit = reorder_frame(&doc, 0, 1).unwrap().unwrap();
        history.apply(&mut doc, edit); // active now 1
        assert_eq!(doc.active_frame(), 1);

        let edit = reorder_frame(&doc, 0, 2).unwrap().unwrap();
        let Edit::ReorderFrame { active_frame_after, .. } = edit else { panic!("expected ReorderFrame") };
        assert_eq!(
            active_frame_after, 0,
            "moving a frame from before to after the active index shifts the uninvolved active index back by one"
        );
    }

    #[test]
    fn reorder_frame_from_equals_to_is_a_no_op_with_no_edit() {
        let doc = Document::new(3, 3);
        assert_eq!(reorder_frame(&doc, 0, 0), Ok(None));
    }

    #[test]
    fn duplicate_frame_inserts_a_deep_clone_immediately_after_the_source() {
        let mut doc = Document::new(3, 3);
        doc.set_cell(0, 0, 0, Cell { ch: 'D', ..Cell::BLANK });

        let edit = duplicate_frame(&doc, 0).unwrap();
        let Edit::AddFrame { index, ref frame, .. } = edit else { panic!("expected AddFrame") };
        assert_eq!(index, 1, "the clone lands immediately after the source");
        assert_eq!(frame.layers[0].cells()[0].ch, 'D', "the clone carries the source's content");

        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.frame_count(), 2);
        assert_eq!(doc.cell_at(1, 0, 0, 0).unwrap().ch, 'D');
    }

    /// Timing-asserted like `resize.rs`'s `over_cap_dimension_is_rejected_before_allocating_and_
    /// returns_promptly`: rejection must be prompt, not proportional to the declared frame count.
    #[test]
    fn add_frame_over_max_frames_is_rejected() {
        let mut doc = Document::new(2, 2);
        let mut history = History::new();
        for i in 0..Document::MAX_FRAMES - 1 {
            let edit = add_frame(&doc, i, blank(2, 2)).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.frame_count(), Document::MAX_FRAMES);

        let started = std::time::Instant::now();
        let result = add_frame(&doc, 0, blank(2, 2));
        assert!(started.elapsed() < std::time::Duration::from_millis(200), "must reject before allocating, not after");
        assert_eq!(result, Err(FrameOpError::TooManyFrames { found: Document::MAX_FRAMES + 1, max: Document::MAX_FRAMES }));
    }

    #[test]
    fn add_frame_over_the_total_cell_budget_is_rejected() {
        // A single, large multi-layer frame pushed onto a document already at the width x height
        // that makes one additional MAX_LAYERS-sized frame exceed MAX_TOTAL_CELLS.
        let doc = Document::new(Document::MAX_WIDTH, Document::MAX_HEIGHT);
        let big_frame = Frame {
            layers: (0..Document::MAX_LAYERS).map(|_| Layer::blank(Document::MAX_WIDTH, Document::MAX_HEIGHT)).collect(),
            duration_override: None,
        };
        let result = add_frame(&doc, 1, big_frame);
        assert!(matches!(result, Err(FrameOpError::TotalCellBudgetExceeded { .. })));
    }

    /// `add_frame` must refuse a single frame whose own layer count exceeds `MAX_LAYERS`,
    /// independent of the joint `MAX_TOTAL_CELLS` budget (a tiny document's joint budget stays well
    /// under cap even with one over-sized frame, so only a dedicated per-frame check catches this).
    #[test]
    fn add_frame_with_more_layers_than_max_layers_is_rejected() {
        let doc = Document::new(2, 2);
        let over_max = Frame {
            layers: (0..=Document::MAX_LAYERS).map(|_| Layer::blank(2, 2)).collect(),
            duration_override: None,
        };
        let result = add_frame(&doc, 1, over_max);
        assert_eq!(
            result,
            Err(FrameOpError::TooManyLayers { found: Document::MAX_LAYERS + 1, max: Document::MAX_LAYERS })
        );
    }

    #[test]
    fn set_frame_duration_to_the_same_value_returns_none() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = set_frame_duration(&doc, 0, Some(75)).unwrap().unwrap();
        history.apply(&mut doc, edit);
        assert_eq!(set_frame_duration(&doc, 0, Some(75)).unwrap(), None);
    }
}
