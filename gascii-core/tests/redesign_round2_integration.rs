//! Cross-feature integration for redesign round 2: `BrushShape::Raw` (the new default, a
//! horizontal run) driven through the real sized-tool pipelines (pencil/eraser/line/density
//! brush), not just `footprint()`'s own isolated geometry tests; anchored resize combined with
//! undo/redo and `Document::background`; and `Document::background` carried through resize +
//! save/load + legacy-file compatibility. Complements `feel_integration.rs` (which exercises
//! resize's own pipeline seams but only ever drives it top-left anchored, with Square-shaped sized
//! tools).

use gascii_core::{
    footprint, load_str, save_string, AxisAnchor, BrushShape, Buildup, Cell, DensityBrush, DensityMode,
    Document, Edit, Eraser, Fixed, History, Line, Pencil, PlaneMask, ResizeAnchor, Rgba, Tool, ToolCtx,
    ToolEvent, ToolResponse, resize_document,
};

fn sized_ctx(glyph: char, size: u16, shape: BrushShape) -> ToolCtx {
    ToolCtx {
        layer: 0,
        glyph,
        fg: Rgba::WHITE,
        bg: Rgba::TRANSPARENT,
        mask: PlaneMask::ALL,
        density: DensityMode::Fixed(Fixed(1.0)),
        ramp: Vec::new(),
        size,
        shape,
    }
}

fn commit_cells(resp: ToolResponse) -> Vec<gascii_core::CellEdit> {
    match resp {
        ToolResponse::Commit(Some(Edit::Cells(cells))) => cells,
        other => panic!("expected a committed Edit::Cells, got {other:?}"),
    }
}

// --- 1. BrushShape::Raw x each sized tool, through the real Press/Drag/Release pipeline ---
// (footprint()'s own unit tests in tools/mod.rs pin the isolated geometry; these confirm the same
// geometry survives the actual tool/FreehandStroke pipeline, and specifically that it differs from
// the aspect-corrected Square/Circle shapes every existing sized-tool test exercises instead.)

#[test]
fn raw_shaped_pencil_press_stamps_a_horizontal_run_not_the_aspect_corrected_box() {
    let doc = Document::new(20, 20);
    let mut pencil = Pencil::new();
    let tctx = sized_ctx('#', 3, BrushShape::Raw);
    pencil.update(ToolEvent::Press { x: 10, y: 10 }, &tctx, &doc);
    let cells = commit_cells(pencil.update(ToolEvent::Release, &tctx, &doc));
    assert_eq!(cells.len(), 3, "Raw size-3 press must cover exactly a 1x3 horizontal run, not Square's 3x6 box");
    for c in &cells {
        assert!((9..=11).contains(&c.x) && c.y == 10, "cell ({},{}) outside the expected 1x3 run", c.x, c.y);
    }
}

#[test]
fn raw_shaped_pencil_press_at_the_corner_clips_the_run_to_the_grid() {
    // Raw's single row has no vertical extent to clip at the origin, only the row's left edge;
    // Square's box clips on both axes (pencil.rs's own `sized_stroke_clips_at_the_document_edge`).
    let doc = Document::new(20, 20);
    let mut pencil = Pencil::new();
    let tctx = sized_ctx('#', 3, BrushShape::Raw);
    pencil.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    let cells = commit_cells(pencil.update(ToolEvent::Release, &tctx, &doc));
    assert_eq!(cells.len(), 2, "Raw's 1x3 run clips to 2 cells at the left edge, not Square's 8");
    for c in &cells {
        assert!(c.x <= 1 && c.y == 0);
    }
}

#[test]
fn raw_shaped_eraser_press_clears_a_horizontal_run_and_nothing_outside_it() {
    let mut doc = Document::new(20, 20);
    for y in 8..13u16 {
        for x in 8..13u16 {
            doc.set_cell(0, x, y, Cell { ch: 'Q', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) });
        }
    }
    let mut eraser = Eraser::new();
    let tctx = sized_ctx('#', 3, BrushShape::Raw);
    eraser.update(ToolEvent::Press { x: 10, y: 10 }, &tctx, &doc);
    let cells = commit_cells(eraser.update(ToolEvent::Release, &tctx, &doc));
    assert_eq!(cells.len(), 3, "Raw size-3 erase must clear exactly the 1x3 run, not a wider aspect-corrected box");
    for c in &cells {
        assert!((9..=11).contains(&c.x) && c.y == 10);
        assert_eq!(c.after, Cell::BLANK);
    }
    // Rows above/below the erased run must survive untouched.
    assert_eq!(doc.cell(0, 10, 9), Some(&Cell { ch: 'Q', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) }));
    assert_eq!(doc.cell(0, 10, 11), Some(&Cell { ch: 'Q', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) }));
}

#[test]
fn raw_shaped_thick_line_sweeps_a_single_row_and_still_never_joins() {
    let mut doc = Document::new(30, 30);
    // Existing vertical box-drawing run the line would join against at size 1, to prove size>1
    // (Raw included) still stamps the glyph directly with no join, matching `line.rs`'s own
    // `thick_line_stamps_the_glyph_directly_with_no_join_and_no_duplicates` (which uses Square).
    for y in 0..30u16 {
        doc.set_cell(0, 15, y, Cell { ch: '│', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
    }
    let tctx = sized_ctx('#', 3, BrushShape::Raw);
    let mut line = Line::new();
    line.update(ToolEvent::Press { x: 5, y: 15 }, &tctx, &doc);
    line.update(ToolEvent::Drag { x: 25, y: 15 }, &tctx, &doc);
    let cells = commit_cells(line.update(ToolEvent::Release, &tctx, &doc));

    assert!(cells.iter().all(|c| c.after.ch == '#'), "size>1 Raw must stamp the glyph directly, never join");
    // Raw's footprint has no vertical extent, so the swept run stays a single row (y=15) widened
    // by 1 cell on each end (the first/last stamp's own 3-wide run) -- narrower than Square's
    // multi-row aspect-corrected sweep.
    let xs: std::collections::HashSet<u16> = cells.iter().map(|c| c.x).collect();
    let ys: std::collections::HashSet<u16> = cells.iter().map(|c| c.y).collect();
    assert_eq!(*xs.iter().min().unwrap(), 4);
    assert_eq!(*xs.iter().max().unwrap(), 26);
    assert_eq!(ys, [15u16].into_iter().collect());
    assert_eq!(cells.len(), 23);
}

#[test]
fn raw_shaped_wide_buildup_drag_advances_the_single_row_exactly_once_per_pass() {
    // Mirrors density_brush.rs's own `wide_buildup_drag_advances_each_covered_cell_exactly_once_per_pass`
    // (which pins Square's wider multi-row aspect-corrected band) but for Raw -- the leading-edge-only
    // advance logic (`prev_fp` masking) is shape-dependent, so this is a genuinely different seam,
    // not just a relabeled duplicate: a regression that silently widened Raw's footprint back to
    // Square's geometry would still pass the Square-shaped test but fail this one.
    let mut doc = Document::new(20, 20);
    let mut tctx = sized_ctx('#', 3, BrushShape::Raw);
    tctx.density = DensityMode::Buildup(Buildup);
    tctx.ramp = "abcd".chars().collect();
    let mut brush = DensityBrush::new();
    brush.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
    for x in 3..=8u16 {
        brush.update(ToolEvent::Drag { x, y: 2 }, &tctx, &doc);
    }
    let resp = brush.update(ToolEvent::Release, &tctx, &doc);
    let ToolResponse::Commit(Some(edit)) = resp else { panic!("expected a committed edit") };
    let mut history = History::new();
    history.apply(&mut doc, edit);

    // Raw's footprint has no vertical extent, so only row y=2 (cols 1..=9) advances.
    for x in 1..=9u16 {
        assert_eq!(doc.cell(0, x, 2).unwrap().ch, 'a', "cell ({x},2) must advance exactly once");
    }
    // Rows above/below the single swept row (inside Square's wider band) must be untouched.
    assert_eq!(doc.cell(0, 5, 1), Some(&Cell::BLANK), "row 1 is outside Raw's single-row footprint");
    assert_eq!(doc.cell(0, 5, 3), Some(&Cell::BLANK), "row 3 is outside Raw's single-row footprint");
}

#[test]
fn raw_shaped_footprint_directly_at_sizes_4_and_5_is_still_a_single_row_of_exactly_size_cells() {
    // footprint()'s own unit tests in tools/mod.rs only pin Raw at sizes 1, 2, and 3; this confirms
    // the horizontal-run geometry holds at two sizes those tests never exercise -- size 4 (even,
    // right-biased like size 2) and size 5 (odd, symmetric like size 3) -- via the same public path
    // the app crate calls (`gascii_core::footprint`), not just through a tool pipeline.
    let mut out = Vec::new();

    footprint((10, 10), 4, BrushShape::Raw, &mut out);
    assert_eq!(
        out.iter().copied().collect::<std::collections::HashSet<_>>(),
        [(9, 10), (10, 10), (11, 10), (12, 10)].into_iter().collect(),
        "size 4 Raw must be a single right-biased row of exactly 4 cells"
    );
    assert_eq!(out.len(), 4);

    footprint((10, 10), 5, BrushShape::Raw, &mut out);
    assert_eq!(
        out.iter().copied().collect::<std::collections::HashSet<_>>(),
        [(8, 10), (9, 10), (10, 10), (11, 10), (12, 10)].into_iter().collect(),
        "size 5 Raw must be a single symmetric row of exactly 5 cells"
    );
    assert_eq!(out.len(), 5);
}

#[test]
fn raw_shaped_pencil_press_size_4_at_the_bottom_right_grid_corner_clips_the_run_to_the_grid() {
    // Mirrors `raw_shaped_pencil_press_at_the_corner_clips_the_run_to_the_grid` (size 3, top-left
    // origin) but at an even size and the opposite corner, so it's the right-bias pinned by
    // `footprint_raw_even_size_biases_right` that gets clipped here, not a symmetric overhang.
    let doc = Document::new(20, 20);
    let mut pencil = Pencil::new();
    let tctx = sized_ctx('#', 4, BrushShape::Raw);
    pencil.update(ToolEvent::Press { x: 19, y: 19 }, &tctx, &doc);
    let cells = commit_cells(pencil.update(ToolEvent::Release, &tctx, &doc));
    // hlo=-1, hhi=2 around x=19 -> x in 18..=21; 20 and 21 fall outside the 20-wide document.
    assert_eq!(cells.len(), 2, "Raw's size-4 run clips to 2 cells at the right document edge");
    for c in &cells {
        assert!((18..=19).contains(&c.x) && c.y == 19, "cell ({},{}) outside the expected clipped run", c.x, c.y);
    }
}

#[test]
fn raw_shaped_eraser_size_5_press_near_the_document_edge_clips_the_run_but_never_the_row() {
    // A near-edge clip case at a size the existing raw_shaped_* tests don't cover (5, odd, wider
    // than any prior case here). Raw's footprint has no vertical extent, so only the overhanging
    // column drops -- the rows above/below stay fully untouched, unlike Square/Circle, which would
    // also lose cells vertically at the same document edge.
    let mut doc = Document::new(20, 20);
    for y in 14..=16u16 {
        for x in 14..20u16 {
            doc.set_cell(0, x, y, Cell { ch: 'Q', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) });
        }
    }
    let mut eraser = Eraser::new();
    let tctx = sized_ctx('#', 5, BrushShape::Raw);
    eraser.update(ToolEvent::Press { x: 18, y: 15 }, &tctx, &doc);
    let cells = commit_cells(eraser.update(ToolEvent::Release, &tctx, &doc));
    // hlo=-2, hhi=2 around x=18 -> x in 16..=20; 20 falls outside the 20-wide document.
    assert_eq!(cells.len(), 4, "Raw's size-5 run clips to 4 cells at the right document edge");
    for c in &cells {
        assert!((16..=19).contains(&c.x) && c.y == 15);
        assert_eq!(c.after, Cell::BLANK);
    }
    // Rows above/below the erased row must survive untouched, including their own edge-adjacent
    // cells -- proving the clip is horizontal-only, not a footprint that also lost vertical extent.
    for x in 14..20u16 {
        assert_eq!(doc.cell(0, x, 14), Some(&Cell { ch: 'Q', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) }));
        assert_eq!(doc.cell(0, x, 16), Some(&Cell { ch: 'Q', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) }));
    }
}

// --- 2. Anchored (non-Start) resize x undo/redo x Document::background ---
// (resize.rs's own unit tests cover the anchor math and Start-anchored undo through `History`
// directly; feel_integration.rs's resize tests are always Start-anchored. Neither touches
// `Document::background`, which `resize_document` never reads or writes -- these confirm that
// holds through a real undo/redo round trip for every non-Start anchor combination, not just by
// code inspection.)

#[test]
fn every_non_start_anchor_preserves_a_custom_background_through_resize_and_undo_redo() {
    for (h, v) in [
        (AxisAnchor::Center, AxisAnchor::Start),
        (AxisAnchor::End, AxisAnchor::Start),
        (AxisAnchor::Start, AxisAnchor::Center),
        (AxisAnchor::Center, AxisAnchor::Center),
        (AxisAnchor::End, AxisAnchor::Center),
        (AxisAnchor::Start, AxisAnchor::End),
        (AxisAnchor::Center, AxisAnchor::End),
        (AxisAnchor::End, AxisAnchor::End),
    ] {
        let mut doc = Document::new(4, 4);
        doc.background = Rgba(10, 20, 30, 255);
        doc.set_cell(0, 1, 1, Cell { ch: 'm', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        let mut history = History::new();
        let anchor = ResizeAnchor { h, v };

        let grow = resize_document(&doc, 9, 9, anchor).unwrap().unwrap();
        history.apply(&mut doc, grow);
        assert_eq!(doc.background, Rgba(10, 20, 30, 255), "h={h:?} v={v:?}: grow must not touch background");

        assert!(history.undo(&mut doc));
        assert_eq!(doc.background, Rgba(10, 20, 30, 255), "h={h:?} v={v:?}: undo must not touch background");
        assert_eq!((doc.width, doc.height), (4, 4));

        assert!(history.redo(&mut doc));
        assert_eq!(doc.background, Rgba(10, 20, 30, 255), "h={h:?} v={v:?}: redo must not touch background");
        assert_eq!((doc.width, doc.height), (9, 9));
    }
}

// --- 3. Document::background x resize x save/load, and legacy-file (no background field) x resize ---

#[test]
fn a_center_anchored_resize_then_save_load_round_trips_a_custom_background_exactly() {
    let mut doc = Document::new(3, 3);
    doc.background = Rgba(200, 50, 5, 255);
    doc.set_cell(0, 1, 1, Cell { ch: 'z', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
    let anchor = ResizeAnchor { h: AxisAnchor::Center, v: AxisAnchor::Center };
    let edit = resize_document(&doc, 7, 7, anchor).unwrap().unwrap();
    let mut history = History::new();
    history.apply(&mut doc, edit);

    let loaded = load_str(&save_string(&doc)).expect("a background-carrying resized doc must save and reload");
    assert_eq!(loaded, doc, "byte-exact round trip, background included");
    assert_eq!(loaded.background, Rgba(200, 50, 5, 255));
}

#[test]
fn a_legacy_file_with_no_background_field_loads_as_opaque_black_then_survives_an_anchored_resize() {
    // Simulates opening a document saved by a build that predates `Document::background`, then
    // immediately resizing it with a non-Start anchor -- the full "old file meets new feature"
    // pipeline, not just the loader's own isolated legacy-field test in `io/gascii_json.rs`.
    let doc = Document::new(4, 4);
    let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
    value.as_object_mut().unwrap().remove("background");
    let json = serde_json::to_string(&value).unwrap();
    let mut loaded = load_str(&json).expect("a pre-background file must still load");
    assert_eq!(loaded.background, Rgba(0, 0, 0, 255), "sanity: legacy file defaults to opaque black");

    let anchor = ResizeAnchor { h: AxisAnchor::End, v: AxisAnchor::Center };
    let edit = resize_document(&loaded, 8, 6, anchor).unwrap().unwrap();
    let mut history = History::new();
    history.apply(&mut loaded, edit);
    assert_eq!(loaded.background, Rgba(0, 0, 0, 255), "resize must not disturb the legacy-defaulted background");
    assert_eq!((loaded.width, loaded.height), (8, 6));

    let round_tripped = load_str(&save_string(&loaded)).unwrap();
    assert_eq!(round_tripped, loaded, "the now-resized, previously-legacy document must still round-trip byte-exact");
}
