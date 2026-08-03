//! End-to-end layer-substrate lifecycle, driven only through the public API — mirrors
//! `frame_substrate_integration.rs`'s own shape, generalized from frames to layers: add a layer,
//! draw on it, resize the document, save/load, composite (honoring hidden layers), and confirm the
//! round trip is byte-identical to the in-memory state that produced it.

use gascii_core::{
    add_frame, add_layer, clear_document, composite_cell, composite_frame, duplicate_layer, load_str, reorder_layer,
    resize_document, save_string, set_layer_name, set_layer_visibility, AxisAnchor, BrushShape, Cell, CellEdit,
    CellPatch, DensityMode, Document, Edit, Fixed, Frame, History, LayerOpError, Pencil, PlaneMask, ResizeAnchor,
    ResizeError, Rgba, SelectionTool, Tool, ToolCtx, ToolEvent, ToolResponse,
};

fn cell(ch: char, fg: Rgba, bg: Rgba) -> Cell {
    Cell { ch, fg, bg }
}

fn start() -> ResizeAnchor {
    ResizeAnchor { h: AxisAnchor::Start, v: AxisAnchor::Start }
}

/// A plain, full-mask `ToolCtx` targeting `layer` — the shared shape every layer-op-interaction
/// test below drives a real `Pencil` session through.
fn tctx(layer: usize, glyph: char) -> ToolCtx {
    ToolCtx {
        frame: 0,
        layer,
        glyph,
        fg: Rgba::WHITE,
        bg: Rgba::TRANSPARENT,
        mask: PlaneMask::ALL,
        density: DensityMode::Fixed(Fixed(1.0)),
        ramp: Vec::new(),
        size: 1,
        shape: BrushShape::default(),
    }
}

/// One full press/release `Pencil` gesture at `(x, y)`, returning the committed `Edit`. Panics if
/// nothing commits (every call site below expects a real, non-no-op stroke).
fn press_release(ctx: &ToolCtx, doc: &Document, x: u16, y: u16) -> Edit {
    let mut pencil = Pencil::new();
    pencil.update(ToolEvent::Press { x, y }, ctx, doc);
    let ToolResponse::Commit(Some(edit)) = pencil.update(ToolEvent::Release, ctx, doc) else {
        panic!("expected a committed edit");
    };
    edit
}

#[test]
fn add_draw_hide_save_load_and_composite_round_trip_both_layers_byte_exact() {
    let mut doc = Document::new(4, 4);
    let mut history = History::new();

    // Layer 0: draw a marker.
    doc.set_cell(0, 0, 0, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));

    // Add a second layer (becomes active automatically) and draw on it.
    let edit = add_layer(&doc, 1).unwrap();
    history.apply(&mut doc, edit);
    assert_eq!(doc.active_layer(), 1);
    doc.set_cell(1, 3, 3, cell('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255)));

    // The active-layer cursor is UI/session state, not round-tripped (mirrors active_frame's own
    // contract, and `doc.active_layer = 0` unconditionally on load) — reset it before comparing.
    assert!(doc.set_active_layer(0));
    let before_round_trip = doc.clone();

    let json = save_string(&doc);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(value.get("frames").is_none(), "a single-frame document must still save through the v1 envelope");
    assert!(value.get("layer_meta").is_some(), "the v1 envelope must now carry layer_meta");

    let loaded = load_str(&json).unwrap();
    assert_eq!(loaded, before_round_trip, "the round trip must be byte-exact");
    assert_eq!(loaded.layer_count(), 2);

    // Both layers visible: composite sees layer 1's content on top.
    assert_eq!(composite_cell(&loaded, 0, 3, 3).ch, 'b');

    // Hide layer 1 and confirm compositing (and its whole-sheet wrapper) excludes it. A fresh
    // `History` — `history` above was built against `doc`, a different `Document` instance.
    let hide = set_layer_visibility(&loaded, 1, false).unwrap().unwrap();
    let mut loaded = loaded;
    History::new().apply(&mut loaded, hide);
    assert_eq!(composite_cell(&loaded, 0, 3, 3), Cell::BLANK, "layer 1's marker must be excluded once hidden");
    let sheet = composite_frame(&loaded, 0).unwrap();
    assert_eq!(sheet[0][0].ch, 'a', "layer 0's marker still composites");
    assert_eq!(sheet[3][3], Cell::BLANK);
}

// --- interleaved layer ops + strokes + undo/redo: the LIFO argument made concrete end-to-end ---

/// Draw on layer 0 -> add a layer -> draw on layer 1 -> reorder the two layers -> undo the entire
/// stack back to an empty document, then redo forward -> the document must be byte-identical to a
/// snapshot taken at the matching point in the forward pass, at *every* step, in *both* directions.
/// This is `edit.rs`'s module-doc LIFO safety argument made concrete against a real, mixed-kind
/// history built through the actual tool pipeline (`Pencil`), not hand-built `Edit` literals —
/// including a genuine tracked `active_layer` shift (the reorder moves the layer that was active
/// out from under the cursor, which `frame_ops::shift_for_move` must follow for layers too).
#[test]
fn drawing_via_pencil_on_two_layers_then_reordering_then_undoing_the_full_stack_to_empty_and_redoing_forward_is_identical_at_every_step_both_directions(
) {
    let mut doc = Document::new(4, 4);
    let mut history = History::new();
    let mut forward = vec![doc.clone()]; // depth 0: the empty document, before anything

    let e1 = press_release(&tctx(0, 'a'), &doc, 0, 0);
    history.apply(&mut doc, e1);
    forward.push(doc.clone()); // depth 1: 'a' on layer 0

    let e2 = add_layer(&doc, 1).unwrap();
    history.apply(&mut doc, e2);
    forward.push(doc.clone()); // depth 2: a second, blank layer, now active

    let e3 = press_release(&tctx(1, 'b'), &doc, 3, 3);
    history.apply(&mut doc, e3);
    forward.push(doc.clone()); // depth 3: 'b' on layer 1

    assert_eq!(doc.active_layer(), 1, "sanity: drawing on layer 1 via ctx.layer never moved the cursor further");
    let e4 = reorder_layer(&doc, 0, 1).unwrap().unwrap();
    history.apply(&mut doc, e4);
    forward.push(doc.clone()); // depth 4: layers swapped

    // The reorder both moved content and (since the cursor was sitting at the 'from' index after
    // being swapped in below) moved the active-layer cursor along with it.
    assert_eq!(doc.cell_at(0, 1, 0, 0).unwrap().ch, 'a', "'a' followed the reorder from index 0 to index 1");
    assert_eq!(doc.cell_at(0, 0, 3, 3).unwrap().ch, 'b', "'b' followed the reorder from index 1 to index 0");

    assert_eq!(forward.len(), 5);

    // Undo the full stack back to empty, comparing the *entire* Document (extent, every layer's
    // content, and the active_layer cursor) against the matching forward snapshot at each depth.
    for depth in (0..forward.len() - 1).rev() {
        assert!(history.undo(&mut doc), "undo must succeed at depth {depth}");
        assert_eq!(doc, forward[depth], "undo landing at depth {depth} must match the forward snapshot exactly");
    }
    assert!(!history.can_undo());
    assert_eq!(doc, Document::new(4, 4));

    // Redo forward, comparing against the same snapshots in the opposite direction.
    for (depth, snapshot) in forward.iter().enumerate().skip(1) {
        assert!(history.redo(&mut doc), "redo must succeed at depth {depth}");
        assert_eq!(&doc, snapshot, "redo landing at depth {depth} must match the forward snapshot exactly");
    }
    assert!(!history.can_redo());
}

/// A layer-structural edit (`ReorderLayer`) committed by another path while a stroke is still
/// pending, mid-drag. The stroke's own `resync` must re-pin *and recompose* against the
/// post-reorder content, or a masked-off plane silently commits the wrong layer's stale content —
/// the same "stale pending tool state" corruption class `frame_substrate_integration.rs` pins for
/// frames, now with layers as the vector. Uses a partial mask deliberately: with a full mask
/// `CellEdit.before` is always read fresh at `finish` time regardless of `resync`, so only a
/// masked-off plane can actually expose this gap.
#[test]
fn a_layer_reorder_landing_mid_stroke_must_be_resynced_or_a_masked_off_plane_commits_the_wrong_layers_content() {
    let color_p = Rgba(10, 10, 10, 255);
    let color_q = Rgba(50, 50, 50, 255);

    let mut doc = Document::new(4, 4);
    let mut history = History::new();
    let add = add_layer(&doc, 1).unwrap();
    history.apply(&mut doc, add);

    // Distinguishing bg at the same coordinate on each layer.
    doc.set_cell(0, 2, 2, Cell { ch: 'o', fg: Rgba::WHITE, bg: color_p }); // layer 0
    doc.set_cell(1, 2, 2, Cell { ch: 'o', fg: Rgba::WHITE, bg: color_q }); // layer 1

    // A stroke targeting layer index 1, bg masked off: it must preserve whatever bg is actually
    // there, never overwrite it.
    let mask = PlaneMask { glyph: true, bg: false };
    let stroke_ctx = ToolCtx { frame: 0, layer: 1, glyph: 'X', fg: Rgba::WHITE, bg: Rgba(200, 200, 200, 255), mask, ..tctx(1, 'X') };
    let mut stroke = Pencil::new();
    stroke.update(ToolEvent::Press { x: 2, y: 2 }, &stroke_ctx, &doc);
    // Internally pins before = layer index 1's pre-reorder cell (bg = color_q).

    // Another path (a future "move layer" UI action, or another binding's flush) commits a
    // reorder while this stroke is still pending.
    let reorder = reorder_layer(&doc, 0, 1).unwrap().unwrap();
    history.apply(&mut doc, reorder);
    assert_eq!(
        doc.cell_at(0, 1, 2, 2).unwrap().bg,
        color_p,
        "sanity: the reorder moved color_p into the stroke's target position"
    );

    // The tool must be resynced against the mutation that landed underneath it.
    stroke.resync(&doc, 0, 1);

    let ToolResponse::Commit(Some(finish_edit)) = stroke.update(ToolEvent::Release, &stroke_ctx, &doc) else {
        panic!("expected a committed edit");
    };
    history.apply(&mut doc, finish_edit);

    let committed = doc.cell_at(0, 1, 2, 2).unwrap();
    assert_eq!(committed.ch, 'X', "the unmasked glyph plane must carry the stroke's own write");
    assert_eq!(
        committed.bg, color_p,
        "the masked-off bg plane must preserve layer 1's post-reorder content (color_p), not the \
         stroke's stale pre-reorder pin (color_q) — proves resync recomposed, not just re-pinned"
    );
}

/// `duplicate_layer`'s clone must carry the source's current visibility (not always default to
/// visible), and a `SetLayerName` applied at an index that only exists *after* an interleaved
/// `reorder_layer` must undo back to the exact prior state at every step, in strictly-LIFO order —
/// the same argument `drawing_via_pencil_on_two_layers_then_reordering_...` makes for cell content,
/// applied here to `LayerMeta` and a duplicated layer instead.
#[test]
fn duplicate_layer_carries_visibility_and_a_set_layer_name_survives_an_interleaved_reorder_and_undo() {
    let mut doc = Document::new(3, 3);
    let mut history = History::new();
    let mut forward = vec![doc.clone()]; // depth 0

    // A second layer, so there's something to duplicate and reorder.
    let e1 = add_layer(&doc, 1).unwrap();
    history.apply(&mut doc, e1);
    forward.push(doc.clone()); // depth 1

    // Layer 1 gets hidden.
    let e2 = set_layer_visibility(&doc, 1, false).unwrap().unwrap();
    history.apply(&mut doc, e2);
    forward.push(doc.clone()); // depth 2
    assert!(!doc.layer_visible(1));

    // Duplicate layer 1: the clone (landing at index 2) must carry the hidden visibility forward,
    // not reset it to visible.
    let e3 = duplicate_layer(&doc, 1).unwrap();
    history.apply(&mut doc, e3);
    forward.push(doc.clone()); // depth 3
    assert_eq!(doc.layer_count(), 3);
    assert!(!doc.layer_visible(2), "duplicate_layer must carry the source's visibility into the clone");

    // Reorder: move the duplicate (index 2) to the front. Layer indices addressing everything else
    // shift underneath whatever a *later* SetLayerName targets.
    let e4 = reorder_layer(&doc, 2, 0).unwrap().unwrap();
    history.apply(&mut doc, e4);
    forward.push(doc.clone()); // depth 4
    assert!(!doc.layer_visible(0), "the duplicate's hidden visibility followed it to index 0");

    // A SetLayerName on the now-relocated duplicate, addressed by its *post-reorder* index.
    let e5 = set_layer_name(&doc, 0, "Shading copy".to_string()).unwrap().unwrap();
    history.apply(&mut doc, e5);
    forward.push(doc.clone()); // depth 5
    assert_eq!(doc.layer_name(0), Some("Shading copy"));

    assert_eq!(forward.len(), 6);

    // Undo the full stack back to empty, comparing the entire Document at each depth.
    for depth in (0..forward.len() - 1).rev() {
        assert!(history.undo(&mut doc), "undo must succeed at depth {depth}");
        assert_eq!(doc, forward[depth], "undo landing at depth {depth} must match the forward snapshot exactly");
    }
    assert!(!history.can_undo());
    assert_eq!(doc, Document::new(3, 3));

    // Redo forward, comparing against the same snapshots in the opposite direction.
    for (depth, snapshot) in forward.iter().enumerate().skip(1) {
        assert!(history.redo(&mut doc), "redo must succeed at depth {depth}");
        assert_eq!(&doc, snapshot, "redo landing at depth {depth} must match the forward snapshot exactly");
    }
    assert!(!history.can_redo());
}

// --- persistence round trips, including a hand-authored pre-layers fixture ---

/// A three-layer single-frame document with distinct per-layer content, names, and visibility must
/// round-trip through save/load byte-exact — every layer-metadata field at once.
#[test]
fn a_three_layer_document_with_distinct_content_names_and_visibility_round_trips_byte_exact() {
    let mut doc = Document::new(5, 4);
    let mut history = History::new();

    doc.set_cell(0, 0, 0, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));

    let e1 = add_layer(&doc, 1).unwrap();
    history.apply(&mut doc, e1);
    doc.set_cell(1, 1, 1, cell('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255)));

    let e2 = add_layer(&doc, 2).unwrap();
    history.apply(&mut doc, e2);
    doc.set_cell(2, 2, 2, cell('c', Rgba(7, 8, 9, 255), Rgba(10, 11, 12, 255)));

    let n1 = set_layer_name(&doc, 1, "Ink".to_string()).unwrap().unwrap();
    history.apply(&mut doc, n1);
    let v1 = set_layer_visibility(&doc, 2, false).unwrap().unwrap();
    history.apply(&mut doc, v1);

    assert_eq!(doc.layer_count(), 3);
    // The active-layer cursor is UI/session state, not round-tripped — reset it before comparing.
    assert!(doc.set_active_layer(0));
    let before_round_trip = doc.clone();

    let json = save_string(&doc);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["version"], 1, "a single-frame document must still be tagged version 1");
    assert_eq!(value["layer_meta"].as_array().unwrap().len(), 3);

    let loaded = load_str(&json).unwrap();
    assert_eq!(loaded, before_round_trip, "the full 3-layer document must round-trip byte-exact");
    assert_eq!(loaded.layer_name(1), Some("Ink"));
    assert!(!loaded.layer_visible(2));
    assert!(loaded.layer_visible(0));
    assert!(loaded.layer_visible(1));
    assert_eq!(loaded.cell_at(0, 0, 0, 0).unwrap().ch, 'a');
    assert_eq!(loaded.cell_at(0, 1, 1, 1).unwrap().ch, 'b');
    assert_eq!(loaded.cell_at(0, 2, 2, 2).unwrap().ch, 'c');
}

/// A literal, hand-authored v1 JSON string — exactly the shape a pre-layers build would have
/// written, with no `layer_meta` key anywhere — must load with byte-identical cell content and the
/// documented default per-layer metadata (mirrors `frame_substrate_integration.rs`'s own
/// `a_hand_authored_pre_frames_v1_fixture_...`, generalized to layers).
#[test]
fn a_hand_authored_pre_layers_v1_fixture_loads_with_byte_identical_cells_and_default_layer_metadata() {
    let json = r##"{
        "version": 1,
        "width": 2,
        "height": 2,
        "background": "#0000FFFF",
        "layers": [{
            "glyphs": ["a ", " z"],
            "fg": [[[1, "#FFFFFFFF"], [1, "#FFFFFFFF"]], [[1, "#FFFFFFFF"], [1, "#FFFFFFFF"]]],
            "bg": [[[2, "#00000000"]], [[2, "#00000000"]]]
        }]
    }"##;
    let doc = load_str(json).unwrap();
    assert_eq!(doc.layer_count(), 1);
    assert_eq!(doc.active_layer(), 0);
    assert_eq!(doc.layer_name(0), Some("Layer 1"), "no layer_meta in the file — must fall back to the documented default name");
    assert!(doc.layer_visible(0), "no layer_meta in the file — must fall back to the documented default (visible)");
    assert_eq!(doc.cell(0, 0, 0).unwrap().ch, 'a');
    assert_eq!(doc.cell(0, 1, 0).unwrap().ch, ' ');
    assert_eq!(doc.cell(0, 0, 1).unwrap().ch, ' ');
    assert_eq!(doc.cell(0, 1, 1).unwrap().ch, 'z');
}

// --- resize/clear composed with layers and undo ---

/// Extends `resize.rs`'s own `multi_layer_document_resizes_every_layer_consistently` (only 2
/// layers, built via the `layers_mut()` escape hatch) with the undo half, at `layer_count() == 3`,
/// built entirely through legitimate `add_layer` calls, plus a save/load round trip of both the
/// shrunken and the undo-restored state.
#[test]
fn resizing_a_three_layer_document_then_undoing_restores_every_layers_cropped_content_exactly_and_both_states_round_trip_through_save_load(
) {
    let mut doc = Document::new(5, 5);
    let mut history = History::new();

    for (x, y, ch) in [(0, 0, 'a'), (4, 0, 'b'), (0, 4, 'c'), (4, 4, 'd')] {
        doc.set_cell(0, x, y, cell(ch, Rgba::WHITE, Rgba::TRANSPARENT));
    }

    let e1 = add_layer(&doc, 1).unwrap();
    history.apply(&mut doc, e1);
    doc.set_cell(1, 4, 4, cell('e', Rgba(1, 2, 3, 255), Rgba::TRANSPARENT));

    let e2 = add_layer(&doc, 2).unwrap();
    history.apply(&mut doc, e2);
    doc.set_cell(2, 4, 4, cell('f', Rgba(4, 5, 6, 255), Rgba::TRANSPARENT));

    assert_eq!(doc.layer_count(), 3);
    // The active-layer cursor is UI/session state, not round-tripped, and never touched by resize
    // or undo/redo below — reset it once, up front, so every later load-comparison stays valid.
    assert!(doc.set_active_layer(0));
    let before_resize = doc.clone();

    // Shrink to 2x2 (top-left anchored): only the top-left corner survives directly; layer 1 and
    // layer 2's only content each sat in the cropped-away bottom-right corner.
    let resize_edit = resize_document(&doc, 2, 2, start()).unwrap().unwrap();
    history.apply(&mut doc, resize_edit);
    assert_eq!((doc.width, doc.height), (2, 2));
    assert_eq!(doc.cell(0, 0, 0).unwrap().ch, 'a', "layer 0's top-left survives the crop");
    assert_eq!(doc.cell(1, 0, 0), Some(&Cell::BLANK), "layer 1's only content ('e') was cropped away");
    assert_eq!(doc.cell(2, 0, 0), Some(&Cell::BLANK), "layer 2's only content ('f') was cropped away");

    // Undo restores every layer's cropped-away content exactly, not just one layer's.
    assert!(history.undo(&mut doc));
    assert_eq!(doc, before_resize, "undo must restore all three layers' cropped content byte-exact");
    assert_eq!(doc.cell(1, 4, 4).unwrap().ch, 'e');
    assert_eq!(doc.cell(2, 4, 4).unwrap().ch, 'f');

    // Both the shrunken and the undo-restored states round-trip through save/load.
    assert!(history.redo(&mut doc));
    let shrunk = doc.clone();
    assert_eq!(load_str(&save_string(&doc)).unwrap(), shrunk);

    assert!(history.undo(&mut doc));
    assert_eq!(load_str(&save_string(&doc)).unwrap(), before_resize);
}

/// Extends `clear.rs`'s own `clear_document_blanks_every_nonblank_cell_across_all_layers` (2
/// layers via `layers_mut()`) with `layer_count() == 3` built through legitimate `add_layer` calls,
/// plus a save/load round trip of the post-clear state.
#[test]
fn clearing_the_active_frame_blanks_all_three_layers_through_a_save_load_round_trip() {
    let mut doc = Document::new(3, 3);
    let mut history = History::new();
    let e1 = add_layer(&doc, 1).unwrap();
    history.apply(&mut doc, e1);
    let e2 = add_layer(&doc, 2).unwrap();
    history.apply(&mut doc, e2);
    assert_eq!(doc.layer_count(), 3);
    // The active-layer cursor is UI/session state, not round-tripped — reset it before the
    // eventual load-comparison below (clear itself never touches it).
    assert!(doc.set_active_layer(0));

    doc.set_cell(0, 0, 0, cell('x', Rgba::WHITE, Rgba::TRANSPARENT));
    doc.set_cell(1, 1, 1, cell('y', Rgba::WHITE, Rgba::TRANSPARENT));
    doc.set_cell(2, 2, 2, cell('z', Rgba::WHITE, Rgba::TRANSPARENT));

    let clear_edit = clear_document(&doc).unwrap();
    history.apply(&mut doc, clear_edit);
    for layer in 0..3 {
        assert!(doc.layers()[layer].cells().iter().all(Cell::is_blank), "layer {layer} must be blanked");
    }

    let loaded = load_str(&save_string(&doc)).unwrap();
    assert_eq!(loaded, doc);
    for layer in 0..3 {
        assert!(loaded.layers()[layer].cells().iter().all(Cell::is_blank));
    }
}

// --- selection lift/move/paste interaction: layer-scoped, active layer only ---

/// A Selection lift-move-drop (the tool's own internal "copy and move" gesture) and an external
/// paste (`accept_stamp`, the same path an OS-clipboard paste lands through) driven against a
/// non-default active layer must touch only that layer — every other layer stays byte-identical
/// throughout, forward and after undo.
#[test]
fn selection_lift_move_drop_and_accept_stamp_paste_on_layer_1_leave_layer_0_byte_identical() {
    let mut doc = Document::new(6, 6);
    let mut history = History::new();
    let add = add_layer(&doc, 1).unwrap();
    history.apply(&mut doc, add);

    // Distinct content on each layer at the region the move will touch.
    doc.set_cell(0, 1, 1, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));
    doc.set_cell(1, 1, 1, cell('b', Rgba(9, 9, 9, 255), Rgba::TRANSPARENT));
    let layer0_before = doc.layers()[0].clone();

    let mut sel = SelectionTool::new();
    let ctx1 = tctx(1, '#');

    // Lift-move-drop on layer 1.
    sel.update(ToolEvent::Press { x: 1, y: 1 }, &ctx1, &doc);
    sel.update(ToolEvent::Drag { x: 1, y: 1 }, &ctx1, &doc);
    sel.update(ToolEvent::Release, &ctx1, &doc);
    sel.update(ToolEvent::Press { x: 1, y: 1 }, &ctx1, &doc); // lift
    sel.update(ToolEvent::Drag { x: 4, y: 4 }, &ctx1, &doc); // move
    let ToolResponse::Commit(Some(move_edit)) = sel.update(ToolEvent::Commit, &ctx1, &doc) else {
        panic!("expected a committed move edit");
    };
    history.apply(&mut doc, move_edit);

    assert_eq!(doc.cell_at(0, 1, 4, 4).unwrap().ch, 'b', "the moved content landed on layer 1");
    assert_eq!(doc.layers()[0], layer0_before, "layer 0 must be untouched by a layer-1 move");

    // An external paste (accept_stamp) onto layer 1.
    let patch = CellPatch { width: 1, height: 1, cells: vec![cell('c', Rgba(3, 3, 3, 255), Rgba::TRANSPARENT)] };
    sel.accept_stamp(patch, (0, 0), &doc);
    let ToolResponse::Commit(Some(paste_edit)) = sel.update(ToolEvent::Commit, &ctx1, &doc) else {
        panic!("expected a committed paste edit");
    };
    history.apply(&mut doc, paste_edit);

    assert_eq!(doc.cell_at(0, 1, 0, 0).unwrap().ch, 'c', "the pasted content landed on layer 1");
    assert_eq!(doc.layers()[0], layer0_before, "layer 0 must still be untouched after the paste");

    // Undo both edits: layer 0 was never touched at any point, and undo restores layer 1 exactly.
    assert!(history.undo(&mut doc));
    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell_at(0, 1, 1, 1).unwrap().ch, 'b', "undo must restore layer 1's original content");
    assert_eq!(doc.layers()[0], layer0_before, "layer 0 must remain untouched through both undos");
}

// --- cap enforcement composed across seams, via legitimate ops ---

/// Builds a document up to exactly `MAX_LAYERS` through legitimate, individually-applied
/// `add_layer` calls (not a single oversized construction), then pushes one more — rejected — and
/// confirms the document is left completely unmodified and still fully usable (an ordinary cell
/// edit and its undo still work), not just that the call returned `Err`.
#[test]
fn add_layer_rejection_after_legitimately_building_up_to_max_layers_leaves_the_document_and_history_fully_usable() {
    let mut doc = Document::new(2, 2);
    let mut history = History::new();
    for _ in 0..Document::MAX_LAYERS - 1 {
        let edit = add_layer(&doc, 0).unwrap();
        history.apply(&mut doc, edit);
    }
    assert_eq!(doc.layer_count(), Document::MAX_LAYERS);
    let before_rejection = doc.clone();

    let result = add_layer(&doc, 0);
    assert!(matches!(result, Err(LayerOpError::TooManyLayers { .. })));
    assert_eq!(doc, before_rejection, "a rejected add_layer call must leave the document completely unmodified");

    // Still fully usable afterward: an ordinary cell edit and its undo work cleanly.
    let cell_edit = Edit::Cells(vec![CellEdit { frame: 0, layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('z', Rgba::WHITE, Rgba::TRANSPARENT) }]);
    history.apply(&mut doc, cell_edit);
    assert_eq!(doc.cell(0, 0, 0).unwrap().ch, 'z');
    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK));
}

/// Composes `layer_ops::add_layer`'s own per-call cap checks (exercised at a tiny, cheap extent)
/// with `resize_document`'s independent joint-budget check: a two-frame document (via
/// `frame_ops::add_frame`) with 129 layers per frame — each individually built through the *real*
/// `add_layer` API, cheap at a tiny extent — then a resize toward the max extent must still be
/// caught by the joint budget, proving the three independently-reviewed seams (`frame_ops`,
/// `layer_ops`, `resize`) compose correctly rather than one silently trusting the other.
#[test]
fn layers_built_up_through_legitimate_add_layer_calls_across_two_frames_still_trip_resize_documents_joint_budget_check(
) {
    let mut doc = Document::new(2, 2);
    let mut history = History::new();
    let add_frame_edit = add_frame(&doc, 1, Frame::blank(2, 2)).unwrap();
    history.apply(&mut doc, add_frame_edit);
    assert_eq!(doc.frame_count(), 2);

    for _ in 0..129 {
        let edit = add_layer(&doc, 0).unwrap();
        history.apply(&mut doc, edit);
    }
    assert_eq!(doc.layer_count(), 130, "1 original + 129 legitimately added");
    let before_resize_attempt = doc.clone();

    let started = std::time::Instant::now();
    let result = resize_document(&doc, Document::MAX_WIDTH, Document::MAX_HEIGHT, start());
    assert!(started.elapsed() < std::time::Duration::from_millis(200), "must reject before allocating, not after");
    assert!(matches!(result, Err(ResizeError::TotalCellBudgetExceeded { .. })));
    assert_eq!(doc, before_resize_attempt, "a rejected resize must leave the document completely unmodified");
}
