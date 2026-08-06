//! Layer-collection structural edits: produces an `Edit` through the same pure-fn-returns-Edit
//! contract `frame_ops.rs`/`resize_document`/`clear_document` already use, so `History` stays the
//! sole place that ever actually mutates a `Document`'s layer collection.
//!
//! `add_layer`/`duplicate_layer` and `frame_ops::add_frame`/`duplicate_frame` share one selection
//! rule: the newly inserted element's own index becomes the active one (`active_*_after ==
//! index`) — the user's very next action is almost always on the layer/frame just added or
//! duplicated, so it should already be selected. Undo restores the previously active index.
//!
//! `remove_layer`/`reorder_layer` reuse `frame_ops`'s `shift_for_remove`/`shift_for_move` verbatim —
//! removing or reordering an *uninvolved* active layer shifts by exactly the same rule as the frame
//! case, so there is nothing layer-specific to re-derive there.

use crate::edit::Edit;
use crate::model::{Document, Layer, LayerMeta};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayerOpError {
    IndexOutOfBounds {
        index: usize,
        layer_count: usize,
    },
    TooManyLayers {
        found: usize,
        max: usize,
    },
    /// `remove_layer` refuses to remove a document's only layer — a document with zero layers must
    /// stay unreachable, mirroring `FrameOpError::LastFrame`.
    LastLayer,
    TotalCellBudgetExceeded {
        total_cells: u128,
        max: usize,
    },
    /// Every frame must already report the same layer count as `doc.layer_count()` — the permissive-
    /// metadata contract's one hard safety guarantee (see `LayerMeta`'s field doc on `Document`).
    /// Reachable only from a hand-built or corrupted `Document` that bypassed both the loader and
    /// this module's own mutation path.
    LayerCountDesync,
}

/// Whether every frame's `layers.len()` currently matches `doc.layer_count()` — the invariant every
/// structural function below must hold before it's safe to index frame `f`'s layers by a
/// `layer_meta`-relative position.
fn frames_consistent(doc: &Document) -> bool {
    let len = doc.layer_count();
    doc.frames.iter().all(|f| f.layers.len() == len)
}

/// Sum of every frame's layer count once one new layer is added to *every* frame (`AddLayer`/
/// `RemoveLayer` touch every frame at once, unlike a frame-structural edit which touches one frame),
/// times `width x height` — the joint budget `Document::MAX_TOTAL_CELLS` bounds. `u128` to stay
/// overflow-free against worst-case `u16` extents and `usize` counts multiplied together.
fn total_cells_after_one_more_layer(doc: &Document) -> u128 {
    let existing_layers: usize = (0..doc.frame_count())
        .map(|i| doc.frame(i).map(|f| f.layers.len()).unwrap_or(0))
        .sum();
    (existing_layers as u128 + doc.frame_count() as u128) * doc.width as u128 * doc.height as u128
}

fn check_caps(doc: &Document) -> Result<(), LayerOpError> {
    let new_layer_count = doc.layer_count() + 1;
    if new_layer_count > Document::MAX_LAYERS {
        return Err(LayerOpError::TooManyLayers {
            found: new_layer_count,
            max: Document::MAX_LAYERS,
        });
    }
    let total = total_cells_after_one_more_layer(doc);
    if total > Document::MAX_TOTAL_CELLS as u128 {
        return Err(LayerOpError::TotalCellBudgetExceeded {
            total_cells: total,
            max: Document::MAX_TOTAL_CELLS,
        });
    }
    Ok(())
}

/// Inserts a new blank layer at `at` in every frame. The new layer becomes active
/// (`active_layer_after == at`) — see the module doc's divergence note.
pub fn add_layer(doc: &Document, at: usize) -> Result<Edit, LayerOpError> {
    if !frames_consistent(doc) {
        return Err(LayerOpError::LayerCountDesync);
    }
    check_caps(doc)?;
    let layers = vec![Layer::blank(doc.width, doc.height); doc.frame_count()];
    let meta = LayerMeta::default_named(at);
    Ok(Edit::AddLayer {
        index: at,
        layers,
        meta,
        active_layer_before: doc.active_layer(),
        active_layer_after: at,
    })
}

/// Clones layer `index` (across every frame) and inserts the clone immediately after it. The
/// duplicate becomes active (`active_layer_after == index + 1`) — see the module doc's divergence
/// note.
pub fn duplicate_layer(doc: &Document, index: usize) -> Result<Edit, LayerOpError> {
    if !frames_consistent(doc) {
        return Err(LayerOpError::LayerCountDesync);
    }
    let layer_count = doc.layer_count();
    if index >= layer_count {
        return Err(LayerOpError::IndexOutOfBounds { index, layer_count });
    }
    check_caps(doc)?;
    let layers: Vec<Layer> = doc.frames.iter().map(|f| f.layers[index].clone()).collect();
    let meta = LayerMeta {
        name: format!("{} copy", doc.layer_name(index).unwrap_or_default()),
        visible: doc.layer_meta[index].visible,
    };
    let at = index + 1;
    Ok(Edit::AddLayer {
        index: at,
        layers,
        meta,
        active_layer_before: doc.active_layer(),
        active_layer_after: at,
    })
}

/// Removes the layer at `index` from every frame. Errs `LastLayer` if `doc` has only one layer — a
/// document with zero layers must stay unreachable.
pub fn remove_layer(doc: &Document, index: usize) -> Result<Edit, LayerOpError> {
    let layer_count = doc.layer_count();
    if layer_count <= 1 {
        return Err(LayerOpError::LastLayer);
    }
    if index >= layer_count {
        return Err(LayerOpError::IndexOutOfBounds { index, layer_count });
    }
    if !frames_consistent(doc) {
        return Err(LayerOpError::LayerCountDesync);
    }
    let layers: Vec<Layer> = doc.frames.iter().map(|f| f.layers[index].clone()).collect();
    let meta = doc.layer_meta[index].clone();
    let active_before = doc.active_layer();
    let active_after = crate::frame_ops::shift_for_remove(active_before, index, layer_count - 1);
    Ok(Edit::RemoveLayer {
        index,
        layers,
        meta,
        active_layer_before: active_before,
        active_layer_after: active_after,
    })
}

/// Moves the layer at `from` to `to` (in every frame). `Ok(None)` for a `from == to` no-op (no
/// empty undo entry).
pub fn reorder_layer(doc: &Document, from: usize, to: usize) -> Result<Option<Edit>, LayerOpError> {
    let layer_count = doc.layer_count();
    if from >= layer_count {
        return Err(LayerOpError::IndexOutOfBounds {
            index: from,
            layer_count,
        });
    }
    if to >= layer_count {
        return Err(LayerOpError::IndexOutOfBounds {
            index: to,
            layer_count,
        });
    }
    if from == to {
        return Ok(None);
    }
    if !frames_consistent(doc) {
        return Err(LayerOpError::LayerCountDesync);
    }
    let active_before = doc.active_layer();
    let active_after = crate::frame_ops::shift_for_move(active_before, from, to);
    Ok(Some(Edit::ReorderLayer {
        from,
        to,
        active_layer_before: active_before,
        active_layer_after: active_after,
    }))
}

/// Sets layer `index`'s visibility. `Ok(None)` if `visible` already matches the current value (no
/// empty undo entry).
pub fn set_layer_visibility(
    doc: &Document,
    index: usize,
    visible: bool,
) -> Result<Option<Edit>, LayerOpError> {
    let layer_count = doc.layer_count();
    let current = doc
        .layer_meta
        .get(index)
        .ok_or(LayerOpError::IndexOutOfBounds { index, layer_count })?
        .visible;
    if current == visible {
        return Ok(None);
    }
    Ok(Some(Edit::SetLayerVisibility {
        index,
        before: current,
        after: visible,
    }))
}

/// Sets layer `index`'s name, truncated to `LayerMeta::MAX_NAME_LEN` — the same cap the format
/// loader enforces, so a plugin-driven rename can't write an unbounded name any more than a
/// hand-crafted file can. `Ok(None)` if the (already-clamped) name already matches the current
/// value (no empty undo entry).
pub fn set_layer_name(
    doc: &Document,
    index: usize,
    name: String,
) -> Result<Option<Edit>, LayerOpError> {
    let layer_count = doc.layer_count();
    let current = doc
        .layer_meta
        .get(index)
        .ok_or(LayerOpError::IndexOutOfBounds { index, layer_count })?
        .name
        .clone();
    let name = LayerMeta::clamp_name(name);
    if current == name {
        return Ok(None);
    }
    Ok(Some(Edit::SetLayerName {
        index,
        before: current,
        after: name,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::History;
    use crate::model::{Cell, Frame, Rgba};

    #[test]
    fn add_layer_at_zero_makes_the_new_layer_active_rather_than_shifting_the_old_cursor() {
        // Contrast with frame_ops's `add_frame_at_zero_shifts_a_lower_active_frame_forward`: a
        // frame insert at/before the active index shifts the *old* cursor forward (active_after ==
        // active_before + 1). A layer insert instead always selects the *new* layer.
        let doc = Document::new(3, 3);
        let edit = add_layer(&doc, 0).unwrap();
        let Edit::AddLayer {
            active_layer_before,
            active_layer_after,
            ..
        } = edit
        else {
            panic!("expected AddLayer")
        };
        assert_eq!(active_layer_before, 0);
        assert_eq!(
            active_layer_after, 0,
            "the newly inserted layer's own index becomes active, not a shifted old cursor"
        );
    }

    #[test]
    fn add_layer_after_the_active_layer_still_selects_the_new_layer() {
        // Again contrasting with frame_ops's `add_frame_after_active_frame_leaves_it_unchanged`:
        // inserting a frame *after* the active index leaves the frame cursor unchanged, but
        // inserting a layer always moves the cursor to the new layer regardless of where it lands.
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_layer(&doc, 1).unwrap();
        history.apply(&mut doc, edit);
        assert_eq!(
            doc.active_layer(),
            1,
            "unlike add_frame, a layer insert after the active index still selects the new layer"
        );
    }

    #[test]
    fn duplicate_layer_inserts_a_deep_clone_immediately_after_the_source_and_selects_it() {
        let mut doc = Document::new(3, 3);
        doc.set_cell(
            0,
            0,
            0,
            Cell {
                ch: 'D',
                ..Cell::BLANK
            },
        );

        let edit = duplicate_layer(&doc, 0).unwrap();
        let Edit::AddLayer {
            index,
            ref layers,
            ref meta,
            active_layer_after,
            ..
        } = edit
        else {
            panic!("expected AddLayer")
        };
        assert_eq!(index, 1, "the clone lands immediately after the source");
        assert_eq!(
            active_layer_after, 1,
            "the duplicate becomes active, diverging from duplicate_frame's own precedent"
        );
        assert_eq!(
            layers[0].cells()[0].ch,
            'D',
            "the clone carries the source's content"
        );
        assert_eq!(meta.name, "Layer 1 copy");
        assert!(meta.visible);

        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.layer_count(), 2);
        assert_eq!(doc.cell_at(0, 1, 0, 0).unwrap().ch, 'D');
    }

    #[test]
    fn duplicate_layer_carries_the_sources_visibility_into_the_clones_meta() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let hide = set_layer_visibility(&doc, 0, false).unwrap().unwrap();
        history.apply(&mut doc, hide);
        assert!(!doc.layer_visible(0));

        let edit = duplicate_layer(&doc, 0).unwrap();
        let Edit::AddLayer { ref meta, .. } = edit else {
            panic!("expected AddLayer")
        };
        assert!(!meta.visible, "the duplicate must carry the source's current visibility, not always default to visible");
    }

    #[test]
    fn remove_layer_before_active_layer_shifts_it_back_by_one() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_layer(&doc, 0).unwrap();
        history.apply(&mut doc, edit); // 2 layers, active now 0 (the new one)
        let edit = add_layer(&doc, 0).unwrap();
        history.apply(&mut doc, edit); // 3 layers, active now 0 (the newest one)
        assert_eq!(doc.active_layer(), 0);

        // Move active layer aside so it isn't the one being removed.
        assert!(doc.set_active_layer(2));
        let edit = remove_layer(&doc, 0).unwrap();
        let Edit::RemoveLayer {
            active_layer_after, ..
        } = edit
        else {
            panic!("expected RemoveLayer")
        };
        assert_eq!(
            active_layer_after, 1,
            "removing a layer before the active index shifts it back by one"
        );
    }

    #[test]
    fn remove_layer_at_active_layer_clamps_to_the_new_last_valid_index() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_layer(&doc, 1).unwrap();
        history.apply(&mut doc, edit); // 2 layers
        assert!(doc.set_active_layer(0));

        let edit = remove_layer(&doc, 0).unwrap();
        let Edit::RemoveLayer {
            active_layer_after, ..
        } = edit
        else {
            panic!("expected RemoveLayer")
        };
        assert_eq!(
            active_layer_after, 0,
            "removing the active layer clamps to the new last valid index"
        );
    }

    #[test]
    fn remove_layer_refuses_to_remove_a_documents_only_layer() {
        let doc = Document::new(3, 3);
        assert_eq!(remove_layer(&doc, 0), Err(LayerOpError::LastLayer));
    }

    #[test]
    fn reorder_layer_moving_the_active_layer_itself_follows_it_to_the_destination() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_layer(&doc, 1).unwrap();
        history.apply(&mut doc, edit); // 2 layers
        let edit = add_layer(&doc, 2).unwrap();
        history.apply(&mut doc, edit); // 3 layers
        assert!(doc.set_active_layer(0));

        let edit = reorder_layer(&doc, 0, 2).unwrap().unwrap();
        let Edit::ReorderLayer {
            active_layer_after, ..
        } = edit
        else {
            panic!("expected ReorderLayer")
        };
        assert_eq!(
            active_layer_after, 2,
            "moving the active layer itself follows it to the destination"
        );
    }

    #[test]
    fn reorder_layer_from_equals_to_is_a_no_op_with_no_edit() {
        let doc = Document::new(3, 3);
        assert_eq!(reorder_layer(&doc, 0, 0), Ok(None));
    }

    #[test]
    fn set_layer_visibility_to_the_same_value_returns_none() {
        let doc = Document::new(3, 3);
        assert_eq!(
            set_layer_visibility(&doc, 0, true),
            Ok(None),
            "layer 0 already starts visible"
        );
    }

    #[test]
    fn set_layer_visibility_out_of_bounds_is_an_error() {
        let doc = Document::new(3, 3);
        assert_eq!(
            set_layer_visibility(&doc, 5, false),
            Err(LayerOpError::IndexOutOfBounds {
                index: 5,
                layer_count: 1
            })
        );
    }

    #[test]
    fn set_layer_name_to_the_same_value_returns_none() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = set_layer_name(&doc, 0, "Ink".to_string()).unwrap().unwrap();
        history.apply(&mut doc, edit);
        assert_eq!(set_layer_name(&doc, 0, "Ink".to_string()).unwrap(), None);
    }

    #[test]
    fn set_layer_name_out_of_bounds_is_an_error() {
        let doc = Document::new(3, 3);
        assert_eq!(
            set_layer_name(&doc, 5, "x".to_string()),
            Err(LayerOpError::IndexOutOfBounds {
                index: 5,
                layer_count: 1
            })
        );
    }

    /// A rename far past `LayerMeta::MAX_NAME_LEN` must be truncated, not rejected or passed
    /// through verbatim — closes the same unbounded-string surface at the panel-driven rename path
    /// that the format loader closes at load time.
    #[test]
    fn set_layer_name_truncates_a_name_far_past_the_length_cap() {
        let doc = Document::new(3, 3);
        let huge_name = "x".repeat(LayerMeta::MAX_NAME_LEN * 4);
        let Edit::SetLayerName { after, .. } = set_layer_name(&doc, 0, huge_name).unwrap().unwrap()
        else {
            panic!("expected SetLayerName")
        };
        assert_eq!(
            after.chars().count(),
            LayerMeta::MAX_NAME_LEN,
            "the stored name must be clamped to the cap"
        );
    }

    /// Timing-asserted like `frame_ops`'s own "must reject before allocating" tests: rejection must
    /// be prompt, not proportional to the declared layer count.
    #[test]
    fn add_layer_over_max_layers_is_rejected_before_allocating() {
        let mut doc = Document::new(2, 2);
        let mut history = History::new();
        for _ in 0..Document::MAX_LAYERS - 1 {
            let edit = add_layer(&doc, 0).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.layer_count(), Document::MAX_LAYERS);

        let started = std::time::Instant::now();
        let result = add_layer(&doc, 0);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "must reject before allocating, not after"
        );
        assert_eq!(
            result,
            Err(LayerOpError::TooManyLayers {
                found: Document::MAX_LAYERS + 1,
                max: Document::MAX_LAYERS
            })
        );
    }

    /// Trips the joint `MAX_TOTAL_CELLS` budget independent of the per-call `MAX_LAYERS` cap: two
    /// frames, each already carrying 129 layers (well under `MAX_LAYERS`), at the maximal declared
    /// extent — adding one more layer to both frames pushes the total past budget. Every layer here
    /// is built via `Layer::blank(1, 1)` rather than `Layer::blank(MAX_WIDTH, MAX_HEIGHT)`:
    /// `check_caps` (via `total_cells_after_one_more_layer`) only ever reads `doc.width`/`doc.height`
    /// and `Vec<Layer>::len()`, never a layer's own real cell count, so a document with a "declared"
    /// extent that doesn't match its layers' real allocated size is a legitimate way to exercise the
    /// budget arithmetic without allocating gigabytes of real cell data (contrast with
    /// `frame_ops::add_frame_over_the_total_cell_budget_is_rejected`, which can afford to build one
    /// real oversized frame because a frame-structural budget check only ever needs *one* candidate
    /// frame, not a whole already-huge document).
    #[test]
    fn add_layer_over_the_total_cell_budget_is_rejected() {
        let layers_per_frame = 129; // (129*2 + 2) * MAX_WIDTH*MAX_HEIGHT exceeds MAX_TOTAL_CELLS,
                                    // while layer_count()+1 (130) stays well under MAX_LAYERS (256).
        let frames: Vec<Frame> = (0..2)
            .map(|_| Frame {
                layers: (0..layers_per_frame)
                    .map(|_| crate::model::Layer::blank(1, 1))
                    .collect(),
                duration_override: None,
            })
            .collect();
        let doc = Document {
            width: Document::MAX_WIDTH,
            height: Document::MAX_HEIGHT,
            background: Rgba(0, 0, 0, 255),
            frame_duration_ms: Document::DEFAULT_FRAME_DURATION_MS,
            loop_playback: true,
            frames,
            active_frame: 0,
            layer_meta: (0..layers_per_frame)
                .map(LayerMeta::default_named)
                .collect(),
            active_layer: 0,
        };
        assert!(
            doc.layer_count() < Document::MAX_LAYERS,
            "sanity: must stay under the per-call MAX_LAYERS cap"
        );

        let result = add_layer(&doc, 0);
        assert!(matches!(
            result,
            Err(LayerOpError::TotalCellBudgetExceeded { .. })
        ));
    }

    #[test]
    fn add_layer_and_duplicate_layer_reject_a_desynced_document_as_layer_count_desync() {
        let mut doc = Document::new(3, 3);
        doc.layers_mut().push(crate::model::Layer::blank(3, 3)); // bypasses layer_meta on purpose
        assert_eq!(doc.layers().len(), 2);
        assert_eq!(
            doc.layer_count(),
            1,
            "layer_meta was never touched by the direct layers_mut() push"
        );
        assert_eq!(add_layer(&doc, 0), Err(LayerOpError::LayerCountDesync));
        assert_eq!(
            duplicate_layer(&doc, 0),
            Err(LayerOpError::LayerCountDesync)
        );
    }

    #[test]
    fn remove_layer_and_reorder_layer_reject_a_desynced_document_as_layer_count_desync() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = add_layer(&doc, 1).unwrap();
        history.apply(&mut doc, edit); // 2 layers, still in sync
        assert_eq!(doc.layer_count(), 2);

        // Desync: push a third layer directly onto the active frame, bypassing layer_meta.
        doc.layers_mut().push(crate::model::Layer::blank(3, 3));
        assert_eq!(doc.layers().len(), 3);
        assert_eq!(
            doc.layer_count(),
            2,
            "layer_meta was never touched by the direct push"
        );

        assert_eq!(remove_layer(&doc, 0), Err(LayerOpError::LayerCountDesync));
        assert_eq!(
            reorder_layer(&doc, 0, 1),
            Err(LayerOpError::LayerCountDesync)
        );
    }
}
