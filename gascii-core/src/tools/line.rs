//! Straight-line tool: a horizontal or vertical run auto-joins existing box-drawing art the same
//! way the rectangle tool's border does. A diagonal run has no single-line box glyph, so it stamps
//! the active glyph directly with no join.

use super::{diff_pending, line_cells, mask_apply, PendingCell, Tool, ToolCtx, ToolEvent, ToolResponse};
use crate::join::{join, ArmSet};
use crate::model::{Cell, Document};

#[derive(Default)]
pub struct Line {
    anchor: Option<(u16, u16)>,
    pending: Vec<PendingCell>,
    buf: Vec<(u16, u16)>,
}

impl Line {
    pub fn new() -> Self {
        Self::default()
    }

    fn recompute(&mut self, cur: (u16, u16), ctx: &ToolCtx, doc: &Document) {
        let Some(anchor) = self.anchor else { return };
        self.pending.clear();
        let mut buf = std::mem::take(&mut self.buf);
        line_cells(anchor, cur, &mut buf);

        let strict = doc.settings.strict_ascii;
        let horizontal = anchor.1 == cur.1;
        let vertical = anchor.0 == cur.0;
        for &(x, y) in buf.iter() {
            if !doc.in_bounds(x, y) {
                continue;
            }
            let before = doc.cell(ctx.layer, x, y).copied().unwrap_or(Cell::BLANK);
            let ch = if horizontal {
                join(before.ch, ArmSet::E.union(ArmSet::W), strict, ctx.glyph)
            } else if vertical {
                join(before.ch, ArmSet::N.union(ArmSet::S), strict, ctx.glyph)
            } else {
                ctx.glyph // diagonal: no single-line box glyph, stamp directly
            };
            let proposed = Cell { ch, fg: ctx.fg, bg: ctx.bg };
            self.pending.push(PendingCell { x, y, cell: mask_apply(before, proposed, ctx.mask) });
        }
        self.buf = buf;
    }
}

impl Tool for Line {
    fn update(&mut self, ev: ToolEvent, ctx: &ToolCtx, doc: &Document) -> ToolResponse {
        match ev {
            ToolEvent::Press { x, y } => {
                self.anchor = Some((x, y));
                self.recompute((x, y), ctx, doc);
                ToolResponse::Active
            }
            ToolEvent::Drag { x, y } => {
                self.recompute((x, y), ctx, doc);
                ToolResponse::Active
            }
            ToolEvent::Release => {
                let edit = diff_pending(&self.pending, doc, ctx.layer);
                self.pending.clear();
                self.anchor = None;
                ToolResponse::Commit(edit)
            }
            ToolEvent::Cancel => {
                self.anchor = None;
                self.pending.clear();
                ToolResponse::Idle
            }
            _ => ToolResponse::Active,
        }
    }

    fn pending(&self) -> &[PendingCell] {
        &self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DocSettings, Rgba};
    use crate::tools::PlaneMask;

    fn ctx(mask: PlaneMask, glyph: char) -> ToolCtx {
        ToolCtx { layer: 0, glyph, fg: Rgba::WHITE, bg: Rgba::TRANSPARENT, mask }
    }

    fn drag(doc: &Document, tctx: &ToolCtx, from: (u16, u16), to: (u16, u16)) -> Line {
        let mut line = Line::new();
        line.update(ToolEvent::Press { x: from.0, y: from.1 }, tctx, doc);
        line.update(ToolEvent::Drag { x: to.0, y: to.1 }, tctx, doc);
        line
    }

    #[test]
    fn horizontal_line_cell_set_and_glyphs() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut line = drag(&doc, &tctx, (2, 4), (6, 4));
        let resp = line.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 5);
        assert!(cells.iter().all(|c| c.after.ch == '─' && c.y == 4));
        let xs: std::collections::HashSet<u16> = cells.iter().map(|c| c.x).collect();
        assert_eq!(xs, (2..=6u16).collect());
    }

    #[test]
    fn vertical_line_cell_set_and_glyphs() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut line = drag(&doc, &tctx, (4, 1), (4, 7));
        let resp = line.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 7);
        assert!(cells.iter().all(|c| c.after.ch == '│' && c.x == 4));
    }

    #[test]
    fn diagonal_line_stamps_the_active_glyph_with_no_join() {
        let mut doc = Document::new(10, 10);
        doc.set_cell(0, 2, 2, Cell { ch: '│', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        let tctx = ctx(PlaneMask::ALL, '@');
        let mut line = drag(&doc, &tctx, (2, 2), (6, 6));
        let resp = line.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert!(cells.iter().all(|c| c.after.ch == '@'), "a diagonal run must stamp the glyph directly, never join");
    }

    #[test]
    fn horizontal_line_joins_a_crossing_vertical_run() {
        let mut doc = Document::new(10, 10);
        for y in 0..10u16 {
            doc.set_cell(0, 5, y, Cell { ch: '│', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        }
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut line = drag(&doc, &tctx, (2, 3), (8, 3));
        let resp = line.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        let crossing = cells.iter().find(|c| c.x == 5 && c.y == 3).unwrap();
        assert_eq!(crossing.after.ch, '┼');
    }

    #[test]
    fn single_point_line_is_treated_as_horizontal() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut line = drag(&doc, &tctx, (3, 3), (3, 3));
        let resp = line.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].after.ch, '─');
    }

    #[test]
    fn strict_ascii_horizontal_and_vertical_lines_use_dash_and_pipe() {
        let mut doc = Document::new(10, 10);
        doc.settings = DocSettings { strict_ascii: true };
        let tctx = ctx(PlaneMask::ALL, '#');

        let mut h = drag(&doc, &tctx, (0, 0), (3, 0));
        let resp = h.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert!(cells.iter().all(|c| c.after.ch == '-'));

        let mut v = drag(&doc, &tctx, (0, 1), (0, 4));
        let resp = v.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert!(cells.iter().all(|c| c.after.ch == '|'));
    }

    #[test]
    fn release_with_no_press_commits_none() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut line = Line::new();
        let resp = line.update(ToolEvent::Release, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Commit(None)));
    }

    #[test]
    fn cancel_discards_pending_and_returns_idle() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut line = drag(&doc, &tctx, (0, 0), (4, 4));
        assert!(!line.pending().is_empty());
        let resp = line.update(ToolEvent::Cancel, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Idle));
        assert!(line.pending().is_empty());
    }

    #[test]
    fn drag_beyond_document_bounds_is_clipped_without_panicking() {
        let doc = Document::new(5, 5);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut line = drag(&doc, &tctx, (0, 0), (9999, 0));
        let resp = line.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        for c in &cells {
            assert!(c.x < 5 && c.y < 5);
        }
    }
}
