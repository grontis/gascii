//! End-to-end soundness of `History::top_edit_id()` as a "has anything changed since the last
//! save/open" marker, driven through the real Tool/History pipeline (not synthetic `CellEdit`s
//! constructed by hand) — the property `gascii/src/app.rs`'s `saved_marker`/`is_dirty` depends on
//! for the dirty-close-confirm feature. Complements `gascii-core/src/edit.rs`'s own unit tests
//! (which exercise `top_edit_id()` against hand-built `Edit`s in isolation) by proving the same
//! identity-not-depth property holds under a realistic mixed-tool session: pencil strokes, a text
//! burst, and an eraser stroke, interleaved with undo/redo — the exact shape of use this feature's
//! close-interception logic actually sees. `gascii/src/app.rs`'s `edit_marker_differs` itself
//! (`current != saved`) is a one-line pure function already unit-tested directly in `app.rs`; this
//! file inlines that same comparison (`top_edit_id() == captured_marker`) rather than depending on
//! the `gascii` binary crate, since `gascii-core` cannot depend on its own downstream consumer.

use gascii_core::{
    BrushShape, DensityMode, Document, Eraser, Fixed, History, Pencil, PlaneMask, Rgba, TextTool,
    Tool, ToolCtx, ToolEvent, ToolResponse,
};

fn ctx(mask: PlaneMask, glyph: char, fg: Rgba, bg: Rgba) -> ToolCtx {
    ToolCtx {
        layer: 0,
        glyph,
        fg,
        bg,
        mask,
        density: DensityMode::Fixed(Fixed(1.0)),
        ramp: Vec::new(),
        size: 1,
        shape: BrushShape::Square,
    }
}

/// Drives a full press -> drag(...) -> release gesture through `tool`, committing the result (if
/// any) into `history`/`doc`. Mirrors `gascii/src/canvas.rs`'s real pointer-to-Tool lifecycle
/// without any GUI — same helper shape as `draw_integration.rs`/`persist_integration.rs`.
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

/// Types `text` at `(x, y)` via a real `TextTool` session and commits it as one burst — mirrors
/// `gascii/src/app.rs`'s `flush_active_tool` committing a pending burst before the dirty check in
/// `handle_close_request`.
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
fn undoing_back_to_a_saved_checkpoint_across_a_mixed_tool_session_reports_clean_via_top_edit_id() {
    let mut doc = Document::new(20, 10);
    let mut history = History::new();
    let all_mask = ctx(PlaneMask::ALL, '#', Rgba(200, 10, 10, 255), Rgba(50, 50, 50, 255));

    // A realistic session: pencil, text burst, pencil again.
    let mut pencil1 = Pencil::new();
    stroke(&mut pencil1, &mut history, &mut doc, &all_mask, &[(0, 0), (1, 0)]);
    type_text(&mut doc, &mut history, 5, 5, "Hi", &all_mask);

    // Simulate a successful save: the app records `saved_marker = history.top_edit_id()` here.
    let saved_marker = history.top_edit_id();
    assert!(saved_marker.is_some(), "a session with real committed edits must have a Some marker");

    // More work happens after the save: a third stroke, then an eraser stroke.
    let mut pencil2 = Pencil::new();
    stroke(&mut pencil2, &mut history, &mut doc, &all_mask, &[(10, 9)]);
    let bg_only_erase = ctx(PlaneMask { glyph: false, bg: true }, ' ', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut eraser = Eraser::new();
    stroke(&mut eraser, &mut history, &mut doc, &bg_only_erase, &[(0, 0)]);
    assert_ne!(history.top_edit_id(), saved_marker, "post-save edits must diverge from the saved marker");

    // Undo both post-save edits, landing exactly back on the saved checkpoint.
    assert!(history.undo(&mut doc));
    assert!(history.undo(&mut doc));
    assert_eq!(
        history.top_edit_id(),
        saved_marker,
        "undoing back to exactly the saved edit must restore the exact same marker — the core \
         regression this feature exists to get right (no spurious close-confirm prompt)"
    );
}

#[test]
fn a_new_edit_after_undo_at_the_same_stack_depth_as_the_saved_edit_still_reports_dirty() {
    // The request's own pitfall, driven through the real Tool pipeline rather than hand-built
    // `Edit`s: save at depth 2, undo once (depth 1), draw something new (back to depth 2) — the
    // marker must NOT coincidentally match just because the stack depth matches again.
    let mut doc = Document::new(20, 10);
    let mut history = History::new();
    let all_mask = ctx(PlaneMask::ALL, '#', Rgba(200, 10, 10, 255), Rgba(50, 50, 50, 255));

    let mut pencil1 = Pencil::new();
    stroke(&mut pencil1, &mut history, &mut doc, &all_mask, &[(0, 0)]);
    let mut pencil2 = Pencil::new();
    stroke(&mut pencil2, &mut history, &mut doc, &all_mask, &[(1, 0)]);

    // Simulate save at depth 2.
    let saved_marker = history.top_edit_id();

    // Undo once (depth 1), then draw a genuinely different stroke, landing back at depth 2.
    assert!(history.undo(&mut doc));
    let mut pencil3 = Pencil::new();
    stroke(&mut pencil3, &mut history, &mut doc, &all_mask, &[(2, 0)]);

    assert_ne!(
        history.top_edit_id(),
        saved_marker,
        "a different edit landing at the same stack depth as the saved marker must still be \
         reported dirty — stack depth alone is not a sound identity"
    );
}

#[test]
fn undoing_below_the_saved_checkpoint_then_redoing_back_up_to_it_restores_the_exact_saved_marker() {
    let mut doc = Document::new(20, 10);
    let mut history = History::new();
    let all_mask = ctx(PlaneMask::ALL, '#', Rgba(200, 10, 10, 255), Rgba(50, 50, 50, 255));

    let mut pencil1 = Pencil::new();
    stroke(&mut pencil1, &mut history, &mut doc, &all_mask, &[(0, 0)]);
    let mut pencil2 = Pencil::new();
    stroke(&mut pencil2, &mut history, &mut doc, &all_mask, &[(1, 0)]);

    // Simulate a save right after the second stroke.
    let saved_marker = history.top_edit_id();

    // A third, post-save edit pushes the stack one deeper.
    let mut pencil3 = Pencil::new();
    stroke(&mut pencil3, &mut history, &mut doc, &all_mask, &[(2, 0)]);
    assert_ne!(history.top_edit_id(), saved_marker);

    // Undo twice — past the third edit AND past the saved checkpoint itself, down to the first
    // edit — then redo exactly once, landing back precisely on the saved checkpoint (not redoing
    // the third, post-save edit as well).
    assert!(history.undo(&mut doc));
    assert!(history.undo(&mut doc));
    assert!(history.redo(&mut doc));
    assert_eq!(
        history.top_edit_id(),
        saved_marker,
        "undoing past the saved checkpoint and redoing back up exactly to it must restore the \
         exact same marker — the same id, not merely 'some' edit that happens to sit at the \
         right depth"
    );
}

#[test]
fn a_freshly_opened_document_reports_a_none_marker_matching_a_brand_new_history() {
    // Mirrors `gascii/src/app.rs::open_file`'s success branch: `self.history = History::new();
    // self.saved_marker = self.history.top_edit_id();` — reading from the fresh History rather
    // than hardcoding None. Proves that path and a genuinely untouched document agree.
    let opened_doc = Document::new(15, 15);
    let fresh_history_after_open = History::new();
    assert_eq!(
        fresh_history_after_open.top_edit_id(),
        None,
        "a freshly opened document's marker must be None, matching an untouched document"
    );
    // Sanity: the untouched document itself has no undo entries to walk.
    assert!(!fresh_history_after_open.can_undo());
    let _ = opened_doc; // only used to make the "opened document" framing explicit
}

#[test]
fn saving_with_no_prior_edits_then_drawing_then_undoing_the_only_edit_returns_to_the_initial_none_marker() {
    // The "brand-new/untitled document is clean by construction" edge case from the architect
    // plan: `saved_marker == None` on a fresh app agrees with a fresh `History::new()`. This test
    // proves the property survives a single edit + undo round trip, not just at t=0.
    let mut doc = Document::new(8, 8);
    let mut history = History::new();
    let saved_marker_at_launch = history.top_edit_id();
    assert_eq!(saved_marker_at_launch, None);

    let all_mask = ctx(PlaneMask::ALL, 'x', Rgba::WHITE, Rgba::TRANSPARENT);
    let mut pencil = Pencil::new();
    stroke(&mut pencil, &mut history, &mut doc, &all_mask, &[(3, 3)]);
    assert_ne!(history.top_edit_id(), saved_marker_at_launch);

    assert!(history.undo(&mut doc));
    assert_eq!(
        history.top_edit_id(),
        saved_marker_at_launch,
        "undoing the only edit back to an empty stack must restore the exact initial None marker"
    );
}
