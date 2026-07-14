//! Cross-feature integration for fill/rect/line/selection/clipboard: the tools interacting with
//! each other, with undo/redo, and with persistence — the seams no single tool's own unit tests
//! reach. Complements `draw_integration.rs` (drawing) and `persist_integration.rs` (persistence)
//! by covering the structural tools through the same real Tool/History pipeline.

use gascii_core::{
    load_str, save_string, BrushShape, Cell, CellPatch, CellRect, DensityMode, Document, Fixed,
    FloodFill, History, Line, PlaneMask, Rectangle, Rgba, SelectionTool, Tool, ToolCtx, ToolEvent,
    ToolResponse,
};

fn ctx(mask: PlaneMask, glyph: char, fg: Rgba, bg: Rgba) -> ToolCtx {
    ToolCtx { layer: 0, glyph, fg, bg, mask, density: DensityMode::Fixed(Fixed(1.0)), ramp: Vec::new(), size: 1, shape: BrushShape::Square }
}

/// Drives a full press -> drag(...) -> release gesture, committing the result (if any) into
/// `history`/`doc`. Works for any Tool whose commit lifecycle is Press/Drag/Release (fill,
/// rectangle, line) — mirrors `draw_integration.rs`'s `stroke` helper.
fn stroke(tool: &mut dyn Tool, history: &mut History, doc: &mut Document, tctx: &ToolCtx, path: &[(u16, u16)]) {
    let (&first, rest) = path.split_first().expect("stroke path must have at least one point");
    tool.update(ToolEvent::Press { x: first.0, y: first.1 }, tctx, doc);
    for &(x, y) in rest {
        tool.update(ToolEvent::Drag { x, y }, tctx, doc);
    }
    if let ToolResponse::Commit(Some(edit)) = tool.update(ToolEvent::Release, tctx, doc) {
        history.apply(doc, edit);
    }
}

fn commit_edit(resp: ToolResponse) -> Option<gascii_core::Edit> {
    match resp {
        ToolResponse::Commit(edit) => edit,
        other => panic!("expected Commit, got {other:?}"),
    }
}

// --- 1. Selection x persistence ---

#[test]
fn a_floating_selection_must_be_flushed_before_save_or_the_move_is_silently_lost_from_the_saved_file() {
    let mut doc = Document::new(10, 10);
    let content = Cell { ch: 'Q', fg: Rgba(1, 2, 3, 255), bg: Rgba(4, 5, 6, 255) };
    for y in 2..5u16 {
        for x in 2..5u16 {
            doc.set_cell(0, x, y, content);
        }
    }
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut sel = SelectionTool::new();
    sel.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
    sel.update(ToolEvent::Drag { x: 4, y: 4 }, &tctx, &doc);
    sel.update(ToolEvent::Release, &tctx, &doc);
    sel.update(ToolEvent::Press { x: 3, y: 3 }, &tctx, &doc); // lift
    sel.update(ToolEvent::Drag { x: 6, y: 6 }, &tctx, &doc); // move, not yet dropped

    // While floating, the document itself is untouched (the float is overlay-only) — a save
    // taken at this exact instant, without the flush every real save/export/copy site performs
    // first, would silently omit the move entirely.
    let unflushed_snapshot = doc.clone();

    // The trigger every save/export/copy path in the app fires before reading `doc`: commit
    // (drop) whatever the active cross-frame tool has pending.
    if let ToolResponse::Commit(Some(edit)) = sel.update(ToolEvent::Commit, &tctx, &doc) {
        history.apply(&mut doc, edit);
    }
    assert_ne!(doc, unflushed_snapshot, "the drop must actually change the document relative to the unflushed state");

    let json = save_string(&doc);
    let loaded = load_str(&json).expect("a document with a dropped selection move must save and reload");
    assert_eq!(loaded, doc, "the saved+reloaded document must match the flushed (dropped) state, not the pre-drop float");
}

#[test]
fn after_save_and_reload_the_dropped_moves_edit_is_baked_in_and_has_no_undo_entry_of_its_own() {
    let mut doc = Document::new(10, 10);
    let content = Cell { ch: 'Q', fg: Rgba(1, 2, 3, 255), bg: Rgba(4, 5, 6, 255) };
    for y in 0..2u16 {
        for x in 0..2u16 {
            doc.set_cell(0, x, y, content);
        }
    }
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut sel = SelectionTool::new();
    sel.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    sel.update(ToolEvent::Drag { x: 1, y: 1 }, &tctx, &doc);
    sel.update(ToolEvent::Release, &tctx, &doc);
    sel.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc); // lift
    sel.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc); // move
    if let ToolResponse::Commit(Some(edit)) = sel.update(ToolEvent::Commit, &tctx, &doc) {
        history.apply(&mut doc, edit);
    }

    let loaded = load_str(&save_string(&doc)).unwrap();

    // History is never persisted: a freshly loaded document — even one whose entire visible state
    // was produced by a selection drop just before saving — starts with a completely empty undo
    // stack. The drop itself is not "one step back" after a reload; it is simply baked in.
    let mut history2 = History::new();
    let mut loaded2 = loaded.clone();
    assert!(!history2.can_undo(), "a freshly loaded document must have no undo entries, including for the drop that produced its content");

    // Prove this isn't a coincidence of an empty stack: a genuinely new edit on the loaded
    // document is exactly one entry, and undoing it restores precisely the loaded (post-drop)
    // state — nothing from the pre-load drop leaks back in because there is nothing left to leak.
    let mut rect = Rectangle::new();
    stroke(&mut rect, &mut history2, &mut loaded2, &tctx, &[(8, 8), (9, 9)]);
    assert!(history2.can_undo());
    assert!(history2.undo(&mut loaded2));
    assert_eq!(loaded2, loaded, "undoing the one post-load edit must restore exactly the loaded (post-drop) state");
    assert!(!history2.can_undo());
}

// --- 2. Fill/rect/line x undo x join ---

#[test]
fn rectangle_drawn_over_an_existing_line_then_undone_restores_the_lines_original_glyph_at_the_crossing_cells() {
    let mut doc = Document::new(12, 12);
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    // A vertical line first, independently committed.
    let mut line = Line::new();
    stroke(&mut line, &mut history, &mut doc, &tctx, &[(5, 0), (5, 11)]);
    let after_line = doc.clone();
    assert_eq!(doc.cell(0, 5, 2).unwrap().ch, '│');

    // A rectangle whose top and bottom edges cross that line at (5,2) and (5,8).
    let mut rect = Rectangle::new();
    stroke(&mut rect, &mut history, &mut doc, &tctx, &[(2, 2), (8, 8)]);
    assert_eq!(doc.cell(0, 5, 2).unwrap().ch, '┼');
    assert_eq!(doc.cell(0, 5, 8).unwrap().ch, '┼');
    assert_ne!(doc, after_line);

    // Undo the rectangle: the crossing cells must revert to the line's original glyph — the
    // join-modified neighbor cells, not just the rectangle's own newly-drawn ones — not to Blank
    // or to some intermediate state.
    assert!(history.undo(&mut doc));
    assert_eq!(doc, after_line, "undo of the rectangle must restore the exact pre-rectangle state byte-for-byte");
    assert_eq!(doc.cell(0, 5, 2).unwrap().ch, '│', "the crossing cell must revert to the line's glyph, not Blank");
    assert_eq!(doc.cell(0, 5, 8).unwrap().ch, '│');
}

#[test]
fn fill_on_a_1024x1024_document_from_a_corner_is_exactly_one_undo_entry_and_round_trips_byte_exact() {
    let mut doc = Document::new(1024, 1024);
    let before = doc.clone();
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut fill = FloodFill::new();
    fill.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    let resp = fill.update(ToolEvent::Release, &tctx, &doc);
    let edit = commit_edit(resp).expect("a full-canvas fill from a blank corner must commit an edit");
    history.apply(&mut doc, edit);

    assert!(doc.layers[0].cells().iter().all(|c| c.ch == '#'), "the whole canvas must be filled");
    assert!(history.can_undo());
    assert!(!history.can_redo());

    assert!(history.undo(&mut doc));
    assert_eq!(doc, before, "undo of a full-canvas fill must restore byte-exact blank state");
    assert!(!history.can_undo(), "a full-canvas fill must be exactly one undo entry");

    assert!(history.redo(&mut doc));
    assert!(doc.layers[0].cells().iter().all(|c| c.ch == '#'));
}

#[test]
fn single_point_rectangle_degenerates_the_same_way_a_single_point_line_does() {
    // Both dimensions collapse at once (press == drag target): the rectangle tool's degenerate
    // branch order (checked y0==y1 before x0==x1) treats it as a horizontal run, exactly matching
    // Line's own documented single-point behavior (`single_point_line_is_treated_as_horizontal` in
    // tools/line.rs) — a consistency gap neither tool's own test suite checks against the other.
    let doc = Document::new(10, 10);
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut rect = Rectangle::new();
    rect.update(ToolEvent::Press { x: 3, y: 3 }, &tctx, &doc);
    rect.update(ToolEvent::Drag { x: 3, y: 3 }, &tctx, &doc);
    let resp = rect.update(ToolEvent::Release, &tctx, &doc);
    let gascii_core::Edit::Cells(cells) = commit_edit(resp).expect("a single-point rectangle must still commit one cell") else {
        panic!("expected a Cells edit");
    };
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].after.ch, '─');
}

#[test]
fn rectangle_spanning_the_documents_full_extent_produces_exact_corner_glyphs_with_no_out_of_bounds_cells() {
    let doc = Document::new(6, 6);
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut rect = Rectangle::new();
    rect.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    rect.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc);
    let resp = rect.update(ToolEvent::Release, &tctx, &doc);
    let gascii_core::Edit::Cells(cells) = commit_edit(resp).expect("expected a committed edit") else {
        panic!("expected a Cells edit");
    };
    let by_pos: std::collections::HashMap<(u16, u16), char> = cells.iter().map(|c| ((c.x, c.y), c.after.ch)).collect();
    assert_eq!(by_pos[&(0, 0)], '┌');
    assert_eq!(by_pos[&(5, 0)], '┐');
    assert_eq!(by_pos[&(0, 5)], '└');
    assert_eq!(by_pos[&(5, 5)], '┘');
    for c in &cells {
        assert!(c.x < 6 && c.y < 6, "no cell of a rectangle exactly at the document's own extent may fall out of bounds");
    }
}

#[test]
fn moving_or_pasting_a_stamp_writes_full_cells_regardless_of_the_active_plane_mask() {
    // Locked decision: move/paste transplant whole cells (glyph+fg+bg) — the plane mask governs
    // ordinary drawing tools' writes but is deliberately never consulted for a float's drop, so
    // moving colored content never silently drops its colors just because, say, only the glyph
    // plane happened to be enabled at the time.
    let mut doc = Document::new(10, 10);
    let content = Cell { ch: 'Z', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) };
    doc.set_cell(0, 1, 1, content);
    let glyph_only_mask = PlaneMask { glyph: true, fg: false, bg: false };
    let tctx = ctx(glyph_only_mask, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut sel = SelectionTool::new();
    sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
    sel.update(ToolEvent::Drag { x: 1, y: 1 }, &tctx, &doc); // 1x1 selection
    sel.update(ToolEvent::Release, &tctx, &doc);
    sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
    sel.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc); // move, mask still glyph-only
    let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
    let edit = commit_edit(resp).expect("expected a write edit");
    let mut history = History::new();
    history.apply(&mut doc, edit);

    assert_eq!(doc.cell(0, 5, 5), Some(&content), "the moved cell must carry its full original fg/bg, not just the glyph the active mask would have allowed a normal stroke to write");
}

// --- 3. Clipboard round-trips ---

#[test]
fn copying_a_region_and_pasting_it_elsewhere_reproduces_identical_glyphs_and_colors_via_the_internal_clipboard_path() {
    let mut doc = Document::new(10, 10);
    let cells = [
        ((0u16, 0u16), Cell { ch: 'a', fg: Rgba(10, 20, 30, 255), bg: Rgba(40, 50, 60, 255) }),
        ((1, 0), Cell { ch: 'b', fg: Rgba(11, 21, 31, 255), bg: Rgba(41, 51, 61, 255) }),
        ((0, 1), Cell { ch: 'c', fg: Rgba(12, 22, 32, 255), bg: Rgba(42, 52, 62, 255) }),
        ((1, 1), Cell { ch: 'd', fg: Rgba(13, 23, 33, 255), bg: Rgba(43, 53, 63, 255) }),
    ];
    for &(pos, cell) in &cells {
        doc.set_cell(0, pos.0, pos.1, cell);
    }
    let rect = CellRect { x0: 0, y0: 0, x1: 1, y1: 1 };
    let patch = CellPatch::from_region(&doc, rect, 0);

    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut sel = SelectionTool::new();
    sel.accept_stamp(patch, (7, 7), &doc);
    let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
    let edit = commit_edit(resp).expect("expected a write-only paste edit");
    history.apply(&mut doc, edit);

    for &(pos, original) in &cells {
        let pasted = doc.cell(0, 7 + pos.0, 7 + pos.1).unwrap();
        assert_eq!(*pasted, original, "the pasted cell at offset {pos:?} must be identical (glyph AND colors) to the copied original");
    }
}

#[test]
fn round_tripping_a_copy_through_the_external_text_path_preserves_glyphs_but_resets_colors_to_the_active_ones() {
    let mut doc = Document::new(4, 1);
    doc.set_cell(0, 0, 0, Cell { ch: 'x', fg: Rgba(200, 0, 0, 255), bg: Rgba(0, 0, 200, 255) });
    doc.set_cell(0, 1, 0, Cell { ch: 'y', fg: Rgba(200, 0, 0, 255), bg: Rgba(0, 0, 200, 255) });
    let rect = CellRect { x0: 0, y0: 0, x1: 1, y1: 0 };
    let original = CellPatch::from_region(&doc, rect, 0);

    // Simulates copying (to_text -> the OS plain-text clipboard) then pasting that same text back
    // in from an *external* source (as if another application had re-typed it) — the internal
    // colored path is a separate, faster route (previous test); this one is what actually happens
    // when the OS clipboard content doesn't match the app's own internal patch.
    let text = original.to_text();
    let active_fg = Rgba(9, 9, 9, 255);
    let active_bg = Rgba::TRANSPARENT;
    let (roundtripped, dropped) = CellPatch::from_external_text(&text, active_fg, active_bg);
    assert_eq!(dropped, 0);

    assert_eq!(roundtripped.cells[0].ch, 'x');
    assert_eq!(roundtripped.cells[1].ch, 'y');
    assert_eq!(roundtripped.cells[0].fg, active_fg, "external paste must use the currently active fg, not the copied cell's original color");
    assert_ne!(roundtripped.cells[0].fg, original.cells[0].fg, "the external path must not smuggle the original color through plain text");
    assert_eq!(roundtripped.cells[0].bg, active_bg);
}

#[test]
fn pasting_a_stamp_that_hangs_off_the_document_edge_clips_the_overhanging_cells_with_no_source_to_blank() {
    let doc = Document::new(5, 5);
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut sel = SelectionTool::new();
    let patch = CellPatch { width: 3, height: 3, cells: vec![Cell { ch: 'Q', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }; 9] };
    // Stamp spans (3,3)-(5,5) on a doc whose valid indices only go up to (4,4).
    sel.accept_stamp(patch, (3, 3), &doc);
    let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
    let gascii_core::Edit::Cells(cells) = commit_edit(resp).expect("expected a clipped write edit") else {
        panic!("expected a Cells edit");
    };
    assert_eq!(cells.len(), 4, "only the 2x2 in-bounds portion of the 3x3 stamp survives the clip");
    for c in &cells {
        assert!(c.x < 5 && c.y < 5);
        assert_eq!(c.after.ch, 'Q');
    }
}

// --- 4. Join under adversarial sequencing ---

#[test]
fn two_lines_drawn_in_separate_strokes_crossing_each_other_join_into_a_cross_and_undo_peels_back_one_stroke_at_a_time() {
    let mut doc = Document::new(10, 10);
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut h_line = Line::new();
    stroke(&mut h_line, &mut history, &mut doc, &tctx, &[(0, 5), (9, 5)]);
    assert_eq!(doc.cell(0, 5, 5).unwrap().ch, '─');

    let mut v_line = Line::new();
    stroke(&mut v_line, &mut history, &mut doc, &tctx, &[(5, 0), (5, 9)]);
    assert_eq!(doc.cell(0, 5, 5).unwrap().ch, '┼', "two independently-committed lines crossing must union into a full cross");

    // Undo the second (vertical) stroke: the intersection must fall back to the horizontal line's
    // glyph, not Blank — the join's "existing" side survives a sibling stroke's undo.
    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell(0, 5, 5).unwrap().ch, '─');
    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell(0, 5, 5), Some(&Cell::BLANK));
}

#[test]
fn a_rectangle_corner_landing_on_an_existing_rectangle_corner_unions_into_a_four_way_junction() {
    let mut doc = Document::new(14, 14);
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut rect1 = Rectangle::new();
    stroke(&mut rect1, &mut history, &mut doc, &tctx, &[(2, 2), (6, 6)]);
    assert_eq!(doc.cell(0, 6, 6).unwrap().ch, '┘', "sanity: rect1's bottom-right corner");

    let mut rect2 = Rectangle::new();
    stroke(&mut rect2, &mut history, &mut doc, &tctx, &[(6, 6), (10, 10)]);
    assert_eq!(doc.cell(0, 6, 6).unwrap().ch, '┼', "two rectangle corners landing on the same cell must union into a full cross junction");
}


// --- 5. Trigger-table cross-check: one action = one undo entry, no-op = no entry ---

#[test]
fn one_action_one_undo_entry_across_fill_rectangle_line_selection_move_delete_and_paste() {
    let mut doc = Document::new(20, 20);
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
    let mut snapshots = vec![doc.clone()]; // s0: blank

    // 1. Fill: the whole blank canvas is one connected region.
    let mut fill = FloodFill::new();
    fill.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    if let ToolResponse::Commit(Some(edit)) = fill.update(ToolEvent::Release, &tctx, &doc) {
        history.apply(&mut doc, edit);
    }
    snapshots.push(doc.clone());

    // 2. Rectangle border over the now-filled canvas.
    let mut rect = Rectangle::new();
    stroke(&mut rect, &mut history, &mut doc, &tctx, &[(2, 2), (6, 6)]);
    snapshots.push(doc.clone());

    // 3. Line crossing the rectangle's border.
    let mut line = Line::new();
    stroke(&mut line, &mut history, &mut doc, &tctx, &[(4, 0), (4, 19)]);
    snapshots.push(doc.clone());

    // 4. Selection move: marquee, lift, move, drop — one combined Edit.
    let mut sel = SelectionTool::new();
    sel.update(ToolEvent::Press { x: 10, y: 10 }, &tctx, &doc);
    sel.update(ToolEvent::Drag { x: 11, y: 11 }, &tctx, &doc);
    sel.update(ToolEvent::Release, &tctx, &doc);
    sel.update(ToolEvent::Press { x: 10, y: 10 }, &tctx, &doc); // lift
    sel.update(ToolEvent::Drag { x: 15, y: 15 }, &tctx, &doc); // move
    if let ToolResponse::Commit(Some(edit)) = sel.update(ToolEvent::Commit, &tctx, &doc) {
        history.apply(&mut doc, edit);
    }
    snapshots.push(doc.clone());

    // 5. Selection delete on a fresh region.
    sel.update(ToolEvent::Press { x: 0, y: 15 }, &tctx, &doc);
    sel.update(ToolEvent::Drag { x: 1, y: 16 }, &tctx, &doc);
    sel.update(ToolEvent::Release, &tctx, &doc);
    if let ToolResponse::Commit(Some(edit)) = sel.update(ToolEvent::Delete, &tctx, &doc) {
        history.apply(&mut doc, edit);
    }
    snapshots.push(doc.clone());

    // 6. Paste: accept_stamp + Commit, no source to blank.
    let patch = CellPatch { width: 2, height: 2, cells: vec![Cell { ch: 'P', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }; 4] };
    sel.accept_stamp(patch, (17, 0), &doc);
    if let ToolResponse::Commit(Some(edit)) = sel.update(ToolEvent::Commit, &tctx, &doc) {
        history.apply(&mut doc, edit);
    }
    snapshots.push(doc.clone());

    assert_eq!(snapshots.len(), 7);
    for i in 1..snapshots.len() {
        assert_ne!(snapshots[i], snapshots[i - 1], "action {i} must actually have changed the document");
    }

    // A full undo must walk back through every snapshot in exact reverse order — proving each of
    // the six actions landed as precisely one undo entry, no more, no fewer.
    for i in (0..snapshots.len() - 1).rev() {
        assert!(history.undo(&mut doc));
        assert_eq!(doc, snapshots[i], "undo did not land on snapshot {i}");
    }
    assert!(!history.can_undo(), "exactly six entries expected — no extra hidden entries left over");

    // A full redo must walk forward through the same snapshots.
    for snapshot in snapshots.iter().skip(1) {
        assert!(history.redo(&mut doc));
        assert_eq!(&doc, snapshot);
    }
    assert!(!history.can_redo());
}

#[test]
fn a_no_op_attempt_of_each_new_tool_action_commits_none_and_never_reaches_history() {
    let doc = Document::new(10, 10); // fully Blank
    let history = History::new();
    let blank_tctx = ctx(PlaneMask::ALL, ' ', Rgba::WHITE, Rgba::TRANSPARENT); // matches Cell::BLANK exactly
    let any_tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    // Fill: painting Blank over an already-blank region changes nothing.
    let mut fill = FloodFill::new();
    fill.update(ToolEvent::Press { x: 0, y: 0 }, &blank_tctx, &doc);
    assert!(matches!(fill.update(ToolEvent::Release, &blank_tctx, &doc), ToolResponse::Commit(None)));

    // Rectangle/Line released with no Press at all.
    let mut rect = Rectangle::new();
    assert!(matches!(rect.update(ToolEvent::Release, &any_tctx, &doc), ToolResponse::Commit(None)));
    let mut line = Line::new();
    assert!(matches!(line.update(ToolEvent::Release, &any_tctx, &doc), ToolResponse::Commit(None)));

    // Selection: a marquee-only session drops to nothing; Delete with nothing selected.
    let mut sel = SelectionTool::new();
    sel.update(ToolEvent::Press { x: 1, y: 1 }, &any_tctx, &doc);
    sel.update(ToolEvent::Drag { x: 2, y: 2 }, &any_tctx, &doc);
    sel.update(ToolEvent::Release, &any_tctx, &doc);
    assert!(matches!(sel.update(ToolEvent::Commit, &any_tctx, &doc), ToolResponse::Commit(None)));
    let mut sel2 = SelectionTool::new();
    assert!(matches!(sel2.update(ToolEvent::Delete, &any_tctx, &doc), ToolResponse::Commit(None)));

    // Paste: an empty patch (e.g. an all-rejected external paste) has nothing to write.
    let mut sel3 = SelectionTool::new();
    let empty_patch = CellPatch { width: 0, height: 0, cells: vec![] };
    sel3.accept_stamp(empty_patch, (0, 0), &doc);
    assert!(matches!(sel3.update(ToolEvent::Commit, &any_tctx, &doc), ToolResponse::Commit(None)));

    assert!(!history.can_undo(), "none of these no-op attempts may ever have reached History::apply");
}
