//! Rectangular selection: marquee -> lift into a floating stamp -> move -> drop. The document is
//! never mutated while a stamp floats — drop reads the document's *current* state and produces one
//! combined `Edit` covering both the vacated source and the written destination, so a whole
//! marquee-lift-move-drop gesture is exactly one undo entry (matching `TextTool`'s one-burst-one-
//! entry contract) and a mid-float external mutation is automatically accounted for with no
//! `resync` needed — `before` is read at drop time, never pinned at lift time.

use std::collections::HashMap;

use super::{CellRect, PendingCell, SelectionView, Tool, ToolCtx, ToolEvent, ToolResponse};
use crate::clipboard::CellPatch;
use crate::edit::{CellEdit, Edit};
use crate::model::{Cell, Document};

/// Lifted or pasted cells hovering at `dest`, not yet written into the document. Move and paste
/// transplant whole cells (glyph+fg+bg) — the plane mask is deliberately not consulted here, so
/// moving content never silently drops its colors.
struct FloatingStamp {
    patch: CellPatch,
    /// The region this stamp was lifted from, if it came from a move (not a paste). Drop blanks
    /// whatever part of this region the shifted destination doesn't already cover.
    source: Option<CellRect>,
    dest: (i32, i32),
}

impl FloatingStamp {
    /// Current on-screen extent, clamped to non-negative cell coordinates (a stamp dragged off the
    /// top/left edge still needs a renderable outline).
    fn rect(&self) -> CellRect {
        CellRect {
            x0: self.dest.0.max(0) as u16,
            y0: self.dest.1.max(0) as u16,
            x1: (self.dest.0 + self.patch.width as i32 - 1).max(0) as u16,
            y1: (self.dest.1 + self.patch.height as i32 - 1).max(0) as u16,
        }
    }

    fn contains(&self, x: u16, y: u16) -> bool {
        let (px, py) = (x as i32, y as i32);
        px >= self.dest.0
            && py >= self.dest.1
            && px < self.dest.0 + self.patch.width as i32
            && py < self.dest.1 + self.patch.height as i32
    }

    fn pending_cells(&self) -> Vec<PendingCell> {
        let mut out = Vec::new();
        for row in 0..self.patch.height {
            for col in 0..self.patch.width {
                let dx = self.dest.0 + col as i32;
                let dy = self.dest.1 + row as i32;
                if dx < 0 || dy < 0 {
                    continue;
                }
                let idx = row as usize * self.patch.width as usize + col as usize;
                out.push(PendingCell { x: dx as u16, y: dy as u16, cell: self.patch.cells[idx] });
            }
        }
        out
    }

    /// Builds the single combined `Edit` for dropping this stamp: destination cells (clipped to
    /// the document; on overlap, the destination write wins) plus any source cell the shifted
    /// destination doesn't already cover (blanked). The stamp's full rectangle replaces the
    /// destination rectangle — a Blank patch cell overwrites the destination cell it lands on
    /// rather than revealing whatever was underneath, matching how an ordinary stroke's Blank
    /// write also replaces rather than composites. `before` is read from `doc` right now — a
    /// mid-float external mutation is reflected automatically, with nothing to resync.
    fn to_edit(&self, doc: &Document, layer: usize) -> Option<Edit> {
        let mut touched: HashMap<(u16, u16), (Cell, Cell)> = HashMap::new();

        for row in 0..self.patch.height {
            for col in 0..self.patch.width {
                let dx = self.dest.0 + col as i32;
                let dy = self.dest.1 + row as i32;
                if dx < 0 || dy < 0 || dx >= doc.width as i32 || dy >= doc.height as i32 {
                    continue; // off-canvas destination cell: clipped, dropped
                }
                let (x, y) = (dx as u16, dy as u16);
                let idx = row as usize * self.patch.width as usize + col as usize;
                let after = self.patch.cells[idx];
                let before = doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK);
                touched.insert((x, y), (before, after));
            }
        }
        if let Some(src) = self.source {
            for y in src.y0..=src.y1 {
                for x in src.x0..=src.x1 {
                    // A destination write already claimed this cell (an overlap) — it wins over
                    // the source's blank, per the drop's overlap-merge contract.
                    touched.entry((x, y)).or_insert_with(|| {
                        let before = doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK);
                        (before, Cell::BLANK)
                    });
                }
            }
        }

        let mut cell_edits: Vec<CellEdit> = touched
            .into_iter()
            .filter(|&(_, (before, after))| before != after)
            .map(|((x, y), (before, after))| CellEdit { layer, x, y, before, after })
            .collect();
        cell_edits.sort_by_key(|c| (c.y, c.x));
        (!cell_edits.is_empty()).then_some(Edit::Cells(cell_edits))
    }
}

enum DragMode {
    Idle,
    Marquee { anchor: (u16, u16) },
    Move { grab: (u16, u16), orig_dest: (i32, i32) },
}

/// Rectangular selection with move-to-floating-stamp and Delete. `resync` is deliberately left at
/// the trait's default no-op: because a drop reads `before` from the document at drop time (not
/// lift time), a mid-float external mutation needs no re-pinning — unlike `TextTool`'s burst.
pub struct SelectionTool {
    selection: Option<CellRect>,
    float: Option<FloatingStamp>,
    mode: DragMode,
    pending: Vec<PendingCell>,
}

impl Default for SelectionTool {
    fn default() -> Self {
        SelectionTool { selection: None, float: None, mode: DragMode::Idle, pending: Vec::new() }
    }
}

impl SelectionTool {
    pub fn new() -> Self {
        Self::default()
    }

    fn rebuild_pending(&mut self) {
        self.pending = self.float.as_ref().map(FloatingStamp::pending_cells).unwrap_or_default();
    }

    fn blank_region(doc: &Document, layer: usize, rect: CellRect) -> Option<Edit> {
        let mut cell_edits = Vec::new();
        for y in rect.y0..=rect.y1 {
            for x in rect.x0..=rect.x1 {
                let before = doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK);
                if before == Cell::BLANK {
                    continue;
                }
                cell_edits.push(CellEdit { layer, x, y, before, after: Cell::BLANK });
            }
        }
        (!cell_edits.is_empty()).then_some(Edit::Cells(cell_edits))
    }
}

impl Tool for SelectionTool {
    fn update(&mut self, ev: ToolEvent, ctx: &ToolCtx, doc: &Document) -> ToolResponse {
        match ev {
            ToolEvent::Press { x, y } => {
                if self.float.is_some() {
                    let regrab = self.float.as_ref().map(|f| (f.contains(x, y), f.dest));
                    if let Some((true, dest)) = regrab {
                        self.mode = DragMode::Move { grab: (x, y), orig_dest: dest };
                        return ToolResponse::Active;
                    }
                    // Click away from the float: drop it now, then start a fresh marquee at the
                    // click — mirrors TextTool::Press's same-call flush-then-relocate.
                    let edit = self.float.take().and_then(|f| f.to_edit(doc, ctx.layer));
                    self.mode = DragMode::Marquee { anchor: (x, y) };
                    self.selection = Some(CellRect::from_corners((x, y), (x, y)));
                    self.rebuild_pending();
                    return ToolResponse::Commit(edit);
                }
                if let Some(sel) = self.selection {
                    if sel.contains(x, y) {
                        let patch = CellPatch::from_region(doc, sel, ctx.layer);
                        let dest = (sel.x0 as i32, sel.y0 as i32);
                        self.float = Some(FloatingStamp { patch, source: Some(sel), dest });
                        self.mode = DragMode::Move { grab: (x, y), orig_dest: dest };
                        self.rebuild_pending();
                        return ToolResponse::Active;
                    }
                }
                self.mode = DragMode::Marquee { anchor: (x, y) };
                self.selection = Some(CellRect::from_corners((x, y), (x, y)));
                ToolResponse::Active
            }
            ToolEvent::Drag { x, y } => {
                match self.mode {
                    DragMode::Marquee { anchor } => {
                        self.selection = Some(CellRect::from_corners(anchor, (x, y)));
                    }
                    DragMode::Move { grab, orig_dest } => {
                        let dx = x as i32 - grab.0 as i32;
                        let dy = y as i32 - grab.1 as i32;
                        if let Some(float) = &mut self.float {
                            float.dest = (orig_dest.0 + dx, orig_dest.1 + dy);
                        }
                        self.rebuild_pending();
                    }
                    DragMode::Idle => {}
                }
                ToolResponse::Active
            }
            ToolEvent::Release => {
                self.mode = DragMode::Idle;
                ToolResponse::Active // no commit here — a marquee stays defined, a float stays floating
            }
            ToolEvent::Commit => {
                let edit = if let Some(float) = self.float.take() {
                    let rect = float.rect();
                    let edit = float.to_edit(doc, ctx.layer);
                    self.selection = Some(rect); // marquee stays at the stamp's final position
                    edit
                } else {
                    None
                };
                // The float this mode's Move/Marquee state referred to (if any) no longer exists
                // once Commit consumes it — reset explicitly rather than leaving `mode` stale.
                self.mode = DragMode::Idle;
                self.rebuild_pending();
                ToolResponse::Commit(edit)
            }
            ToolEvent::Delete => {
                let response = if let Some(float) = self.float.take() {
                    let source = float.source;
                    self.selection = source;
                    self.rebuild_pending();
                    ToolResponse::Commit(source.and_then(|src| Self::blank_region(doc, ctx.layer, src)))
                } else if let Some(sel) = self.selection {
                    ToolResponse::Commit(Self::blank_region(doc, ctx.layer, sel))
                } else {
                    ToolResponse::Commit(None)
                };
                // Same reasoning as Commit: whatever float/selection `mode` referred to may have
                // just been consumed, so it must not be left pointing at stale state.
                self.mode = DragMode::Idle;
                response
            }
            ToolEvent::Cancel => {
                self.float = None;
                self.selection = None;
                self.mode = DragMode::Idle;
                self.pending.clear();
                ToolResponse::Idle
            }
            _ => ToolResponse::Active,
        }
    }

    fn pending(&self) -> &[PendingCell] {
        &self.pending
    }

    fn accept_stamp(&mut self, patch: CellPatch, at: (u16, u16), _doc: &Document) {
        self.float = Some(FloatingStamp { patch, source: None, dest: (at.0 as i32, at.1 as i32) });
        self.selection = None;
        self.mode = DragMode::Idle;
        self.rebuild_pending();
    }

    fn selection_overlay(&self) -> Option<SelectionView> {
        let marquee = match &self.float {
            Some(float) => Some(float.rect()),
            None => self.selection,
        };
        let lifted_source = self.float.as_ref().and_then(|f| f.source);
        if marquee.is_none() && lifted_source.is_none() {
            return None;
        }
        Some(SelectionView { marquee, lifted_source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgba;
    use crate::tools::PlaneMask;

    fn ctx() -> ToolCtx {
        ToolCtx {
            layer: 0,
            glyph: '#',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
            mask: PlaneMask::ALL,
            density: crate::brush::DensityMode::Fixed(crate::brush::Fixed(1.0)),
            ramp: Vec::new(),
            size: 1,
            shape: crate::tools::BrushShape::default(),
        }
    }

    fn cell(ch: char) -> Cell {
        Cell { ch, fg: Rgba(1, 2, 3, 255), bg: Rgba(4, 5, 6, 255) }
    }

    fn filled_doc(w: u16, h: u16) -> Document {
        let mut doc = Document::new(w, h);
        for y in 0..h {
            for x in 0..w {
                doc.set_cell(0, x, y, cell((b'a' + ((x + y * w) % 26) as u8) as char));
            }
        }
        doc
    }

    fn commit_edit(resp: ToolResponse) -> Option<Edit> {
        match resp {
            ToolResponse::Commit(edit) => edit,
            other => panic!("expected Commit, got {other:?}"),
        }
    }

    #[test]
    fn marquee_only_session_commits_none_and_never_touches_the_doc() {
        let doc = filled_doc(10, 10);
        let before = doc.clone();
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 4, y: 4 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        assert!(commit_edit(resp).is_none());
        assert_eq!(doc, before, "a marquee alone must never mutate the document");
    }

    #[test]
    fn lift_and_drop_at_zero_offset_is_an_identity_no_op() {
        let doc = filled_doc(10, 10);
        let before = doc.clone();
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        // Lift: press again inside the now-defined selection.
        sel.update(ToolEvent::Press { x: 3, y: 3 }, &tctx, &doc);
        assert!(!sel.pending().is_empty(), "lifting should populate the float overlay");
        // Drop without moving.
        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        assert!(commit_edit(resp).is_none(), "dropping at the same position must be a no-op edit");
        assert_eq!(doc, before);
    }

    #[test]
    fn small_offset_move_merges_overlap_into_one_cell_edit_per_cell() {
        let doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 4, y: 4 }, &tctx, &doc); // 3x3 selection at (2,2)-(4,4)
        sel.update(ToolEvent::Release, &tctx, &doc);

        sel.update(ToolEvent::Press { x: 3, y: 3 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 4, y: 3 }, &tctx, &doc); // move +1 in x (partial overlap)
        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a combined edit") else { panic!("expected an Edit::Cells") };

        let coords: std::collections::HashSet<(u16, u16)> = cells.iter().map(|c| (c.x, c.y)).collect();
        assert_eq!(cells.len(), coords.len(), "each touched cell must appear exactly once");
        // Source columns 2..=4, dest columns 3..=5: union spans x in 2..=5.
        for c in &cells {
            assert!((2..=5).contains(&c.x) && (2..=4).contains(&c.y));
        }
    }

    #[test]
    fn large_disjoint_offset_blanks_all_source_and_writes_all_destination() {
        let doc = filled_doc(20, 20);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 1, y: 1 }, &tctx, &doc); // 2x2 selection
        sel.update(ToolEvent::Release, &tctx, &doc);

        sel.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 10, y: 10 }, &tctx, &doc); // move far away (disjoint)
        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a combined edit") else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 8, "4 source cells blanked + 4 destination cells written, fully disjoint");

        let mut history = crate::edit::History::new();
        let mut doc2 = doc.clone();
        history.apply(&mut doc2, Edit::Cells(cells));
        for y in 0..2u16 {
            for x in 0..2u16 {
                assert_eq!(doc2.cell(0, x, y), Some(&Cell::BLANK), "source must be fully blanked");
            }
        }
        for y in 10..12u16 {
            for x in 10..12u16 {
                assert_eq!(doc2.cell(0, x, y), doc.cell(0, x - 10, y - 10), "destination gets the lifted content");
            }
        }
    }

    #[test]
    fn move_off_canvas_clips_destination_but_still_blanks_source() {
        let doc = filled_doc(5, 5);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 3, y: 3 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 4, y: 4 }, &tctx, &doc); // 2x2 selection at (3,3)-(4,4)
        sel.update(ToolEvent::Release, &tctx, &doc);

        sel.update(ToolEvent::Press { x: 3, y: 3 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 6, y: 6 }, &tctx, &doc); // shift +3, pushes off the 5x5 canvas
        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a combined edit") else { panic!("expected an Edit::Cells") };

        // All 4 source cells must be blanked regardless of how much of the destination survived.
        for y in 3..5u16 {
            for x in 3..5u16 {
                let e = cells.iter().find(|c| c.x == x && c.y == y).expect("source cell must be edited");
                assert_eq!(e.after, Cell::BLANK);
            }
        }
        // No destination cell may land out of bounds.
        for c in &cells {
            assert!(c.x < 5 && c.y < 5, "no clipped destination cell may reach the edit");
        }
    }

    #[test]
    fn delete_on_a_float_blanks_only_the_source_and_discards_the_content() {
        let doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc); // move elsewhere

        let resp = sel.update(ToolEvent::Delete, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a blank-source edit") else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 4);
        assert!(cells.iter().all(|c| c.after == Cell::BLANK));
        // The destination region (5,5)-(6,6) must be untouched — the float's content is discarded.
        assert!(!cells.iter().any(|c| c.x >= 5 && c.y >= 5));
    }

    #[test]
    fn delete_on_a_plain_selection_blanks_the_region() {
        let doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);

        let resp = sel.update(ToolEvent::Delete, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a blank edit") else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 4);
        assert!(cells.iter().all(|c| c.after == Cell::BLANK));
    }

    #[test]
    fn delete_with_nothing_selected_commits_none() {
        let doc = filled_doc(5, 5);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        let resp = sel.update(ToolEvent::Delete, &tctx, &doc);
        assert!(commit_edit(resp).is_none());
    }

    #[test]
    fn cancel_discards_a_float_without_mutating_the_document() {
        let doc = filled_doc(10, 10);
        let before = doc.clone();
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 8, y: 8 }, &tctx, &doc);

        let resp = sel.update(ToolEvent::Cancel, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Idle));
        assert!(sel.pending().is_empty());
        assert_eq!(doc, before, "Cancel must never mutate the document — it was never touched while floating");
        assert!(sel.selection_overlay().is_none());
    }

    #[test]
    fn accept_stamp_injects_a_float_that_drops_identically_to_a_moved_one() {
        let doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        let patch = CellPatch { width: 2, height: 2, cells: vec![cell('Q'); 4] };
        sel.accept_stamp(patch, (3, 3), &doc);
        assert_eq!(sel.pending().len(), 4);

        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a write edit") else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 4, "a pasted stamp has no source, so drop only writes the destination");
        for c in &cells {
            assert!((3..5).contains(&c.x) && (3..5).contains(&c.y));
            assert_eq!(c.after.ch, 'Q');
        }
    }

    #[test]
    fn mid_float_external_mutation_is_reflected_at_drop_with_no_resync_needed() {
        let mut doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 1, y: 1 }, &tctx, &doc); // 1x1 selection
        sel.update(ToolEvent::Release, &tctx, &doc);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 6, y: 6 }, &tctx, &doc); // move to (6,6)

        // External mutation at the destination cell, bypassing the tool entirely — no resync call.
        let externally_written = cell('Z');
        doc.set_cell(0, 6, 6, externally_written);

        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a combined edit") else { panic!("expected an Edit::Cells") };
        let dest_edit = cells.iter().find(|c| c.x == 6 && c.y == 6).unwrap();
        assert_eq!(dest_edit.before, externally_written, "before must reflect doc's post-mutation state");
    }

    #[test]
    fn click_away_from_a_float_drops_it_and_starts_a_fresh_marquee_in_one_press() {
        let doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc); // move to (5,5)-(6,6)

        // Click far away from the floating stamp.
        let resp = sel.update(ToolEvent::Press { x: 9, y: 9 }, &tctx, &doc);
        let edit = commit_edit(resp).expect("click-away must drop the float as a committed edit");
        let Edit::Cells(cells) = edit else { panic!("expected an Edit::Cells") };
        assert!(cells.iter().any(|c| c.x == 5 && c.y == 5), "the drop must include the destination write");

        // A brand-new marquee now starts at the click point.
        assert_eq!(sel.selection_overlay().unwrap().marquee, Some(CellRect { x0: 9, y0: 9, x1: 9, y1: 9 }));
        assert!(sel.pending().is_empty(), "no float remains after the drop");
    }

    #[test]
    fn a_dropped_floats_blank_cells_overwrite_non_blank_destination_content() {
        // The float's rectangle fully replaces its destination rectangle on drop, Blank cells
        // included — a Blank patch cell landing on a filled destination cell must blank it, not
        // let the destination's prior glyph survive underneath a "transparent" Blank.
        let mut doc = filled_doc(10, 10);
        doc.set_cell(0, 1, 1, Cell::BLANK); // one Blank hole inside an otherwise-filled 2x1 source
        let before = doc.clone();
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        // Source: (1,1)-(2,1) — (1,1) Blank, (2,1) filled.
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 2, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 6, y: 1 }, &tctx, &doc); // move +5 onto filled dest (6,1)-(7,1)
        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a combined edit") else { panic!("expected an Edit::Cells") };

        let dest_blank = cells.iter().find(|c| c.x == 6 && c.y == 1).expect("dest cell under the float's Blank must be edited");
        assert_eq!(dest_blank.after, Cell::BLANK, "a Blank patch cell must overwrite the destination, not leave it showing through");
        assert_ne!(dest_blank.before, Cell::BLANK, "sanity: the destination cell actually had non-blank content before the drop");

        let mut history = crate::edit::History::new();
        let mut doc2 = doc.clone();
        history.apply(&mut doc2, Edit::Cells(cells.clone()));
        assert_eq!(doc2.cell(0, 6, 1), Some(&Cell::BLANK));
        assert_eq!(doc2.cell(0, 7, 1).unwrap().ch, before.cell(0, 2, 1).unwrap().ch, "the filled patch cell still lands normally");

        assert!(history.undo(&mut doc2));
        assert_eq!(doc2, before, "undo must restore the destination's original content byte-exact");
    }

    #[test]
    fn a_pasted_stamps_blank_cells_overwrite_non_blank_destination_content() {
        // Same full-rectangle-replace contract via the paste path (accept_stamp): an
        // external-text-style patch with a Blank pad cell must blank the destination cell it
        // lands on, not let the destination's prior content survive underneath.
        let doc = filled_doc(10, 10);
        let before = doc.clone();
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        let patch = CellPatch { width: 2, height: 1, cells: vec![cell('Q'), Cell::BLANK] };
        sel.accept_stamp(patch, (3, 3), &doc);
        let resp = sel.update(ToolEvent::Commit, &tctx, &doc);
        let Edit::Cells(cells) = commit_edit(resp).expect("expected a write edit") else { panic!("expected an Edit::Cells") };

        let blank_edit = cells.iter().find(|c| c.x == 4 && c.y == 3).expect("dest cell under the pasted Blank must be edited");
        assert_eq!(blank_edit.after, Cell::BLANK);
        let glyph_edit = cells.iter().find(|c| c.x == 3 && c.y == 3).expect("dest cell under the pasted glyph must be edited");
        assert_eq!(glyph_edit.after.ch, 'Q');

        let mut history = crate::edit::History::new();
        let mut doc2 = doc.clone();
        history.apply(&mut doc2, Edit::Cells(cells));
        assert_eq!(doc2.cell(0, 4, 3), Some(&Cell::BLANK));
        assert!(history.undo(&mut doc2));
        assert_eq!(doc2, before, "undo must restore the destination's original content byte-exact");
    }

    #[test]
    fn re_grabbing_inside_a_float_re_enters_move_without_committing() {
        let doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc); // now at (5,5)-(6,6)
        sel.update(ToolEvent::Release, &tctx, &doc);

        // Press again inside the float's current position: must re-grab, not drop.
        let resp = sel.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Active), "re-grabbing must not commit");
        assert!(!sel.pending().is_empty(), "the float must still exist after a re-grab");
    }

    #[test]
    fn undo_of_a_dropped_move_restores_the_document_in_one_step() {
        let doc = filled_doc(10, 10);
        let before = doc.clone();
        let mut working = doc.clone();
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        let mut history = crate::edit::History::new();

        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &working);
        sel.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &working);
        sel.update(ToolEvent::Release, &tctx, &working);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &working); // lift
        sel.update(ToolEvent::Drag { x: 6, y: 6 }, &tctx, &working); // move

        let resp = sel.update(ToolEvent::Commit, &tctx, &working);
        let edit = commit_edit(resp).expect("expected a combined edit");
        history.apply(&mut working, edit);
        assert_ne!(working, before, "sanity: the move actually changed the document");

        assert!(history.undo(&mut working));
        assert_eq!(working, before, "a single undo must revert the entire move");
    }

    #[test]
    fn commit_resets_drag_mode_to_idle_even_when_called_mid_move_without_a_release() {
        let doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc); // mid-move: mode == Move, no Release yet

        sel.update(ToolEvent::Commit, &tctx, &doc);
        assert!(
            matches!(sel.mode, DragMode::Idle),
            "mode must not be left pointing at a float that Commit just consumed"
        );
    }

    #[test]
    fn delete_resets_drag_mode_to_idle_even_when_called_mid_move_without_a_release() {
        let doc = filled_doc(10, 10);
        let tctx = ctx();
        let mut sel = SelectionTool::new();
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        sel.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &doc);
        sel.update(ToolEvent::Release, &tctx, &doc);
        sel.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc); // lift
        sel.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc); // mid-move: mode == Move, no Release yet

        sel.update(ToolEvent::Delete, &tctx, &doc);
        assert!(
            matches!(sel.mode, DragMode::Idle),
            "mode must not be left pointing at a float that Delete just consumed"
        );
    }
}
