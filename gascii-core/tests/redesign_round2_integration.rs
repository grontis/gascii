//! Cross-feature integration for redesign round 2: `BrushShape::Raw` (the new default) driven
//! through the real sized-tool pipelines (pencil/eraser/line/density brush), not just
//! `footprint()`'s own isolated geometry tests; anchored resize combined with undo/redo and
//! `Document::background`; and `Document::background` carried through resize + save/load +
//! legacy-file compatibility. Complements `feel_integration.rs` (which exercises resize's own
//! pipeline seams but only ever drives it top-left anchored, with Square-shaped sized tools).

use gascii_core::{
    load_str, save_string, AxisAnchor, BrushShape, Buildup, Cell, DensityBrush, DensityMode, Document,
    Edit, Eraser, Fixed, History, Line, Pencil, PlaneMask, ResizeAnchor, Rgba, Tool, ToolCtx, ToolEvent,
    ToolResponse, resize_document,
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
fn raw_shaped_pencil_press_stamps_the_true_size_by_size_box_not_the_aspect_corrected_one() {
    let doc = Document::new(20, 20);
    let mut pencil = Pencil::new();
    let tctx = sized_ctx('#', 3, BrushShape::Raw);
    pencil.update(ToolEvent::Press { x: 10, y: 10 }, &tctx, &doc);
    let cells = commit_cells(pencil.update(ToolEvent::Release, &tctx, &doc));
    assert_eq!(cells.len(), 9, "Raw size-3 press must cover exactly a 3x3 box (9 cells), not Square's 6x3 (18)");
    for c in &cells {
        assert!((9..=11).contains(&c.x) && (9..=11).contains(&c.y), "cell ({},{}) outside the expected 3x3 box", c.x, c.y);
    }
}

#[test]
fn raw_shaped_pencil_press_at_the_corner_clips_to_a_quarter_of_the_size_by_size_box() {
    // Square size-3 clipped at the origin keeps 8 cells (pencil.rs's own
    // `sized_stroke_clips_at_the_document_edge`); Raw's uncorrected 3x3 box keeps only 4.
    let doc = Document::new(20, 20);
    let mut pencil = Pencil::new();
    let tctx = sized_ctx('#', 3, BrushShape::Raw);
    pencil.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
    let cells = commit_cells(pencil.update(ToolEvent::Release, &tctx, &doc));
    assert_eq!(cells.len(), 4, "Raw's uncorrected box clips to a 2x2 quadrant at the origin, not Square's 8");
    for c in &cells {
        assert!(c.x <= 1 && c.y <= 1);
    }
}

#[test]
fn raw_shaped_eraser_press_clears_the_true_size_by_size_box_and_nothing_outside_it() {
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
    assert_eq!(cells.len(), 9, "Raw size-3 erase must clear exactly the 3x3 box, not a wider aspect-corrected one");
    for c in &cells {
        assert!((9..=11).contains(&c.x) && (9..=11).contains(&c.y));
        assert_eq!(c.after, Cell::BLANK);
    }
    // Cells just outside the 3x3 box (e.g. the painted region's own edge) must survive untouched.
    assert_eq!(doc.cell(0, 8, 8), Some(&Cell { ch: 'Q', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) }));
}

#[test]
fn raw_shaped_thick_line_sweeps_a_narrower_band_than_square_and_still_never_joins() {
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
    // Raw's uncorrected 3-row-by-3-col footprint swept along a 21-cell horizontal run covers a
    // 23-wide x 3-tall band (cols 4..=26, rows 14..=16) = 69 cells -- narrower than Square's
    // 6-wide-per-step aspect-corrected sweep, which would cover a much wider band.
    let xs: std::collections::HashSet<u16> = cells.iter().map(|c| c.x).collect();
    let ys: std::collections::HashSet<u16> = cells.iter().map(|c| c.y).collect();
    assert_eq!(*xs.iter().min().unwrap(), 4);
    assert_eq!(*xs.iter().max().unwrap(), 26);
    assert_eq!(ys, [14u16, 15, 16].into_iter().collect());
    assert_eq!(cells.len(), 23 * 3);
}

#[test]
fn raw_shaped_wide_buildup_drag_advances_the_narrower_true_size_band_exactly_once_per_pass() {
    // Mirrors density_brush.rs's own `wide_buildup_drag_advances_each_covered_cell_exactly_once_per_pass`
    // (which pins Square's wider 0..=11 aspect-corrected band) but for Raw -- the leading-edge-only
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

    // Raw's uncorrected footprint sweeps cols 1..=9, rows 1..=3 (narrower than Square's 0..=11).
    for y in 1..=3u16 {
        for x in 1..=9u16 {
            assert_eq!(doc.cell(0, x, y).unwrap().ch, 'a', "cell ({x},{y}) must advance exactly once");
        }
    }
    // Just outside the Raw band (still inside Square's wider band) must be untouched.
    assert_eq!(doc.cell(0, 0, 2), Some(&Cell::BLANK), "col 0 is outside Raw's narrower footprint");
    assert_eq!(doc.cell(0, 10, 2), Some(&Cell::BLANK), "col 10 is outside Raw's narrower footprint");
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
