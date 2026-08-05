//! End-to-end frame-substrate lifecycle, driven only through the public API — the shape a Phase 4
//! consumer (frame-switching UI, onion-skinning) will actually exercise: add a frame, draw on it,
//! resize the document, save as v2, load the v2 file back, composite both frames, and confirm the
//! round trip is byte-identical to the in-memory state that produced it.

use gascii_core::{
    add_frame, clear_document, composite_frame, duplicate_frame, load_str, reorder_frame, resize_document,
    save_string, set_frame_duration, AxisAnchor, BrushShape, Cell, CellEdit, DensityMode, Document, Edit, Fixed,
    Frame, FrameOpError, History, Layer, Pencil, PlaneMask, ResizeAnchor, ResizeError, Rgba, Tool, ToolCtx,
    ToolEvent, ToolResponse,
};

fn cell(ch: char, fg: Rgba, bg: Rgba) -> Cell {
    Cell { ch, fg, bg }
}

fn start() -> ResizeAnchor {
    ResizeAnchor { h: AxisAnchor::Start, v: AxisAnchor::Start }
}

/// A plain, full-mask `ToolCtx` targeting `frame` and stamping `glyph` in white-on-transparent —
/// the shared shape every frame-op-interaction test below drives a real `Pencil` session through.
fn tctx(frame: usize, glyph: char) -> ToolCtx {
    ToolCtx {
        frame,
        layer: 0,
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
fn add_draw_resize_save_load_and_composite_round_trip_both_frames_byte_exact() {
    let mut doc = Document::new(4, 4);
    let mut history = History::new();

    // Frame 0: draw a marker.
    doc.set_cell(0, 0, 0, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));

    // Add a second frame and draw on it.
    let edit = add_frame(&doc, 1, Frame::blank(4, 4)).unwrap();
    history.apply(&mut doc, edit);
    assert!(doc.set_active_frame(1));
    doc.set_cell(0, 3, 3, cell('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255)));
    assert!(doc.set_active_frame(0));

    // Resize the whole document (extent is document-wide — both frames grow together).
    let resize_edit = resize_document(&doc, 6, 6, start()).unwrap().expect("a real extent change yields an edit");
    assert!(matches!(resize_edit, Edit::Resize { .. }));
    history.apply(&mut doc, resize_edit);
    assert_eq!(doc.width, 6);
    assert_eq!(doc.height, 6);

    let before_round_trip = doc.clone();

    // Save (must be v2 — frame_count() == 2), load back, and confirm a byte-exact Document.
    let json = save_string(&doc);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(value.get("frames").is_some(), "a 2-frame document must save through the v2 envelope");

    let loaded = load_str(&json).unwrap();
    assert_eq!(loaded, before_round_trip, "the round trip must be byte-exact");
    assert_eq!(loaded.frame_count(), 2);

    // Composite both frames explicitly and confirm each still carries its own marker post-round-trip.
    let composite0 = composite_frame(&loaded, 0).unwrap();
    let composite1 = composite_frame(&loaded, 1).unwrap();
    assert_eq!(composite0[0][0].ch, 'a', "frame 0's marker survives resize + save/load");
    assert_eq!(composite1[3][3].ch, 'b', "frame 1's marker survives resize + save/load, at its original (unresized-shifted) coordinates");
    // Newly padded region (from the top-left-anchored grow) stays Blank in both frames.
    assert_eq!(composite0[5][5], Cell::BLANK);
    assert_eq!(composite1[5][5], Cell::BLANK);
}

// --- interleaved frame ops + strokes + undo/redo: the LIFO argument made concrete end-to-end ---

/// Draw on frame 0 -> add a frame -> draw on frame 1 -> reorder the two frames -> undo the entire
/// stack back to an empty document, then redo forward -> the document must be byte-identical to a
/// snapshot taken at the matching point in the forward pass, at *every* step, in *both*
/// directions. This is the module doc's LIFO safety argument (`edit.rs`) made concrete against a
/// real, mixed-kind history built through the actual tool pipeline (`Pencil`), not hand-built
/// `Edit` literals — including a genuine tracked `active_frame` shift (the reorder moves the frame
/// that was active out from under the cursor, which `frame_ops::shift_for_move` must follow).
#[test]
fn drawing_via_pencil_on_two_frames_then_reordering_then_undoing_the_full_stack_to_empty_and_redoing_forward_is_identical_at_every_step_both_directions(
) {
    let mut doc = Document::new(4, 4);
    let mut history = History::new();
    let mut forward = vec![doc.clone()]; // depth 0: the empty document, before anything

    let e1 = press_release(&tctx(0, 'a'), &doc, 0, 0);
    history.apply(&mut doc, e1);
    forward.push(doc.clone()); // depth 1: 'a' on frame 0

    let e2 = add_frame(&doc, 1, Frame::blank(4, 4)).unwrap();
    history.apply(&mut doc, e2);
    forward.push(doc.clone()); // depth 2: a second, blank frame — now the active one

    // Frame 1 is drawn on via ctx.frame explicitly — CellEdit addressing never depends on the
    // document's active-frame cursor (that independence is pinned per-tool in the ctx-frame unit
    // tests; here the cursor happens to sit on 1 because AddFrame selected the frame it inserted).
    let e3 = press_release(&tctx(1, 'b'), &doc, 3, 3);
    history.apply(&mut doc, e3);
    forward.push(doc.clone()); // depth 3: 'b' on frame 1

    assert_eq!(doc.active_frame(), 1, "sanity: the cursor sits on the frame AddFrame inserted");
    let e4 = reorder_frame(&doc, 0, 1).unwrap().unwrap();
    history.apply(&mut doc, e4);
    forward.push(doc.clone()); // depth 4: frames swapped

    // The reorder both moved content and shifted the active-frame cursor along with its frame —
    // a genuine tracked side effect, not a no-op.
    assert_eq!(doc.cell_at(1, 0, 0, 0).unwrap().ch, 'a', "'a' followed the reorder from index 0 to index 1");
    assert_eq!(doc.cell_at(0, 0, 3, 3).unwrap().ch, 'b', "'b' followed the reorder from index 1 to index 0");
    assert_eq!(doc.active_frame(), 0, "the cursor's frame slid from index 1 to 0 and the cursor followed it");

    assert_eq!(forward.len(), 5);

    // Undo the full stack back to empty, comparing the *entire* Document (extent, every frame's
    // content, and the active_frame cursor) against the matching forward snapshot at each depth.
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

/// A frame-structural edit (`ReorderFrame`) committed by another path while a stroke is still
/// pending, mid-drag. The stroke's own `resync` must re-pin *and recompose* against the
/// post-reorder content, or a masked-off plane silently commits the wrong frame's stale content —
/// this codebase's own named "stale pending tool state" corruption class
/// (`edit.rs::apply_and_undo_do_not_validate_before_...`), now with frames as the vector. Uses a
/// partial mask deliberately: with a full mask `CellEdit.before` is always read fresh at `finish`
/// time regardless of `resync`, so only a masked-off plane can actually expose this gap (see
/// `FreehandStroke::resync`'s own doc comment on why re-pinning alone is not enough).
#[test]
fn a_frame_reorder_landing_mid_stroke_must_be_resynced_or_a_masked_off_plane_commits_the_wrong_frames_content() {
    let color_p = Rgba(10, 10, 10, 255);
    let color_q = Rgba(50, 50, 50, 255);

    let mut doc = Document::new(4, 4);
    let mut history = History::new();
    let add = add_frame(&doc, 1, Frame::blank(4, 4)).unwrap();
    history.apply(&mut doc, add); // the new frame becomes active
    assert!(doc.set_active_frame(0));

    // Distinguishing bg at the same coordinate on each frame (the setup content itself isn't what
    // is under test, so it's written directly rather than through a tool). `set_cell_at` is
    // `pub(crate)`-only, so this integration crate switches the active frame instead, mirroring
    // every other integration test's convention.
    doc.set_cell(0, 2, 2, Cell { ch: 'o', fg: Rgba::WHITE, bg: color_p }); // frame 0
    assert!(doc.set_active_frame(1));
    doc.set_cell(0, 2, 2, Cell { ch: 'o', fg: Rgba::WHITE, bg: color_q });
    assert!(doc.set_active_frame(0));

    // A stroke targeting frame index 1, bg masked off: it must preserve whatever bg is actually
    // there, never overwrite it.
    let mask = PlaneMask { glyph: true, bg: false };
    let stroke_ctx = ToolCtx { frame: 1, layer: 0, glyph: 'X', fg: Rgba::WHITE, bg: Rgba(200, 200, 200, 255), mask, ..tctx(1, 'X') };
    let mut stroke = Pencil::new();
    stroke.update(ToolEvent::Press { x: 2, y: 2 }, &stroke_ctx, &doc);
    // Internally pins before = frame index 1's pre-reorder cell (bg = color_q).

    // Another path (a future "move frame" UI action, or another binding's flush) commits a
    // reorder while this stroke is still pending.
    let reorder = reorder_frame(&doc, 0, 1).unwrap().unwrap();
    history.apply(&mut doc, reorder);
    assert_eq!(
        doc.cell_at(1, 0, 2, 2).unwrap().bg,
        color_p,
        "sanity: the reorder moved color_p into the stroke's target position"
    );

    // The tool must be resynced against the mutation that landed underneath it — mirrors every
    // other resync call site's contract (`Tool::resync`'s own doc comment).
    stroke.resync(&doc, 1, 0);

    let ToolResponse::Commit(Some(finish_edit)) = stroke.update(ToolEvent::Release, &stroke_ctx, &doc) else {
        panic!("expected a committed edit");
    };
    history.apply(&mut doc, finish_edit);

    let committed = doc.cell_at(1, 0, 2, 2).unwrap();
    assert_eq!(committed.ch, 'X', "the unmasked glyph plane must carry the stroke's own write");
    assert_eq!(
        committed.bg, color_p,
        "the masked-off bg plane must preserve frame 1's post-reorder content (color_p), not the \
         stroke's stale pre-reorder pin (color_q) — proves resync recomposed, not just re-pinned"
    );
}

/// `duplicate_frame`'s clone must carry the source's `duration_override` (not reset it), and a
/// `SetFrameDuration` applied at an index that only exists *after* an interleaved `reorder_frame`
/// must undo back to the exact prior state at every step, in strictly-LIFO order — the same
/// argument `drawing_via_pencil_on_two_frames_then_reordering_...` makes for cell content, applied
/// here to `duration_override` and a duplicated frame instead.
#[test]
fn duplicate_frame_carries_duration_override_and_a_set_frame_duration_survives_an_interleaved_reorder_and_undo() {
    let mut doc = Document::new(3, 3);
    let mut history = History::new();
    let mut forward = vec![doc.clone()]; // depth 0

    // A second frame, so there's something to duplicate and reorder.
    let e1 = add_frame(&doc, 1, Frame::blank(3, 3)).unwrap();
    history.apply(&mut doc, e1);
    forward.push(doc.clone()); // depth 1

    // Frame 1 gets its own duration override.
    let e2 = set_frame_duration(&doc, 1, Some(50)).unwrap().unwrap();
    history.apply(&mut doc, e2);
    forward.push(doc.clone()); // depth 2
    assert_eq!(doc.frame(1).unwrap().duration_override, Some(50));

    // Duplicate frame 1: the clone (landing at index 2) must carry the override forward, not reset
    // it to the document default.
    let e3 = duplicate_frame(&doc, 1).unwrap();
    history.apply(&mut doc, e3);
    forward.push(doc.clone()); // depth 3
    assert_eq!(doc.frame_count(), 3);
    assert_eq!(doc.frame(2).unwrap().duration_override, Some(50), "duplicate_frame must carry duration_override into the clone");

    // Reorder: move the duplicate (index 2) to the front. Frame indices addressing everything else
    // shift underneath whatever a *later* SetFrameDuration targets.
    let e4 = reorder_frame(&doc, 2, 0).unwrap().unwrap();
    history.apply(&mut doc, e4);
    forward.push(doc.clone()); // depth 4
    assert_eq!(doc.frame(0).unwrap().duration_override, Some(50), "the duplicate's override followed it to index 0");

    // A SetFrameDuration on the now-relocated duplicate, addressed by its *post-reorder* index —
    // exactly the positional-addressing argument `edit.rs`'s module doc makes, now exercised with
    // duration overrides instead of cells.
    let e5 = set_frame_duration(&doc, 0, Some(999)).unwrap().unwrap();
    history.apply(&mut doc, e5);
    forward.push(doc.clone()); // depth 5
    assert_eq!(doc.frame(0).unwrap().duration_override, Some(999));

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

// --- persistence round trips across the version boundary ---

/// A three-frame document with distinct per-frame content, per-frame duration overrides, a
/// document-level duration default, disabled looping, and a custom background — every v2-only
/// field at once — must round-trip through save/load byte-exact.
#[test]
fn a_three_frame_document_with_distinct_content_durations_and_a_custom_background_round_trips_through_v2_byte_exact(
) {
    let mut doc = Document::new(5, 4);
    doc.background = Rgba(11, 22, 33, 255);
    doc.frame_duration_ms = 80;
    doc.loop_playback = false;
    let mut history = History::new();

    doc.set_cell(0, 0, 0, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));

    let e1 = add_frame(&doc, 1, Frame::blank(5, 4)).unwrap();
    history.apply(&mut doc, e1);
    assert!(doc.set_active_frame(1));
    doc.set_cell(0, 1, 1, cell('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255)));
    assert!(doc.set_active_frame(0));

    let e2 = add_frame(&doc, 2, Frame::blank(5, 4)).unwrap();
    history.apply(&mut doc, e2);
    assert!(doc.set_active_frame(2));
    doc.set_cell(0, 2, 2, cell('c', Rgba(7, 8, 9, 255), Rgba(10, 11, 12, 255)));
    assert!(doc.set_active_frame(0));

    let d1 = set_frame_duration(&doc, 1, Some(30)).unwrap().unwrap();
    history.apply(&mut doc, d1);
    let d2 = set_frame_duration(&doc, 2, Some(500)).unwrap().unwrap();
    history.apply(&mut doc, d2);

    assert_eq!(doc.frame_count(), 3);
    let before_round_trip = doc.clone();

    let json = save_string(&doc);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["version"], 2);
    assert_eq!(value["frames"].as_array().unwrap().len(), 3);

    let loaded = load_str(&json).unwrap();
    assert_eq!(loaded, before_round_trip, "the full 3-frame document must round-trip byte-exact");
    assert_eq!(loaded.background, Rgba(11, 22, 33, 255));
    assert_eq!(loaded.frame_duration_ms, 80);
    assert!(!loaded.loop_playback);
    assert_eq!(loaded.resolved_frame_duration_ms(0), Some(80), "frame 0 has no override, falls back to the default");
    assert_eq!(loaded.resolved_frame_duration_ms(1), Some(30));
    assert_eq!(loaded.resolved_frame_duration_ms(2), Some(500));
    assert_eq!(loaded.cell_at(0, 0, 0, 0).unwrap().ch, 'a');
    assert_eq!(loaded.cell_at(1, 0, 1, 1).unwrap().ch, 'b');
    assert_eq!(loaded.cell_at(2, 0, 2, 2).unwrap().ch, 'c');
}

/// Stronger than the coder's own `single_frame_documents_still_save_as_the_v1_envelope_shape`
/// (which only checks `frames` is absent and `layers` is present): pins the *exact* key set and
/// the literal `version` value, so a v2-only field (`frame_duration_ms`/`loop_playback`) leaking
/// into a single-frame save would fail here even if it slipped past a looser "no frames key"
/// check.
#[test]
fn a_single_frame_document_saves_as_a_literal_version_1_envelope_with_exactly_the_pre_frames_key_set() {
    let mut doc = Document::new(3, 3);
    doc.set_cell(0, 1, 1, cell('m', Rgba::WHITE, Rgba(1, 2, 3, 255)));
    let json = save_string(&doc);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["background", "height", "layer_meta", "layers", "version", "width"],
        "a single-frame save must be exactly the pre-frames v1 key set plus the additive layer_meta \
         field — no v2-only field leaks in"
    );
    assert_eq!(value["version"], 1, "a single-frame document must be tagged version 1, not CURRENT_VERSION (2)");
}

/// A literal, hand-authored v1 JSON string — exactly the shape a pre-frames build would have
/// written, with no `frames`/`frame_duration_ms`/`loop_playback` key anywhere — must load with
/// byte-identical cell content and the documented frame defaults. Distinct from the coder's own
/// `a_v1_file_with_no_frames_field_loads_as_a_single_frame_document` (which only asserts
/// `frame_count() == 1`): this asserts the actual decoded content and the default metadata too.
#[test]
fn a_hand_authored_pre_frames_v1_fixture_loads_with_byte_identical_cells_and_default_frame_metadata() {
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
    assert_eq!(doc.frame_count(), 1);
    assert_eq!(doc.active_frame(), 0);
    assert_eq!(doc.cell(0, 0, 0).unwrap().ch, 'a');
    assert_eq!(doc.cell(0, 1, 0).unwrap().ch, ' ');
    assert_eq!(doc.cell(0, 0, 1).unwrap().ch, ' ');
    assert_eq!(doc.cell(0, 1, 1).unwrap().ch, 'z');
    assert_eq!(doc.background, Rgba(0, 0, 255, 255));
    assert_eq!(
        doc.frame_duration_ms,
        Document::DEFAULT_FRAME_DURATION_MS,
        "no frame_duration_ms in the file — must fall back to the documented default"
    );
    assert!(doc.loop_playback, "no loop_playback in the file — must fall back to the documented default (true)");
}

/// Per the plan's documented D-format contract (`gascii-core/src/model.rs`'s `frame_duration_ms`
/// doc comment / the architect plan's Known Limitations): fps/loop are only ever meaningful, and
/// only ever serialized, once a document has more than one frame. A single-frame document with a
/// customized duration/loop still saves as v1 (no such fields exist in that envelope at all), so
/// the customization is intentionally lossy here. No existing test exercised this specific
/// scenario end-to-end (a coverage gap against the plan's own documented edge case) — pinned so a
/// future change to this contract shows up as a deliberate test update, not a silent drift.
#[test]
fn a_single_frame_documents_customized_playback_metadata_does_not_survive_a_save_load_round_trip() {
    let mut doc = Document::new(3, 3);
    doc.frame_duration_ms = 999;
    doc.loop_playback = false;

    let json = save_string(&doc);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(value.get("frame_duration_ms").is_none(), "a v1-shaped save must not carry frame_duration_ms at all");
    assert!(value.get("loop_playback").is_none(), "a v1-shaped save must not carry loop_playback at all");

    let loaded = load_str(&json).unwrap();
    assert_eq!(
        loaded.frame_duration_ms,
        Document::DEFAULT_FRAME_DURATION_MS,
        "the customization does not survive — a v1 load always falls back to the documented default"
    );
    assert!(loaded.loop_playback, "loop_playback likewise falls back to its documented default (true)");
}

// --- resize/clear composed with frames and undo ---

/// Extends the coder's own `resize_document_resizes_every_frame_not_just_the_active_one` (which
/// only checks the forward grow direction) with the undo half, across three frames, plus a
/// save/load round trip of both the shrunken and the undo-restored state.
#[test]
fn resizing_a_three_frame_document_then_undoing_restores_every_frames_cropped_content_exactly_and_both_states_round_trip_through_save_load(
) {
    let mut doc = Document::new(5, 5);
    let mut history = History::new();

    for (x, y, ch) in [(0, 0, 'a'), (4, 0, 'b'), (0, 4, 'c'), (4, 4, 'd')] {
        doc.set_cell(0, x, y, cell(ch, Rgba::WHITE, Rgba::TRANSPARENT));
    }

    let e1 = add_frame(&doc, 1, Frame::blank(5, 5)).unwrap();
    history.apply(&mut doc, e1);
    assert!(doc.set_active_frame(1));
    doc.set_cell(0, 4, 4, cell('e', Rgba(1, 2, 3, 255), Rgba::TRANSPARENT));
    assert!(doc.set_active_frame(0));

    let e2 = add_frame(&doc, 2, Frame::blank(5, 5)).unwrap();
    history.apply(&mut doc, e2);
    assert!(doc.set_active_frame(2));
    doc.set_cell(0, 4, 4, cell('f', Rgba(4, 5, 6, 255), Rgba::TRANSPARENT));
    assert!(doc.set_active_frame(0));

    let before_resize = doc.clone();

    // Shrink to 2x2 (top-left anchored): only the top-left corner survives directly; frame 1 and
    // frame 2's only content each sat in the cropped-away bottom-right corner.
    let resize_edit = resize_document(&doc, 2, 2, start()).unwrap().unwrap();
    history.apply(&mut doc, resize_edit);
    assert_eq!((doc.width, doc.height), (2, 2));
    assert_eq!(doc.cell_at(0, 0, 0, 0).unwrap().ch, 'a', "frame 0's top-left survives the crop");
    assert_eq!(doc.cell_at(1, 0, 0, 0), Some(&Cell::BLANK), "frame 1's only content ('e') was cropped away");
    assert_eq!(doc.cell_at(2, 0, 0, 0), Some(&Cell::BLANK), "frame 2's only content ('f') was cropped away");

    // Undo restores every frame's cropped-away content exactly, not just the active frame's.
    assert!(history.undo(&mut doc));
    assert_eq!(doc, before_resize, "undo must restore all three frames' cropped content byte-exact");
    assert_eq!(doc.cell_at(1, 0, 4, 4).unwrap().ch, 'e');
    assert_eq!(doc.cell_at(2, 0, 4, 4).unwrap().ch, 'f');

    // Both the shrunken and the undo-restored states round-trip through save/load.
    assert!(history.redo(&mut doc));
    let shrunk = doc.clone();
    assert_eq!(load_str(&save_string(&doc)).unwrap(), shrunk);

    assert!(history.undo(&mut doc));
    assert_eq!(load_str(&save_string(&doc)).unwrap(), before_resize);
}

/// Extends the coder's own `clear_document_only_blanks_the_active_frame_leaving_other_frames_
/// untouched` (unit-level, no persistence) with a save/load round trip of the post-clear state.
#[test]
fn clearing_the_active_frame_leaves_other_frames_intact_through_a_save_load_round_trip() {
    let mut doc = Document::new(3, 3);
    let mut history = History::new();
    let e1 = add_frame(&doc, 1, Frame::blank(3, 3)).unwrap();
    history.apply(&mut doc, e1); // the new frame becomes active
    assert!(doc.set_active_frame(0));

    doc.set_cell(0, 0, 0, cell('x', Rgba::WHITE, Rgba::TRANSPARENT)); // frame 0
    assert!(doc.set_active_frame(1));
    doc.set_cell(0, 1, 1, cell('y', Rgba::WHITE, Rgba::TRANSPARENT)); // frame 1
    assert!(doc.set_active_frame(0));

    let clear_edit = clear_document(&doc).unwrap();
    history.apply(&mut doc, clear_edit);
    assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK), "the active frame (0) must be cleared");
    assert_eq!(doc.cell_at(1, 0, 1, 1).unwrap().ch, 'y', "frame 1 must survive untouched");

    let loaded = load_str(&save_string(&doc)).unwrap();
    assert_eq!(loaded.cell_at(0, 0, 0, 0), Some(&Cell::BLANK));
    assert_eq!(loaded.cell_at(1, 0, 1, 1).unwrap().ch, 'y');
    assert_eq!(loaded, doc);
}

// --- cap enforcement composed across seams, via legitimate ops ---

/// Builds a document up to exactly `MAX_FRAMES` through legitimate, individually-applied
/// `add_frame` calls (not a single oversized construction), then pushes one more — rejected — and
/// confirms the document is left completely unmodified and still fully usable (an ordinary cell
/// edit and its undo still work), not just that the call returned `Err`.
#[test]
fn add_frame_rejection_after_legitimately_building_up_to_max_frames_leaves_the_document_and_history_fully_usable() {
    let mut doc = Document::new(2, 2);
    let mut history = History::new();
    for i in 0..Document::MAX_FRAMES - 1 {
        let edit = add_frame(&doc, i, Frame::blank(2, 2)).unwrap();
        history.apply(&mut doc, edit);
    }
    assert_eq!(doc.frame_count(), Document::MAX_FRAMES);
    let before_rejection = doc.clone();

    let result = add_frame(&doc, 0, Frame::blank(2, 2));
    assert!(matches!(result, Err(FrameOpError::TooManyFrames { .. })));
    assert_eq!(doc, before_rejection, "a rejected add_frame call must leave the document completely unmodified");

    // Still fully usable afterward: an ordinary cell edit and its undo work cleanly. Addressed
    // against frame 0 explicitly (`cell_at`, not the implicit `cell`) — the 254 preceding inserts
    // at ever-lower indices dragged the active-frame cursor up to the last frame along with them
    // (each insert lands at/before the then-active index), so `doc.cell(...)`'s implicit
    // addressing would read frame 254, not frame 0.
    let cell_edit = Edit::Cells(vec![CellEdit { frame: 0, layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('z', Rgba::WHITE, Rgba::TRANSPARENT) }]);
    history.apply(&mut doc, cell_edit);
    assert_eq!(doc.cell_at(0, 0, 0, 0).unwrap().ch, 'z');
    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell_at(0, 0, 0, 0), Some(&Cell::BLANK));
}

/// Composes `frame_ops::add_frame`'s own per-call cap checks with `resize_document`'s independent
/// joint-budget check: five frames, each individually built through the *real* `add_frame` API
/// (not `resize.rs`'s own unit test's `layers_mut()` escape hatch) at MAX_LAYERS each, cheap at a
/// tiny extent — then a resize toward the max extent must still be caught by the joint budget,
/// proving the two independently-reviewed seams compose correctly rather than one silently
/// trusting the other.
#[test]
fn frames_built_up_through_legitimate_add_frame_calls_still_trip_resize_documents_joint_budget_check() {
    let mut doc = Document::new(2, 2);
    let mut history = History::new();
    for i in 0..5 {
        let frame = Frame { layers: (0..Document::MAX_LAYERS).map(|_| Layer::blank(2, 2)).collect(), duration_override: None };
        let edit = add_frame(&doc, i, frame).unwrap();
        history.apply(&mut doc, edit);
    }
    assert_eq!(doc.frame_count(), 6, "1 original + 5 legitimately added, each at the per-frame MAX_LAYERS cap");
    let before_resize_attempt = doc.clone();

    let started = std::time::Instant::now();
    let result = resize_document(&doc, Document::MAX_WIDTH, Document::MAX_HEIGHT, start());
    assert!(started.elapsed() < std::time::Duration::from_millis(200), "must reject before allocating, not after");
    assert!(matches!(result, Err(ResizeError::TotalCellBudgetExceeded { .. })));
    assert_eq!(doc, before_resize_attempt, "a rejected resize must leave the document completely unmodified");
}
