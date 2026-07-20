//! Flood fill: an instant, one-click tool. Press computes and previews the whole fill region;
//! Release commits it. The 4-connected region match is exact-cell-equality and mask-independent —
//! only the *write* respects the plane mask.

use std::collections::{HashSet, VecDeque};

use super::{diff_pending, mask_apply, PendingCell, PlaneMask, Tool, ToolCtx, ToolEvent, ToolResponse};
use crate::model::{Cell, Document};

#[derive(Default)]
pub struct FloodFill {
    pending: Vec<PendingCell>,
    /// The Press-time `(proposed, mask)` inputs, held so `resync` can recompose every pending
    /// cell against the live document. The flood *region* stays pinned at Press: recomputing it
    /// against a mutated document could balloon it far beyond what the preview showed (a Clear
    /// would turn a bounded fill into a whole-canvas one), so the previewed shape is the contract
    /// and only the composition refreshes.
    source: Option<(Cell, PlaneMask)>,
}

impl FloodFill {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Tool for FloodFill {
    fn update(&mut self, ev: ToolEvent, ctx: &ToolCtx, doc: &Document) -> ToolResponse {
        match ev {
            ToolEvent::Press { x, y } => {
                self.pending.clear();
                self.source = None;
                if !doc.in_bounds(x, y) {
                    return ToolResponse::Active;
                }
                let target = doc.cell(ctx.layer, x, y).copied().unwrap_or(Cell::BLANK);
                let proposed = Cell { ch: ctx.glyph, fg: ctx.fg, bg: ctx.bg };
                self.source = Some((proposed, ctx.mask));

                // Iterative worklist — never recursion: a full 1024x1024 canvas is ~1M cells and
                // would overflow the stack if this were a recursive flood.
                let mut visited: HashSet<(u16, u16)> = HashSet::new();
                let mut worklist: VecDeque<(u16, u16)> = VecDeque::new();
                worklist.push_back((x, y));
                visited.insert((x, y));
                while let Some((cx, cy)) = worklist.pop_front() {
                    let cell = doc.cell(ctx.layer, cx, cy).copied().unwrap_or(Cell::BLANK);
                    self.pending.push(PendingCell { x: cx, y: cy, cell: mask_apply(cell, proposed, ctx.mask) });

                    let mut neighbors: [Option<(u16, u16)>; 4] = [None; 4];
                    if cx > 0 {
                        neighbors[0] = Some((cx - 1, cy));
                    }
                    if cy > 0 {
                        neighbors[1] = Some((cx, cy - 1));
                    }
                    if cx + 1 < doc.width {
                        neighbors[2] = Some((cx + 1, cy));
                    }
                    if cy + 1 < doc.height {
                        neighbors[3] = Some((cx, cy + 1));
                    }
                    for n in neighbors.into_iter().flatten() {
                        if !visited.insert(n) {
                            continue;
                        }
                        let ncell = doc.cell(ctx.layer, n.0, n.1).copied().unwrap_or(Cell::BLANK);
                        if ncell == target {
                            worklist.push_back(n);
                        }
                    }
                }
                ToolResponse::Active
            }
            ToolEvent::Drag { .. } => ToolResponse::Active, // fill is instant on Press; drag previews nothing new
            ToolEvent::Release => {
                let edit = diff_pending(&self.pending, doc, ctx.layer);
                self.pending.clear();
                self.source = None;
                ToolResponse::Commit(edit)
            }
            ToolEvent::Cancel => {
                self.pending.clear();
                self.source = None;
                ToolResponse::Idle
            }
            _ => ToolResponse::Active,
        }
    }

    fn pending(&self) -> &[PendingCell] {
        &self.pending
    }

    /// Recomposes every pending cell against `doc`'s current content. Without this, a mutation
    /// landing between Press and Release (Clear, undo/redo, the other binding's commit) leaves
    /// the Press-time composition in `pending`, and Release's `diff_pending` would commit it
    /// wholesale — re-imposing the superseded content, on masked-off planes especially.
    fn resync(&mut self, doc: &Document, layer: usize) {
        let Some((proposed, mask)) = self.source else { return };
        for p in &mut self.pending {
            let current = doc.cell(layer, p.x, p.y).copied().unwrap_or(Cell::BLANK);
            p.cell = mask_apply(current, proposed, mask);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgba;
    use crate::tools::PlaneMask;

    fn ctx(mask: PlaneMask, glyph: char, fg: Rgba, bg: Rgba) -> ToolCtx {
        ToolCtx {
            layer: 0,
            glyph,
            fg,
            bg,
            mask,
            density: crate::brush::DensityMode::Fixed(crate::brush::Fixed(1.0)),
            ramp: Vec::new(),
            size: 1,
            shape: crate::tools::BrushShape::default(),
        }
    }

    fn press_release(doc: &Document, tctx: &ToolCtx, at: (u16, u16)) -> ToolResponse {
        let mut fill = FloodFill::new();
        fill.update(ToolEvent::Press { x: at.0, y: at.1 }, tctx, doc);
        fill.update(ToolEvent::Release, tctx, doc)
    }

    #[test]
    fn fills_a_single_connected_region_of_matching_cells() {
        let doc = Document::new(10, 10); // fully Blank
        let tctx = ctx(PlaneMask::ALL, '#', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        let resp = press_release(&doc, &tctx, (5, 5));
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 100, "the whole blank canvas is one connected region");
        assert!(cells.iter().all(|c| c.after.ch == '#'));
    }

    #[test]
    fn fill_stops_at_a_differing_border() {
        let mut doc = Document::new(5, 5);
        // A vertical wall of '|' at column 3 bounds the fill to columns 0..3.
        for y in 0..5u16 {
            doc.set_cell(0, 3, y, Cell { ch: '|', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        }
        let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
        let resp = press_release(&doc, &tctx, (0, 0));
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 15, "columns 0..3 across 5 rows = 15 cells");
        assert!(cells.iter().all(|c| c.x < 3));
    }

    #[test]
    fn diagonal_neighbors_are_not_connected() {
        let mut doc = Document::new(3, 3);
        // A plus-shaped wall isolates the four diagonal corners from each other and from center.
        doc.set_cell(0, 1, 0, Cell { ch: '#', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 0, 1, Cell { ch: '#', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 2, 1, Cell { ch: '#', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 1, 2, Cell { ch: '#', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 1, 1, Cell { ch: '#', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });

        let tctx = ctx(PlaneMask::ALL, '@', Rgba::WHITE, Rgba::TRANSPARENT);
        let resp = press_release(&doc, &tctx, (0, 0)); // top-left corner, Blank, diagonal-only to (2,0) etc.
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 1, "the corner cell has no 4-connected Blank neighbor");
        assert_eq!((cells[0].x, cells[0].y), (0, 0));
    }

    #[test]
    fn full_1024x1024_fill_completes_via_an_iterative_worklist() {
        let doc = Document::new(1024, 1024);
        let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
        let resp = press_release(&doc, &tctx, (512, 512));
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 1024 * 1024);
    }

    #[test]
    fn fill_that_changes_nothing_commits_none() {
        let mut doc = Document::new(5, 5);
        let existing = Cell { ch: '#', fg: Rgba(1, 2, 3, 255), bg: Rgba(4, 5, 6, 255) };
        for y in 0..5u16 {
            for x in 0..5u16 {
                doc.set_cell(0, x, y, existing);
            }
        }
        let tctx = ctx(PlaneMask::ALL, existing.ch, existing.fg, existing.bg);
        let resp = press_release(&doc, &tctx, (2, 2));
        assert!(matches!(resp, ToolResponse::Commit(None)));
    }

    #[test]
    fn glyph_only_mask_fills_glyph_and_its_text_color_but_preserves_existing_bg_per_cell() {
        let mut doc = Document::new(3, 1);
        // Three Blank-glyph cells that differ only in fg — but the match is by glyph+fg+bg, so
        // this is actually three separate 1-cell regions, not one connected match.
        doc.set_cell(0, 0, 0, Cell { ch: ' ', fg: Rgba(1, 1, 1, 255), bg: Rgba(7, 7, 7, 255) });
        doc.set_cell(0, 1, 0, Cell { ch: ' ', fg: Rgba(2, 2, 2, 255), bg: Rgba(7, 7, 7, 255) });
        doc.set_cell(0, 2, 0, Cell { ch: ' ', fg: Rgba(1, 1, 1, 255), bg: Rgba(7, 7, 7, 255) });

        let mask = PlaneMask { glyph: true, bg: false };
        let tctx = ctx(mask, '#', Rgba(9, 9, 9, 255), Rgba(9, 9, 9, 255));
        let resp = press_release(&doc, &tctx, (0, 0));
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        // Only (0,0) matches the clicked cell's exact glyph+fg+bg; (1,0) differs in fg and is
        // outside the match region even though it's adjacent.
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].after.ch, '#');
        assert_eq!(cells[0].after.fg, Rgba(9, 9, 9, 255), "text color follows the glyph plane");
        assert_eq!(cells[0].after.bg, Rgba(7, 7, 7, 255), "bg masked off: keeps existing per-cell bg");
    }

    #[test]
    fn each_plane_mask_combination_writes_only_the_enabled_planes_over_the_matched_region() {
        let doc = Document::new(4, 1); // fully Blank, one connected region
        let proposed_fg = Rgba(10, 20, 30, 255);
        let proposed_bg = Rgba(40, 50, 60, 255);
        for mask in [
            PlaneMask { glyph: true, bg: false },
            PlaneMask { glyph: false, bg: true },
            PlaneMask::ALL,
        ] {
            let tctx = ctx(mask, '@', proposed_fg, proposed_bg);
            let resp = press_release(&doc, &tctx, (0, 0));
            let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
                panic!("expected a committed edit for mask {mask:?}");
            };
            for c in &cells {
                assert_eq!(c.after.ch == '@', mask.glyph);
                assert_eq!(c.after.fg == proposed_fg, mask.glyph, "text color follows the glyph plane");
                assert_eq!(c.after.bg == proposed_bg, mask.bg);
            }
        }
    }

    #[test]
    fn press_out_of_bounds_yields_an_empty_fill() {
        let doc = Document::new(5, 5);
        let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
        let mut fill = FloodFill::new();
        fill.update(ToolEvent::Press { x: 999, y: 999 }, &tctx, &doc);
        assert!(fill.pending().is_empty());
        let resp = fill.update(ToolEvent::Release, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Commit(None)));
    }

    /// A document mutation landing between Press and Release (a Clear, an undo/redo, the other
    /// binding's commit) must not be silently reverted by the fill's Release: `resync` recomposes
    /// the held region against the mutated document, so masked-off planes commit the document's
    /// *current* content, never the Press-time snapshot.
    #[test]
    fn resync_after_external_clear_recomposes_instead_of_reimposing_stale_content() {
        let mut doc = Document::new(3, 1);
        let colored = Cell { ch: 'x', fg: Rgba(1, 1, 1, 255), bg: Rgba(7, 7, 7, 255) };
        for x in 0..3u16 {
            doc.set_cell(0, x, 0, colored);
        }
        let mask = PlaneMask { glyph: true, bg: false };
        let tctx = ctx(mask, '#', Rgba(9, 9, 9, 255), Rgba(2, 2, 2, 255));
        let mut fill = FloodFill::new();
        fill.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        assert_eq!(fill.pending().len(), 3);
        assert!(fill.pending().iter().all(|p| p.cell.bg == colored.bg), "press-time composition keeps existing bg");

        // Simulate a Clear landing mid-gesture.
        for x in 0..3u16 {
            doc.set_cell(0, x, 0, Cell::BLANK);
        }
        fill.resync(&doc, 0);

        let resp = fill.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 3, "the previewed region still commits");
        for c in &cells {
            assert_eq!(c.after.ch, '#');
            assert_eq!(c.after.bg, Cell::BLANK.bg, "bg masked off: the cleared bg must survive, not the press-time bg");
            assert_eq!(c.before, Cell::BLANK, "before must be the post-clear value");
        }
    }

    /// The flood region itself is pinned at Press: an external mutation must not grow it, even
    /// when the mutated document would flood further.
    #[test]
    fn resync_never_regrows_the_region_beyond_the_press_time_flood() {
        let mut doc = Document::new(5, 1);
        // A wall at column 2 bounds the press-time flood to columns 0..2.
        doc.set_cell(0, 2, 0, Cell { ch: '|', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
        let mut fill = FloodFill::new();
        fill.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        assert_eq!(fill.pending().len(), 2);

        // The wall vanishes mid-gesture (e.g. an undo); the region must stay as previewed.
        doc.set_cell(0, 2, 0, Cell::BLANK);
        fill.resync(&doc, 0);
        assert_eq!(fill.pending().len(), 2, "resync recomposes, never re-floods");
        let resp = fill.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert!(cells.iter().all(|c| c.x < 2));
    }

    #[test]
    fn cancel_discards_pending_and_returns_idle() {
        let doc = Document::new(5, 5);
        let tctx = ctx(PlaneMask::ALL, '#', Rgba::WHITE, Rgba::TRANSPARENT);
        let mut fill = FloodFill::new();
        fill.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        assert!(!fill.pending().is_empty());
        let resp = fill.update(ToolEvent::Cancel, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Idle));
        assert!(fill.pending().is_empty());
    }
}
