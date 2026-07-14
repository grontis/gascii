//! Cross-feature integration for resize and the density brush: resize interacting with the rest
//! of the pipeline (floats/bursts, persistence, undo/redo, cap boundaries, degenerate extremes),
//! the density brush interacting with masks/persistence/scale, and a mixed
//! `Edit::Cells`/`Edit::Resize` history walk — the seams no single tool's or `resize_document`'s
//! own unit tests reach. Complements the other `*_integration.rs` suites.

use gascii_core::{
    builtin_ramps, load_str, resize_document, save_string, BrushShape, Buildup, Cell, CellPatch, DensityBrush,
    DensityMode, Document, Edit, Fixed, History, Pencil, PlaneMask, ResizeError, Rgba,
    SelectionTool, TextTool, Tool, ToolCtx, ToolEvent, ToolResponse,
};

fn ctx(density: DensityMode, ramp: &str, mask: PlaneMask, glyph: char, fg: Rgba, bg: Rgba) -> ToolCtx {
    ToolCtx { layer: 0, glyph, fg, bg, mask, density, ramp: ramp.chars().collect(), size: 1, shape: BrushShape::Square }
}

fn fixed_ctx(mask: PlaneMask, glyph: char, fg: Rgba, bg: Rgba) -> ToolCtx {
    ctx(DensityMode::Fixed(Fixed(1.0)), "", mask, glyph, fg, bg)
}

/// Drives a full press -> drag(...) -> release gesture, committing the result (if any) into
/// `history`/`doc`. Shared shape with `structure_integration.rs`'s `stroke` helper — works for any
/// `Tool` whose commit lifecycle is Press/Drag/Release (pencil, the density brush).
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

fn cell(ch: char) -> Cell {
    Cell { ch, fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }
}

// --- 1. Resize x floats/bursts (the app's own flush-before-resize trigger-table row) ---

#[test]
fn resize_after_flushing_a_pending_text_burst_bakes_the_burst_in_before_growing_the_document() {
    let mut doc = Document::new(5, 5);
    let mut history = History::new();
    let tctx = fixed_ctx(PlaneMask::ALL, ' ', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut text = TextTool::new();
    text.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    text.update(ToolEvent::Char('H'), &tctx, &doc);
    text.update(ToolEvent::Char('i'), &tctx, &doc);
    // Not yet committed: the burst lives only in the tool's own pending overlay.
    assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK), "sanity: an uncommitted burst never touches doc directly");

    // Exactly what the app's resize dialog's Apply handler does: flush, then resize_document.
    if let ToolResponse::Commit(Some(edit)) = text.update(ToolEvent::Commit, &tctx, &doc) {
        history.apply(&mut doc, edit);
    }
    assert_eq!(doc.cell(0, 0, 0).unwrap().ch, 'H');
    assert_eq!(doc.cell(0, 1, 0).unwrap().ch, 'i');

    let edit = resize_document(&doc, 10, 10).unwrap().unwrap();
    history.apply(&mut doc, edit);
    assert_eq!(doc.width, 10);
    assert_eq!(doc.height, 10);
    assert_eq!(doc.cell(0, 0, 0).unwrap().ch, 'H', "the flushed burst's content must survive the grow");
    assert_eq!(doc.cell(0, 1, 0).unwrap().ch, 'i');
    assert_eq!(doc.cell(0, 9, 9), Some(&Cell::BLANK));
}

#[test]
fn resize_after_flushing_a_floating_selection_drop_operates_on_the_post_drop_document_not_a_stale_pre_drop_one() {
    let mut doc = Document::new(8, 8);
    let mut history = History::new();
    let content = cell('Q');
    for y in 2..5u16 {
        for x in 2..5u16 {
            doc.set_cell(0, x, y, content);
        }
    }
    let tctx = fixed_ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut sel = SelectionTool::new();
    sel.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
    sel.update(ToolEvent::Drag { x: 4, y: 4 }, &tctx, &doc);
    sel.update(ToolEvent::Release, &tctx, &doc);
    sel.update(ToolEvent::Press { x: 3, y: 3 }, &tctx, &doc); // lift (interior point)
    sel.update(ToolEvent::Drag { x: 4, y: 4 }, &tctx, &doc); // move by (+1,+1) -> new region (3,3)-(5,5)

    // Exactly what the app's resize Apply handler does: flush (drop) before reading self.doc.
    if let ToolResponse::Commit(Some(edit)) = sel.update(ToolEvent::Commit, &tctx, &doc) {
        history.apply(&mut doc, edit);
    }
    assert_eq!(doc.cell(0, 2, 2), Some(&Cell::BLANK), "sanity: the source is vacated by the drop");
    assert_eq!(doc.cell(0, 3, 3), Some(&content));
    assert_eq!(doc.cell(0, 5, 5), Some(&content));

    let edit = resize_document(&doc, 6, 6).unwrap().unwrap();
    history.apply(&mut doc, edit);
    assert_eq!((doc.width, doc.height), (6, 6));
    // If resize had operated on a stale pre-drop snapshot, (2,2) would be 'Q' again here instead
    // of the vacated Blank the actual (flushed) document held at the moment of resize.
    assert_eq!(doc.cell(0, 2, 2), Some(&Cell::BLANK), "resize must see the post-drop document, not a stale pre-drop one");
    assert_eq!(doc.cell(0, 3, 3), Some(&content));
    assert_eq!(doc.cell(0, 5, 5), Some(&content));
}

// --- 2. Resize x persistence ---

#[test]
fn resize_then_save_then_load_round_trip_preserves_the_new_extent_and_content() {
    let mut doc = Document::new(5, 5);
    let mut history = History::new();
    let tctx = fixed_ctx(PlaneMask::ALL, '#', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
    let mut pencil = Pencil::new();
    stroke(&mut pencil, &mut history, &mut doc, &tctx, &[(0, 0), (1, 0)]);

    // Mixed grow (height) and shrink (width) in one resize, same as `resize.rs`'s own unit test,
    // but here carried all the way through a save/load round trip.
    let edit = resize_document(&doc, 3, 8).unwrap().unwrap();
    history.apply(&mut doc, edit);
    assert_eq!((doc.width, doc.height), (3, 8));

    let loaded = load_str(&save_string(&doc)).expect("a resized document must save and reload");
    assert_eq!(loaded.width, 3);
    assert_eq!(loaded.height, 8);
    assert_eq!(loaded.cell(0, 0, 0).unwrap().ch, '#');
    assert_eq!(loaded.cell(0, 1, 0).unwrap().ch, '#');
    assert_eq!(loaded, doc, "the loaded document must be byte-for-byte identical to the resized one");
}

#[test]
fn resize_then_undo_then_redo_then_save_round_trips_the_redone_post_resize_state() {
    let mut doc = Document::new(4, 4);
    doc.set_cell(0, 1, 1, cell('m'));
    let mut history = History::new();

    let edit = resize_document(&doc, 9, 9).unwrap().unwrap();
    history.apply(&mut doc, edit);
    let after_grow = doc.clone();

    assert!(history.undo(&mut doc));
    assert_eq!(doc.width, 4, "undo must restore the pre-resize extent");

    assert!(history.redo(&mut doc));
    assert_eq!(doc, after_grow, "redo must restore exactly the post-resize state");

    let loaded = load_str(&save_string(&doc)).unwrap();
    assert_eq!(loaded, after_grow, "a save taken after redo must reflect the redone (grown) extent, not the undone one");
}

// --- 3. Resize at cap boundaries and degenerate extremes ---

#[test]
fn resize_one_past_the_cap_is_cleanly_rejected_while_exactly_at_the_cap_applies_as_one_undo_entry() {
    let mut doc = Document::new(10, 10);
    let mut history = History::new();

    assert_eq!(
        resize_document(&doc, Document::MAX_WIDTH + 1, Document::MAX_HEIGHT),
        Err(ResizeError::TooLarge {
            width: Document::MAX_WIDTH + 1,
            height: Document::MAX_HEIGHT,
            max_width: Document::MAX_WIDTH,
            max_height: Document::MAX_HEIGHT,
        })
    );
    assert!(!history.can_undo(), "a rejected resize request must never reach History");

    let edit = resize_document(&doc, Document::MAX_WIDTH, Document::MAX_HEIGHT).unwrap().unwrap();
    history.apply(&mut doc, edit);
    assert_eq!((doc.width, doc.height), (Document::MAX_WIDTH, Document::MAX_HEIGHT));
    assert!(history.can_undo());

    assert!(history.undo(&mut doc));
    assert_eq!((doc.width, doc.height), (10, 10), "exactly one undo must fully restore the pre-cap-resize extent");
}

#[test]
fn shrinking_to_1x1_then_growing_back_preserves_only_the_top_left_anchor_cell_not_the_rest_of_the_original_content() {
    // Resize is a fresh top-left-anchored op each time, not an undo — content lost to a shrink is
    // genuinely gone from a subsequent grow (only History's undo stack can bring it back), except
    // for whatever still sits at the permanent top-left anchor.
    let mut doc = Document::new(5, 5);
    for y in 0..5u16 {
        for x in 0..5u16 {
            doc.set_cell(0, x, y, cell((b'a' + (x + y * 5) as u8) as char));
        }
    }
    let original = doc.clone();
    let mut history = History::new();

    let shrink = resize_document(&doc, 1, 1).unwrap().unwrap();
    history.apply(&mut doc, shrink);
    assert_eq!((doc.width, doc.height), (1, 1));
    assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));

    let grow = resize_document(&doc, 5, 5).unwrap().unwrap();
    history.apply(&mut doc, grow);
    assert_eq!((doc.width, doc.height), (5, 5));
    assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')), "the top-left anchor cell survives both operations");
    assert_ne!(doc, original, "content elsewhere must NOT be resurrected by growing back to the original size");
    assert_eq!(doc.cell(0, 4, 4), Some(&Cell::BLANK), "content lost to the shrink is genuinely gone, not restored");

    // History still faithfully recorded two distinct entries (one mutation, one entry) —
    // undoing both, in order, restores first the 1x1 state, then the full original content.
    assert!(history.undo(&mut doc));
    assert_eq!((doc.width, doc.height), (1, 1));
    assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));
    assert!(history.undo(&mut doc));
    assert_eq!(doc, original, "undoing both resizes in order must restore the exact original 5x5 content");
    assert!(!history.can_undo());
}

#[test]
fn a_same_size_resize_attempt_mid_session_produces_no_history_entry_and_a_single_undo_still_reverts_only_the_real_edit() {
    let mut doc = Document::new(6, 6);
    let mut history = History::new();
    let tctx = fixed_ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut pencil = Pencil::new();
    stroke(&mut pencil, &mut history, &mut doc, &tctx, &[(0, 0)]);
    let after_pencil = doc.clone();

    assert_eq!(resize_document(&doc, 6, 6).unwrap(), None, "same-size resize is a no-op with no Edit produced");
    assert_eq!(doc, after_pencil, "a same-size resize attempt (never applied, since there was no Edit) must not alter the document");

    assert!(history.undo(&mut doc));
    assert_eq!(doc, Document::new(6, 6), "the one real undo must revert exactly the pencil stroke, proving no phantom resize entry was pushed");
    assert!(!history.can_undo());
}

// --- 4. Density brush x the tool ecosystem ---

#[test]
fn buildup_stroke_over_mixed_on_ramp_off_ramp_and_blank_cells_commits_as_one_edit_and_undoes_byte_exact() {
    let mut doc = Document::new(5, 1);
    let ramp = "wxyz"; // no space character: Blank is unambiguously off-ramp here.
    doc.set_cell(0, 0, 0, Cell { ch: 'x', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }); // on-ramp, index 1
    doc.set_cell(0, 1, 0, Cell { ch: '?', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }); // off-ramp glyph
    // (2,0) stays Cell::BLANK: off-ramp (ramp has no space).
    let original = doc.clone();

    let mut history = History::new();
    let tctx = ctx(DensityMode::Buildup(Buildup), ramp, PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut brush = DensityBrush::new();
    stroke(&mut brush, &mut history, &mut doc, &tctx, &[(0, 0), (1, 0), (2, 0)]);

    assert_eq!(doc.cell(0, 0, 0).unwrap().ch, 'y', "on-ramp 'x' (index 1) must advance to index 2");
    assert_eq!(doc.cell(0, 1, 0).unwrap().ch, 'w', "an off-ramp glyph must land on step 0");
    assert_eq!(doc.cell(0, 2, 0).unwrap().ch, 'w', "a Blank cell, off-ramp for a ramp with no space, must also land on step 0");

    assert!(history.can_undo());
    assert!(history.undo(&mut doc));
    assert_eq!(doc, original, "one undo must restore all three cells to their exact pre-stroke values");
    assert!(!history.can_undo(), "the whole mixed-content stroke must be exactly one undo entry");
}

#[test]
fn fixed_intensity_with_a_glyph_only_mask_writes_the_ramp_glyph_but_preserves_the_cells_existing_colors() {
    let mut doc = Document::new(3, 3);
    let existing = Cell { ch: 'Q', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) };
    doc.set_cell(0, 1, 1, existing);
    let glyph_only = PlaneMask { glyph: true, fg: false, bg: false };
    let tctx = ctx(
        DensityMode::Fixed(Fixed(1.0)),
        " .:-=+*#%@",
        glyph_only,
        '#',
        Rgba(255, 0, 0, 255), // deliberately different from the existing colors
        Rgba(0, 0, 255, 255),
    );
    let mut history = History::new();
    let mut brush = DensityBrush::new();
    stroke(&mut brush, &mut history, &mut doc, &tctx, &[(1, 1)]);

    let result = doc.cell(0, 1, 1).unwrap();
    assert_eq!(result.ch, '@', "Fixed(1.0) over a 10-char ramp lands on the last (darkest) glyph");
    assert_eq!(result.fg, existing.fg, "glyph-only mask must preserve the cell's original fg");
    assert_eq!(result.bg, existing.bg, "glyph-only mask must preserve the cell's original bg");
}

#[test]
fn a_brush_stroke_survives_a_save_load_round_trip_and_a_second_stroke_continues_buildup_from_the_persisted_state() {
    let mut doc = Document::new(4, 4);
    let ramp = "abcd";
    let mut history = History::new();
    let tctx = ctx(DensityMode::Buildup(Buildup), ramp, PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut brush1 = DensityBrush::new();
    stroke(&mut brush1, &mut history, &mut doc, &tctx, &[(2, 2)]);
    assert_eq!(doc.cell(0, 2, 2).unwrap().ch, 'a');

    let loaded = load_str(&save_string(&doc)).expect("a document touched by the density brush must save and reload");
    assert_eq!(loaded.cell(0, 2, 2).unwrap().ch, 'a', "the brushed glyph must round-trip exactly");

    let mut doc2 = loaded;
    let mut history2 = History::new();
    let mut brush2 = DensityBrush::new();
    stroke(&mut brush2, &mut history2, &mut doc2, &tctx, &[(2, 2)]);
    assert_eq!(
        doc2.cell(0, 2, 2).unwrap().ch,
        'b',
        "a fresh brush on the reloaded document must continue Buildup from the persisted ramp position, not restart at step 0"
    );
}

#[test]
fn brush_stroke_at_the_far_corner_of_a_1024_square_document_commits_and_undoes_byte_exact() {
    let mut doc = Document::new(1024, 1024);
    let before = doc.clone();
    let mut history = History::new();
    // Fixed(0.5) over a 10-char ramp: nearest index of 0.5*9=4.5 rounds to 5 -> '+'.
    let tctx = ctx(DensityMode::Fixed(Fixed(0.5)), " .:-=+*#%@", PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut brush = DensityBrush::new();
    stroke(&mut brush, &mut history, &mut doc, &tctx, &[(1023, 1023)]);

    assert_eq!(doc.cell(0, 1023, 1023).unwrap().ch, '+');
    assert!(history.undo(&mut doc));
    assert_eq!(doc, before);
}

/// Locks the documented, intentional space-first-ramp nuance (`brush.rs`'s `Buildup` doc comment,
/// and the coder's own flagged implementation-summary deviation note): for the built-in "ASCII
/// shading" ramp, whose own lightest character IS a literal space, a genuinely Blank cell is
/// already on-ramp at index 0 before any touch — so its first Buildup pass lands on index 1 (the
/// ramp's second character), NOT index 0 (which would just leave it looking blank). This is the
/// single highest-stakes new assertion in this file (an easy off-by-one/inverted-special-case bug
/// for a future change to introduce silently) and is bite-proofed below.
#[test]
fn buildup_on_the_built_in_space_first_ascii_ramp_touching_a_blank_cell_advances_past_index_zero_to_index_one() {
    let mut doc = Document::new(3, 3); // fully Cell::BLANK
    assert_eq!(doc.cell(0, 1, 1), Some(&Cell::BLANK), "sanity: the target cell is genuinely Blank before the stroke");

    let ascii_shading = builtin_ramps().into_iter().find(|r| r.name == "ASCII shading").unwrap();
    assert_eq!(ascii_shading.chars[0], ' ', "sanity: the built-in ramp this test targets is genuinely space-first");
    let ramp_str: String = ascii_shading.chars.iter().collect();

    let mut history = History::new();
    let tctx = ctx(DensityMode::Buildup(Buildup), &ramp_str, PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut brush = DensityBrush::new();
    stroke(&mut brush, &mut history, &mut doc, &tctx, &[(1, 1)]);

    assert_eq!(
        doc.cell(0, 1, 1).unwrap().ch,
        '.',
        "a Blank cell touched by Buildup on a space-first ramp must land on index 1 ('.'), not stay at index 0 (' ')"
    );
}

// --- 5. Mixed Edit::Cells/Edit::Resize history stays variant-agnostic ---

#[test]
fn one_action_one_undo_entry_across_pencil_brush_and_resize_interleaved_walks_undo_redo_through_mixed_variant_history() {
    let mut doc = Document::new(6, 6);
    let mut history = History::new();
    let mut snapshots = vec![doc.clone()]; // s0: blank 6x6

    // 1. Pencil stroke (Edit::Cells).
    let pencil_ctx = fixed_ctx(PlaneMask::ALL, 'P', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
    let mut pencil = Pencil::new();
    stroke(&mut pencil, &mut history, &mut doc, &pencil_ctx, &[(0, 0), (1, 0)]);
    snapshots.push(doc.clone());

    // 2. Density brush stroke, Fixed mode (Edit::Cells).
    let brush_ctx = ctx(DensityMode::Fixed(Fixed(1.0)), " .:-=+*#%@", PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut brush = DensityBrush::new();
    stroke(&mut brush, &mut history, &mut doc, &brush_ctx, &[(3, 3)]);
    snapshots.push(doc.clone());

    // 3. Resize grow (Edit::Resize).
    let edit = resize_document(&doc, 10, 10).unwrap().unwrap();
    history.apply(&mut doc, edit);
    snapshots.push(doc.clone());

    // 4. Pencil stroke on the newly grown area (Edit::Cells, on the post-resize doc).
    let mut pencil2 = Pencil::new();
    stroke(&mut pencil2, &mut history, &mut doc, &pencil_ctx, &[(8, 8), (9, 9)]);
    snapshots.push(doc.clone());

    // 5. Resize shrink back down (Edit::Resize), clipping away the just-drawn (8,8)/(9,9) cells.
    let edit = resize_document(&doc, 7, 7).unwrap().unwrap();
    history.apply(&mut doc, edit);
    snapshots.push(doc.clone());

    // 6. A second density brush stroke, Buildup mode (Edit::Cells), on the shrunk doc.
    let buildup_ctx = ctx(DensityMode::Buildup(Buildup), "abcd", PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut brush2 = DensityBrush::new();
    stroke(&mut brush2, &mut history, &mut doc, &buildup_ctx, &[(0, 6)]);
    snapshots.push(doc.clone());

    assert_eq!(snapshots.len(), 7);
    for i in 1..snapshots.len() {
        assert_ne!(snapshots[i], snapshots[i - 1], "action {i} must actually have changed the document");
    }

    // A full undo must walk back through every snapshot in exact reverse order, across both Edit
    // variants interleaved — proving History's apply/undo stays variant-agnostic under a real
    // mixed sequence, not just in edit.rs's own single-variant-at-a-time unit tests.
    for i in (0..snapshots.len() - 1).rev() {
        assert!(history.undo(&mut doc));
        assert_eq!(doc, snapshots[i], "undo did not land on snapshot {i}");
    }
    assert!(!history.can_undo(), "exactly six entries expected across the mixed Cells/Resize sequence");

    // A full redo must walk forward through the same snapshots, including re-applying both resizes.
    for snapshot in snapshots.iter().skip(1) {
        assert!(history.redo(&mut doc));
        assert_eq!(&doc, snapshot);
    }
    assert!(!history.can_redo());
}

#[test]
fn paste_after_a_resize_still_clips_correctly_against_the_new_post_resize_bounds() {
    // A light cross-check that CellPatch clipping (an existing, already-tested behavior) composes
    // correctly with the new post-resize extent, not some stale cached one.
    let mut doc = Document::new(10, 10);
    let mut history = History::new();
    let edit = resize_document(&doc, 4, 4).unwrap().unwrap();
    history.apply(&mut doc, edit);

    let tctx = fixed_ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut sel = SelectionTool::new();
    let patch = CellPatch { width: 3, height: 3, cells: vec![cell('Q'); 9] };
    // Stamp spans (2,2)-(4,4) on a doc whose valid indices only go up to (3,3) post-shrink.
    sel.accept_stamp(patch, (2, 2), &doc);
    let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
    let ToolResponse::Commit(Some(Edit::Cells(cells))) = resp else {
        panic!("expected a clipped write edit");
    };
    assert_eq!(cells.len(), 4, "only the 2x2 in-bounds portion of the 3x3 stamp survives the clip against the new, smaller extent");
    for c in &cells {
        assert!(c.x < 4 && c.y < 4, "no cell may land outside the post-resize extent");
    }
}
