//! End-to-end persistence pipeline: a document built through the real Tool/History pipeline
//! (mixed masks, tools, and a text burst), saved, reloaded, and then edited further — exercised
//! only through the crate's public API. Complements `draw_integration.rs` (drawing) and
//! `io::gascii_json`'s own unit tests (format edge cases) by covering the cross-feature seam
//! neither of those reaches: persistence interacting with the tool pipeline and undo history.

use gascii_core::{
    export_text, load_str, save_string, BrushShape, Cell, DensityMode, Document, Edit, Eraser,
    Fixed, History, Pencil, PlaneMask, Rgba, TextTool, Tool, ToolCtx, ToolEvent, ToolResponse,
};

fn ctx(mask: PlaneMask, glyph: char, fg: Rgba, bg: Rgba) -> ToolCtx {
    ToolCtx { layer: 0, glyph, fg, bg, mask, density: DensityMode::Fixed(Fixed(1.0)), ramp: Vec::new(), size: 1, shape: BrushShape::Square }
}

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

/// Types `text` at `(x, y)` via a real `TextTool` session and commits it as one burst.
fn type_text(doc: &mut Document, history: &mut History, x: u16, y: u16, text: &str, tctx: &ToolCtx) {
    let mut tool = TextTool::new();
    tool.update(ToolEvent::Press { x, y }, tctx, doc);
    for ch in text.chars() {
        tool.update(ToolEvent::Char(ch), tctx, doc);
    }
    if let ToolResponse::Commit(Some(edit)) = tool.update(ToolEvent::Commit, tctx, doc) {
        history.apply(doc, edit);
    }
}

#[test]
fn full_lifecycle_round_trip_through_mixed_tool_pipeline_preserves_document_and_resets_history() {
    let mut doc = Document::new(12, 8);
    let mut history = History::new();

    // 1. Pencil, full mask: paints glyph+fg+bg over a short run.
    let all_mask = ctx(PlaneMask::ALL, '#', Rgba(200, 10, 10, 255), Rgba(50, 50, 50, 255));
    let mut pencil1 = Pencil::new();
    stroke(&mut pencil1, &mut history, &mut doc, &all_mask, &[(0, 0), (1, 0), (2, 0)]);

    // 2. Pencil, full mask, distinct bg at (5,5) — establishes a bg that a later selective stroke
    //    must preserve through save/load.
    let base = ctx(PlaneMask::ALL, 'x', Rgba(9, 9, 9, 255), Rgba(11, 22, 33, 255));
    let mut pencil2 = Pencil::new();
    stroke(&mut pencil2, &mut history, &mut doc, &base, &[(5, 5)]);

    // 3. Pencil, glyph+fg only (bg off): overwrites (5,5)'s glyph/fg but must leave its bg intact.
    let glyph_fg_only = ctx(PlaneMask { glyph: true, fg: true, bg: false }, 'Q', Rgba(1, 2, 3, 255), Rgba(200, 200, 200, 255));
    let mut pencil3 = Pencil::new();
    stroke(&mut pencil3, &mut history, &mut doc, &glyph_fg_only, &[(5, 5)]);
    assert_eq!(doc.cell(0, 5, 5).unwrap().bg, Rgba(11, 22, 33, 255), "sanity: selective stroke preserved bg pre-save");

    // 4. A real TextTool burst: click, type, commit — one undo entry.
    let text_ctx = ctx(PlaneMask::ALL, ' ', Rgba::WHITE, Rgba::TRANSPARENT);
    type_text(&mut doc, &mut history, 0, 3, "Hi", &text_ctx);
    assert_eq!(doc.cell(0, 0, 3).unwrap().ch, 'H');
    assert_eq!(doc.cell(0, 1, 3).unwrap().ch, 'i');

    // 5. Eraser, bg-only: clears just the bg plane of (0,0), leaving its glyph/fg from step 1.
    let bg_only_erase = ctx(PlaneMask { glyph: false, fg: false, bg: true }, ' ', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut eraser = Eraser::new();
    stroke(&mut eraser, &mut history, &mut doc, &bg_only_erase, &[(0, 0)]);
    assert_eq!(doc.cell(0, 0, 0).unwrap().ch, '#', "sanity: bg-only erase left the glyph alone");
    assert_eq!(doc.cell(0, 0, 0).unwrap().bg, Cell::BLANK.bg);

    // Snapshot the fully-built document, then round-trip it.
    let built = doc.clone();
    let json = save_string(&built);
    let loaded = load_str(&json).expect("a document built entirely through the tool pipeline must load back");
    assert_eq!(loaded, built, "loaded document must be byte-for-byte (field-for-field) identical to the one built through the tool pipeline");

    // Continue editing the LOADED document with a fresh History — undo of a post-load edit must
    // restore the loaded state exactly, and there must be no leftover pre-load undo entries.
    let mut doc2 = loaded.clone();
    let mut history2 = History::new();
    assert!(!history2.can_undo(), "a freshly loaded document must start with empty undo history");

    let post_load = ctx(PlaneMask::ALL, '!', Rgba::WHITE, Rgba(1, 1, 1, 255));
    let mut pencil4 = Pencil::new();
    stroke(&mut pencil4, &mut history2, &mut doc2, &post_load, &[(9, 7)]);
    assert_ne!(doc2, loaded, "sanity: the post-load edit changed the document");
    assert!(history2.can_undo());
    assert!(!history2.can_redo());

    assert!(history2.undo(&mut doc2));
    assert_eq!(doc2, loaded, "undo of the one post-load edit must restore the exact loaded state");
    assert!(!history2.can_undo(), "fresh history must have exactly the one entry — no phantom pre-load edits to undo further");
}

#[test]
fn round_trip_preserves_box_drawing_glyphs() {
    let mut doc = Document::new(6, 4);
    let mut history = History::new();
    let box_ctx = ctx(PlaneMask::ALL, '│', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut pencil = Pencil::new();
    stroke(&mut pencil, &mut history, &mut doc, &box_ctx, &[(2, 2)]);
    assert_eq!(doc.cell(0, 2, 2).unwrap().ch, '│');

    let loaded = load_str(&save_string(&doc)).unwrap();
    assert_eq!(loaded.cell(0, 2, 2).unwrap().ch, '│', "non-ASCII content must survive a round trip unstripped");
}

#[test]
fn text_tool_at_the_far_corner_of_a_1024_square_document_stops_at_both_edges_and_commits_one_edit() {
    let doc = Document::new(1024, 1024);
    let mut tool = TextTool::new();
    let tctx = ctx(PlaneMask::ALL, ' ', Rgba::WHITE, Rgba::TRANSPARENT);

    // Click on the last row, one column before the last: types one char to land exactly on the
    // last column, then a second char must be dropped (no wrap) rather than landing at (1024, y)
    // or wrapping to the next row.
    tool.update(ToolEvent::Press { x: 1022, y: 1023 }, &tctx, &doc);
    tool.update(ToolEvent::Char('Y'), &tctx, &doc); // lands at (1022,1023), cursor -> (1023,1023)
    tool.update(ToolEvent::Char('Z'), &tctx, &doc); // lands at (1023,1023), cursor -> (1024,1023) == width, now stopped
    let dropped = tool.update(ToolEvent::Char('!'), &tctx, &doc); // must be a no-op: cursor.x >= width
    assert!(matches!(dropped, ToolResponse::Active));

    // Enter at the last row must go inert (no wrap to a nonexistent row 1024).
    tool.update(ToolEvent::Enter, &tctx, &doc);
    let after_enter = tool.update(ToolEvent::Char('Q'), &tctx, &doc);
    assert!(matches!(after_enter, ToolResponse::Idle), "Enter at the bottom-right corner must go inert, not wrap");

    let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
    let ToolResponse::Commit(Some(Edit::Cells(cells))) = resp else {
        panic!("expected a committed edit with the two in-bounds chars");
    };
    assert_eq!(cells.len(), 2, "only the two in-bounds chars typed before hitting the edge may commit");
    let mut by_pos: Vec<((u16, u16), char)> = cells.iter().map(|c| ((c.x, c.y), c.after.ch)).collect();
    by_pos.sort();
    assert_eq!(by_pos, vec![((1022, 1023), 'Y'), ((1023, 1023), 'Z')]);
}

#[test]
fn interleaved_non_overlapping_text_bursts_survive_a_full_undo_redo_round_trip_then_a_third_overwrite() {
    let mut doc = Document::new(20, 20);
    let mut history = History::new();
    let tctx = ctx(PlaneMask::ALL, ' ', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));

    type_text(&mut doc, &mut history, 0, 0, "ab", &tctx); // burst 1: (0,0)='a', (1,0)='b'
    let s1 = doc.clone();
    type_text(&mut doc, &mut history, 5, 5, "cd", &tctx); // burst 2: (5,5)='c', (6,5)='d'
    let s2 = doc.clone();
    assert_ne!(s1, s2);

    // Full undo walks back through both bursts to blank, in reverse order.
    assert!(history.undo(&mut doc));
    assert_eq!(doc, s1, "first undo must revert only burst 2");
    assert!(history.undo(&mut doc));
    assert!(doc.layers[0].cells().iter().all(Cell::is_blank), "second undo must revert burst 1, restoring a fully blank document");
    assert!(!history.can_undo());

    // Redo walks forward through the same intermediate states.
    assert!(history.redo(&mut doc));
    assert_eq!(doc, s1);
    assert!(history.redo(&mut doc));
    assert_eq!(doc, s2);
    assert!(!history.can_redo());

    // A third, independent burst overwrites a cell burst 2 already committed ((5,5): 'c' -> 'Z'),
    // *after* the full undo/redo round trip above, not interleaved with a pending burst — a
    // different angle on redo/overwrite interaction than the mid-burst-resync regression in
    // draw_integration.rs, which targets a redo racing a still-open burst.
    type_text(&mut doc, &mut history, 5, 5, "Z", &tctx);
    assert_eq!(doc.cell(0, 5, 5).unwrap().ch, 'Z');
    assert!(!history.can_redo(), "a new commit after the round trip must clear any redo stack");

    assert!(history.undo(&mut doc));
    assert_eq!(doc, s2, "undoing the third burst must restore burst 2's committed value ('c'), not burst 1's or Blank");
}

#[test]
fn export_text_composites_across_multiple_layers_before_trimming() {
    // export_text builds on composite(), which is layer-general even though the shipped app only
    // ever writes one layer today (draw_integration.rs's every_builtin_page_glyph... and this
    // crate's own io::mod tests already prove composite() itself is layer-general; this proves
    // export_text specifically doesn't bypass that and flatten only the bottom layer).
    let mut doc = Document::new(4, 2);
    doc.set_cell(0, 0, 0, Cell { ch: 'a', fg: Rgba::WHITE, bg: Rgba(1, 1, 1, 255) });
    doc.set_cell(0, 1, 0, Cell { ch: 'b', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
    doc.layers.push(gascii_core::Layer::blank(4, 2));
    // Top layer fully replaces (0,0) (opaque bg) but leaves (1,0) untouched (Blank on top).
    doc.set_cell(1, 0, 0, Cell { ch: 'X', fg: Rgba::WHITE, bg: Rgba(2, 2, 2, 255) });

    assert_eq!(export_text(&doc), "Xb\n", "row 0 must reflect the top layer's opaque replace at (0,0) and the bottom layer showing through at (1,0); row 1 is untouched/blank");
}
