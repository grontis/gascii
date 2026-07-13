//! End-to-end draw pipeline: pointer gestures -> Tool -> committed Edit -> History -> Document,
//! exercised only through the crate's public API. Complements the per-module unit tests by
//! covering multi-stroke sequences, mixed tools/masks, and adversarial drag paths that no single
//! module's tests combine.

use std::collections::HashSet;

use gascii_core::{
    builtin_pages, line_cells, validate_width, Cell, CellEdit, DensityMode, Document, Edit,
    Eraser, Fixed, History, Pencil, PlaneMask, Rgba, TextTool, Tool, ToolCtx, ToolEvent,
    ToolResponse,
};

fn ctx(mask: PlaneMask, glyph: char, fg: Rgba, bg: Rgba) -> ToolCtx {
    ToolCtx { layer: 0, glyph, fg, bg, mask, density: DensityMode::Fixed(Fixed(1.0)), ramp: Vec::new() }
}

/// Drives a full press -> drag(...) -> release gesture through `tool`, committing the result (if
/// any) into `history`/`doc`. Mirrors the app's pointer-to-Tool lifecycle without any GUI.
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

/// Coordinates touched by a committed `ToolResponse`, as an unordered set — the shape tests care
/// about (contiguity, membership), not push order.
fn commit_coords_of(resp: &ToolResponse) -> HashSet<(u16, u16)> {
    match resp {
        ToolResponse::Commit(Some(gascii_core::Edit::Cells(cells))) => {
            cells.iter().map(|c| (c.x, c.y)).collect()
        }
        _ => HashSet::new(),
    }
}

// --- Undo/redo round-trips from non-trivial, multi-stroke, mixed-mask state ---

#[test]
fn multi_stroke_undo_redo_round_trip_restores_document_byte_identical() {
    let mut doc = Document::new(20, 20);
    // Pre-existing colored background that later glyph/fg-only strokes must never disturb.
    for x in 5..10u16 {
        doc.set_cell(0, x, 5, Cell { ch: ' ', fg: Rgba::WHITE, bg: Rgba(80, 0, 0, 255) });
    }
    let s0 = doc.clone();
    let mut history = History::new();

    let all_mask = ctx(PlaneMask::ALL, '#', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
    let glyph_fg_only_mask = PlaneMask { glyph: true, fg: true, bg: false };
    let glyph_fg_only = ctx(glyph_fg_only_mask, '@', Rgba(200, 0, 0, 255), Rgba(9, 9, 9, 255));
    let eraser_all = ctx(PlaneMask::ALL, ' ', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut pencil1 = Pencil::new();
    stroke(&mut pencil1, &mut history, &mut doc, &all_mask, &[(0, 0), (1, 0), (2, 0), (3, 0)]);
    let s1 = doc.clone();
    assert_ne!(s1, s0, "sanity: stroke 1 changed the document");

    // Overlaps stroke 1 at (2,0)/(3,0), and paints across the pre-colored bg row with a selective
    // glyph+fg (bg-off) mask — must not disturb that bg.
    let mut pencil2 = Pencil::new();
    stroke(
        &mut pencil2,
        &mut history,
        &mut doc,
        &glyph_fg_only,
        &[(2, 0), (3, 0), (4, 0), (5, 5), (6, 5), (7, 5)],
    );
    let s2 = doc.clone();
    assert_ne!(s2, s1, "sanity: stroke 2 changed the document");
    assert_eq!(doc.cell(0, 6, 5).unwrap().bg, Rgba(80, 0, 0, 255), "bg-off stroke must preserve existing bg");

    let mut eraser = Eraser::new();
    stroke(&mut eraser, &mut history, &mut doc, &eraser_all, &[(0, 0), (1, 0)]);
    let s3 = doc.clone();
    assert_ne!(s3, s2, "sanity: stroke 3 changed the document");

    // Each undo reverts exactly one whole stroke, in reverse order.
    assert!(history.undo(&mut doc));
    assert_eq!(doc, s2, "first undo must exactly revert the eraser stroke");
    assert!(history.undo(&mut doc));
    assert_eq!(doc, s1, "second undo must exactly revert stroke 2");
    assert!(history.undo(&mut doc));
    assert_eq!(doc, s0, "third undo must exactly revert stroke 1, restoring the initial document");
    assert!(!history.can_undo());

    // Redo walks forward through the same intermediate states.
    assert!(history.redo(&mut doc));
    assert_eq!(doc, s1);
    assert!(history.redo(&mut doc));
    assert_eq!(doc, s2);
    assert!(history.redo(&mut doc));
    assert_eq!(doc, s3);
    assert!(!history.can_redo());

    // Interleaving: undo once, then commit a brand-new stroke — the redo stack must clear
    // rather than accumulate a stale future.
    assert!(history.undo(&mut doc));
    assert_eq!(doc, s2);
    assert!(history.can_redo());
    let mut pencil3 = Pencil::new();
    stroke(&mut pencil3, &mut history, &mut doc, &all_mask, &[(15, 15)]);
    assert!(!history.can_redo(), "a new commit after undo must clear the redo stack");
}

// --- Stroke commit atomicity ---

#[test]
fn multi_cell_drag_stroke_commits_and_undoes_as_a_single_history_entry() {
    let mut doc = Document::new(30, 30);
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '*', Rgba(1, 1, 1, 255), Rgba(2, 2, 2, 255));

    let mut pencil = Pencil::new();
    // One long diagonal drag: many cells, one Stroke.
    let path: Vec<(u16, u16)> = (0..=19u16).map(|i| (i, i)).collect();
    stroke(&mut pencil, &mut history, &mut doc, &tctx, &path);

    for &(x, y) in &path {
        assert_eq!(doc.cell(0, x, y).unwrap().ch, '*', "cell ({x},{y}) should have been painted");
    }
    assert!(history.can_undo());

    // A single undo call must revert the entire multi-cell stroke at once.
    assert!(history.undo(&mut doc));
    for &(x, y) in &path {
        assert_eq!(doc.cell(0, x, y), Some(&Cell::BLANK), "cell ({x},{y}) should be reverted by the one undo");
    }
    // No second entry left behind: the whole stroke was exactly one history entry.
    assert!(!history.can_undo(), "a single stroke must be exactly one undo entry, not several");
}

// --- PlaneMask end-to-end: overlay preview vs committed document must agree ---

#[test]
fn bg_off_mask_preserves_existing_background_in_both_overlay_and_committed_document() {
    let mut doc = Document::new(10, 10);
    let existing_bg = Rgba(11, 22, 33, 255);
    doc.set_cell(0, 4, 4, Cell { ch: 'x', fg: Rgba(9, 9, 9, 255), bg: existing_bg });

    let mask = PlaneMask { glyph: true, fg: true, bg: false };
    let tctx = ctx(mask, 'Q', Rgba(1, 2, 3, 255), Rgba(200, 200, 200, 255));
    let mut pencil = Pencil::new();

    pencil.update(ToolEvent::Press { x: 4, y: 4 }, &tctx, &doc);
    let pending = pencil.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].cell.bg, existing_bg, "overlay preview must show the preserved bg, not the proposed one");
    assert_eq!(pending[0].cell.ch, 'Q');

    let resp = pencil.update(ToolEvent::Release, &tctx, &doc);
    let ToolResponse::Commit(Some(edit)) = resp else {
        panic!("expected a committed edit");
    };
    let mut history = History::new();
    history.apply(&mut doc, edit);

    let committed = doc.cell(0, 4, 4).unwrap();
    assert_eq!(committed.ch, 'Q');
    assert_eq!(committed.fg, Rgba(1, 2, 3, 255));
    assert_eq!(committed.bg, existing_bg, "committed document must preserve the masked-off bg, matching the overlay");
}

#[test]
fn default_mask_pencil_stroke_fully_replaces_glyph_fg_and_bg_over_existing_cell() {
    // The default plane mask (all planes on) is what a fresh Pencil stroke uses out of the box:
    // drawing over a cell that already has a glyph, fg, and bg must fully replace all three, not
    // just the glyph/fg while leaving a stray old bg behind.
    let mut doc = Document::new(10, 10);
    let existing = Cell { ch: 'x', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) };
    doc.set_cell(0, 4, 4, existing);
    let mut history = History::new();

    let proposed = Cell { ch: 'Q', fg: Rgba(1, 2, 3, 255), bg: Rgba(200, 200, 200, 255) };
    let tctx = ctx(PlaneMask::default(), proposed.ch, proposed.fg, proposed.bg);
    let mut pencil = Pencil::new();

    pencil.update(ToolEvent::Press { x: 4, y: 4 }, &tctx, &doc);
    let pending = pencil.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].cell, proposed, "default-mask overlay preview must fully replace glyph, fg, and bg, not preserve the existing bg");

    let resp = pencil.update(ToolEvent::Release, &tctx, &doc);
    let ToolResponse::Commit(Some(edit)) = resp else {
        panic!("expected a committed edit");
    };
    history.apply(&mut doc, edit);

    let committed = doc.cell(0, 4, 4).unwrap();
    assert_eq!(committed, &proposed, "committed document must fully replace glyph, fg, and bg under the default mask, matching the overlay");

    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell(0, 4, 4), Some(&existing), "undo must restore the exact pre-stroke cell, including its original bg");
}

// --- Eraser semantics across a full paint -> partial erase -> undo chain ---

#[test]
fn bg_only_erase_after_full_paint_leaves_expected_composite_and_undoes_cleanly() {
    let mut doc = Document::new(10, 10);
    let mut history = History::new();

    let paint_ctx = ctx(PlaneMask::ALL, 'Q', Rgba(10, 20, 30, 255), Rgba(40, 50, 60, 255));
    let mut pencil = Pencil::new();
    stroke(&mut pencil, &mut history, &mut doc, &paint_ctx, &[(3, 3)]);
    let painted = doc.clone();
    assert_eq!(doc.cell(0, 3, 3), Some(&Cell { ch: 'Q', fg: Rgba(10, 20, 30, 255), bg: Rgba(40, 50, 60, 255) }));

    let bg_only = PlaneMask { glyph: false, fg: false, bg: true };
    let erase_ctx = ctx(bg_only, ' ', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut eraser = Eraser::new();
    stroke(&mut eraser, &mut history, &mut doc, &erase_ctx, &[(3, 3)]);

    let composite = doc.cell(0, 3, 3).unwrap();
    assert_eq!(composite.ch, 'Q', "glyph must survive a bg-only erase");
    assert_eq!(composite.fg, Rgba(10, 20, 30, 255), "fg must survive a bg-only erase");
    assert_eq!(composite.bg, Cell::BLANK.bg, "bg must be cleared by a bg-only erase");

    assert!(history.undo(&mut doc));
    assert_eq!(doc, painted, "undoing the erase must restore the fully painted cell");
    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell(0, 3, 3), Some(&Cell::BLANK), "undoing the paint must restore Blank");
}

#[test]
fn default_mask_eraser_fully_clears_cell_to_blank() {
    // The default plane mask (all planes on) is the eraser's out-of-the-box behavior most users
    // will hit first: it fully replaces the cell with `Cell::BLANK`, matching a full-replace
    // pencil stroke. Selective (partial) erasing is still available by disabling planes in the
    // mask — see `bg_only_erase_after_full_paint_leaves_expected_composite_and_undoes_cleanly`
    // and `full_plane_erase_over_partially_erased_cell_reaches_true_blank`.
    let mut doc = Document::new(10, 10);
    let existing = Cell { ch: 'x', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) };
    doc.set_cell(0, 1, 1, existing);
    let mut history = History::new();

    let mut eraser = Eraser::new();
    stroke(&mut eraser, &mut history, &mut doc, &ctx(PlaneMask::default(), ' ', Rgba::WHITE, Rgba::TRANSPARENT), &[(1, 1)]);

    let after = doc.cell(0, 1, 1).unwrap();
    assert_eq!(after, &Cell::BLANK, "default-mask erase (all planes on) fully clears the cell");

    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell(0, 1, 1), Some(&existing), "undo must restore the exact pre-erase cell");
}

#[test]
fn full_plane_erase_over_partially_erased_cell_reaches_true_blank() {
    let mut doc = Document::new(10, 10);
    doc.set_cell(0, 2, 2, Cell { ch: 'x', fg: Rgba(1, 2, 3, 255), bg: Rgba(4, 5, 6, 255) });

    let glyph_only = PlaneMask { glyph: true, fg: false, bg: false };
    let mut history = History::new();
    let mut eraser1 = Eraser::new();
    stroke(&mut eraser1, &mut history, &mut doc, &ctx(glyph_only, ' ', Rgba::WHITE, Rgba::TRANSPARENT), &[(2, 2)]);
    let after_glyph_erase = doc.cell(0, 2, 2).unwrap();
    assert_eq!(after_glyph_erase.ch, ' ');
    assert_eq!(after_glyph_erase.fg, Rgba(1, 2, 3, 255), "glyph-only erase must not touch fg");
    assert_eq!(after_glyph_erase.bg, Rgba(4, 5, 6, 255), "glyph-only erase must not touch bg");
    assert!(!after_glyph_erase.is_blank(), "a colored bg survives a glyph-only erase, so the cell is not yet Blank");

    let mut eraser2 = Eraser::new();
    stroke(&mut eraser2, &mut history, &mut doc, &ctx(PlaneMask::ALL, ' ', Rgba::WHITE, Rgba::TRANSPARENT), &[(2, 2)]);
    assert_eq!(doc.cell(0, 2, 2), Some(&Cell::BLANK), "an all-planes erase must reach true Blank");
}

// --- Bresenham under adversarial drags, exercised through the full Tool pipeline ---

#[test]
fn single_cell_click_with_no_drag_yields_exactly_one_cell_edit() {
    let doc = Document::new(20, 20);
    let mut pencil = Pencil::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    pencil.update(ToolEvent::Press { x: 7, y: 7 }, &tctx, &doc);
    let resp = pencil.update(ToolEvent::Release, &tctx, &doc);
    let coords = commit_coords_of(&resp);
    assert_eq!(coords, HashSet::from([(7, 7)]));
}

#[test]
fn long_diagonal_and_perfectly_vertical_fast_drags_leave_no_gaps() {
    let doc = Document::new(60, 60);
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    // A single huge jump (as a fast drag / low-frequency polling would deliver) must still
    // interpolate every intermediate cell, not just stamp press+release endpoints.
    let mut diag = Pencil::new();
    diag.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    diag.update(ToolEvent::Drag { x: 49, y: 49 }, &tctx, &doc);
    let resp = diag.update(ToolEvent::Release, &tctx, &doc);
    let coords = commit_coords_of(&resp);
    let expected: HashSet<(u16, u16)> = (0..=49u16).map(|i| (i, i)).collect();
    assert_eq!(coords, expected, "a single fast diagonal drag must touch every intermediate cell");

    let mut vert = Pencil::new();
    vert.update(ToolEvent::Press { x: 5, y: 0 }, &tctx, &doc);
    vert.update(ToolEvent::Drag { x: 5, y: 29 }, &tctx, &doc);
    let resp = vert.update(ToolEvent::Release, &tctx, &doc);
    let coords = commit_coords_of(&resp);
    let expected: HashSet<(u16, u16)> = (0..=29u16).map(|y| (5, y)).collect();
    assert_eq!(coords, expected, "a single fast vertical drag must touch every intermediate cell");
}

#[test]
fn reversed_stroke_endpoints_touch_the_same_cell_set() {
    let doc = Document::new(30, 20);
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut forward = Pencil::new();
    forward.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
    forward.update(ToolEvent::Drag { x: 15, y: 9 }, &tctx, &doc);
    let resp = forward.update(ToolEvent::Release, &tctx, &doc);
    let forward_coords = commit_coords_of(&resp);

    let mut backward = Pencil::new();
    backward.update(ToolEvent::Press { x: 15, y: 9 }, &tctx, &doc);
    backward.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &doc);
    let resp = backward.update(ToolEvent::Release, &tctx, &doc);
    let backward_coords = commit_coords_of(&resp);

    assert_eq!(forward_coords, backward_coords, "a stroke and its endpoint-reversed twin must touch the same cells");
    assert!(!forward_coords.is_empty());
}

#[test]
fn mid_stroke_target_jump_simulating_a_zoom_change_still_yields_a_contiguous_path() {
    // A zoom step changing mid-drag can make the next Drag's target cell land far from the
    // previous one in a single frame (screen position maps to a very different cell). The tool
    // only ever sees cell coordinates, so this is exercised directly as a big jump between two
    // consecutive Drag events; the resulting path must still be the union of two gap-free
    // straight segments (no cells skipped at the jump boundary).
    let doc = Document::new(100, 100);
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);

    let mut pencil = Pencil::new();
    pencil.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    pencil.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc); // ordinary small drag step
    pencil.update(ToolEvent::Drag { x: 60, y: 3 }, &tctx, &doc); // simulated zoom-induced jump
    let resp = pencil.update(ToolEvent::Release, &tctx, &doc);
    let coords = commit_coords_of(&resp);

    let mut expected = HashSet::new();
    let mut buf = Vec::new();
    line_cells((0, 0), (5, 5), &mut buf);
    expected.extend(buf.iter().copied());
    line_cells((5, 5), (60, 3), &mut buf);
    expected.extend(buf.iter().copied());

    assert_eq!(coords, expected, "the committed stroke must equal the union of both interpolated segments, with no gap at the jump");
}

// --- Palette validation feeding the draw pipeline end-to-end ---

#[test]
fn every_builtin_page_glyph_round_trips_through_a_committed_stroke() {
    let pages = builtin_pages();
    let width = pages.iter().map(|p| p.glyphs.len()).max().unwrap_or(0) as u16;
    let height = pages.len() as u16;
    let mut doc = Document::new(width.max(1), height.max(1));
    let mut history = History::new();

    for (row, page) in pages.iter().enumerate() {
        for (col, &ch) in page.glyphs.iter().enumerate() {
            assert!(validate_width(ch).is_ok(), "built-in glyph {ch:?} in page {:?} must pass validate_width", page.name);
            let tctx = ctx(PlaneMask::ALL, ch, Rgba::WHITE, Rgba::TRANSPARENT);
            let mut pencil = Pencil::new();
            stroke(&mut pencil, &mut history, &mut doc, &tctx, &[(col as u16, row as u16)]);
        }
    }

    for (row, page) in pages.iter().enumerate() {
        for (col, &ch) in page.glyphs.iter().enumerate() {
            let got = doc.cell(0, col as u16, row as u16).unwrap().ch;
            assert_eq!(got, ch, "glyph mismatch at page {:?} index {col}", page.name);
        }
    }
}

// --- Boundary conditions ---

#[test]
fn stroke_spans_full_extent_of_a_tiny_document_corner_to_corner() {
    let mut doc = Document::new(5, 5);
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut pencil = Pencil::new();
    stroke(&mut pencil, &mut history, &mut doc, &tctx, &[(0, 0), (4, 4)]);

    for i in 0..5u16 {
        assert_eq!(doc.cell(0, i, i).unwrap().ch, '#', "diagonal cell ({i},{i}) must be painted");
    }
    assert!(history.can_undo());
    assert!(history.undo(&mut doc));
    assert!(doc.layers[0].cells().iter().all(Cell::is_blank), "undo must restore a fully blank tiny document");
}

#[test]
fn stroke_at_the_far_corner_of_a_1024_square_document_commits_and_undoes() {
    let mut doc = Document::new(1024, 1024);
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut pencil = Pencil::new();
    stroke(&mut pencil, &mut history, &mut doc, &tctx, &[(1020, 1020), (1023, 1023)]);

    for i in 1020..=1023u16 {
        assert_eq!(doc.cell(0, i, i).unwrap().ch, '#', "far-corner diagonal cell ({i},{i}) must be painted");
    }
    // Neighbours just outside the stroke's diagonal must remain untouched (no index-math bleed).
    assert_eq!(doc.cell(0, 1023, 1022), Some(&Cell::BLANK));
    assert_eq!(doc.cell(0, 1022, 1023), Some(&Cell::BLANK));

    assert!(history.undo(&mut doc));
    for i in 1020..=1023u16 {
        assert_eq!(doc.cell(0, i, i), Some(&Cell::BLANK));
    }
}

// --- Redo running while a text burst is pending ---

/// Regression for a stale `before` corrupting `History`'s invariant: a redo that mutates a cell a
/// pending `TextTool` burst has already touched must not leave that burst's pinned `before` value
/// out of sync with `doc`'s real state. Mirrors exactly what `gascii/src/app.rs`'s `request_redo`
/// does — `history.redo` applied directly while the burst stays pending, followed by
/// `Tool::resync` — since `app.rs` has no GUI test harness of its own.
#[test]
fn redo_mid_text_burst_touching_an_already_pinned_cell_keeps_before_accurate_through_flush_and_undo() {
    let mut doc = Document::new(10, 10);
    let mut history = History::new();

    // A prior edit at (5,5), then undone: it now sits on the redo stack, and doc is back to Blank.
    let redo_after = Cell { ch: 'Z', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) };
    history.apply(
        &mut doc,
        Edit::Cells(vec![CellEdit { layer: 0, x: 5, y: 5, before: Cell::BLANK, after: redo_after }]),
    );
    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell(0, 5, 5), Some(&Cell::BLANK));
    assert!(history.can_redo());

    // User switches to Text, clicks (5,5), types 'a' — the burst pins before=Blank (doc's current
    // value at the moment of first touch).
    let mut text = TextTool::new();
    let tctx = ctx(PlaneMask::ALL, 'a', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
    text.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
    text.update(ToolEvent::Char('a'), &tctx, &doc);

    // Without committing, the user presses Redo: doc mutates directly under the still-pending
    // burst, bypassing it entirely. `resync` must re-pin the burst's stale before value.
    assert!(history.redo(&mut doc));
    assert_eq!(doc.cell(0, 5, 5), Some(&redo_after));
    text.resync(&doc, 0);

    let ToolResponse::Commit(Some(edit)) = text.update(ToolEvent::Commit, &tctx, &doc) else {
        panic!("expected a committed edit");
    };
    history.apply(&mut doc, edit);
    assert_eq!(doc.cell(0, 5, 5).unwrap().ch, 'a');

    // Undo round-trips byte-exact: first undo must restore the redo's value (the doc's real
    // pre-flush state), not the stale pre-redo Blank the burst originally pinned; second undo
    // restores the original Blank.
    assert!(history.undo(&mut doc));
    assert_eq!(
        doc.cell(0, 5, 5),
        Some(&redo_after),
        "undo of the flushed text edit must restore the redo's value, not a stale pre-redo Blank"
    );
    assert!(history.undo(&mut doc));
    assert_eq!(doc.cell(0, 5, 5), Some(&Cell::BLANK));
}

#[test]
fn drag_target_beyond_document_bounds_is_dropped_without_panicking_or_corrupting_state() {
    // Guards the FreehandStroke in-bounds check itself, independent of the app-side viewport
    // clamp (`screen_to_cell_clamped`) that normally prevents this from ever reaching a Tool.
    let doc = Document::new(10, 10);
    let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut pencil = Pencil::new();
    pencil.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
    // A wildly out-of-bounds target (as could reach the tool if a future caller skipped
    // clamping) must not panic; cells beyond the doc edge are silently dropped.
    pencil.update(ToolEvent::Drag { x: 9_999, y: 9_999 }, &tctx, &doc);
    let resp = pencil.update(ToolEvent::Release, &tctx, &doc);
    let coords = commit_coords_of(&resp);

    assert!(!coords.is_empty(), "the in-bounds prefix of the drag must still be committed");
    for &(x, y) in &coords {
        assert!(x < 10 && y < 10, "no out-of-bounds cell ({x},{y}) may reach a committed edit");
    }
    // The straight-line path from (5,5) toward (9999,9999) stays in-bounds only through (9,9).
    let expected: HashSet<(u16, u16)> = (5..=9u16).map(|i| (i, i)).collect();
    assert_eq!(coords, expected);
}
