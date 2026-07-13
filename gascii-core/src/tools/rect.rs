//! Rectangle tool: border cells resolved against existing box-drawing art via `join`; the interior
//! is untouched. A one-cell-wide or one-cell-tall rectangle degenerates to a straight line.

use super::{diff_pending, mask_apply, CellRect, PendingCell, Tool, ToolCtx, ToolEvent, ToolResponse};
use crate::join::{join, ArmSet};
use crate::model::{Cell, Document};

#[derive(Default)]
pub struct Rectangle {
    anchor: Option<(u16, u16)>,
    pending: Vec<PendingCell>,
}

impl Rectangle {
    pub fn new() -> Self {
        Self::default()
    }

    fn stamp(pending: &mut Vec<PendingCell>, doc: &Document, ctx: &ToolCtx, x: u16, y: u16, arms: ArmSet, strict: bool) {
        if !doc.in_bounds(x, y) {
            return;
        }
        let before = doc.cell(ctx.layer, x, y).copied().unwrap_or(Cell::BLANK);
        let ch = join(before.ch, arms, strict, ctx.glyph);
        let proposed = Cell { ch, fg: ctx.fg, bg: ctx.bg };
        pending.push(PendingCell { x, y, cell: mask_apply(before, proposed, ctx.mask) });
    }

    fn recompute(&mut self, cur: (u16, u16), ctx: &ToolCtx, doc: &Document) {
        let Some(anchor) = self.anchor else { return };
        self.pending.clear();
        let rect = CellRect::from_corners(anchor, cur);
        let strict = doc.settings.strict_ascii;
        let horizontal = ArmSet::E.union(ArmSet::W);
        let vertical = ArmSet::N.union(ArmSet::S);

        if rect.y0 == rect.y1 {
            // Degenerate to a horizontal line (also covers the 1x1 case).
            for x in rect.x0..=rect.x1 {
                Self::stamp(&mut self.pending, doc, ctx, x, rect.y0, horizontal, strict);
            }
        } else if rect.x0 == rect.x1 {
            // Degenerate to a vertical line.
            for y in rect.y0..=rect.y1 {
                Self::stamp(&mut self.pending, doc, ctx, rect.x0, y, vertical, strict);
            }
        } else {
            for x in rect.x0..=rect.x1 {
                Self::stamp(&mut self.pending, doc, ctx, x, rect.y0, corner_or_edge_arms(x, rect.y0, rect), strict);
                Self::stamp(&mut self.pending, doc, ctx, x, rect.y1, corner_or_edge_arms(x, rect.y1, rect), strict);
            }
            for y in (rect.y0 + 1)..rect.y1 {
                Self::stamp(&mut self.pending, doc, ctx, rect.x0, y, vertical, strict);
                Self::stamp(&mut self.pending, doc, ctx, rect.x1, y, vertical, strict);
            }
        }
    }
}

/// Base arm set for a border cell of a proper (non-degenerate) rectangle: corners get the two
/// arms that meet there, straight edge cells get the horizontal or vertical pair. Only called for
/// cells on the top or bottom row of a rect with `x0<x1 && y0<y1`.
fn corner_or_edge_arms(x: u16, y: u16, rect: CellRect) -> ArmSet {
    let top = y == rect.y0;
    let bottom = y == rect.y1;
    let left = x == rect.x0;
    let right = x == rect.x1;
    match (top, bottom, left, right) {
        (true, false, true, false) => ArmSet::E.union(ArmSet::S),
        (true, false, false, true) => ArmSet::W.union(ArmSet::S),
        (false, true, true, false) => ArmSet::E.union(ArmSet::N),
        (false, true, false, true) => ArmSet::W.union(ArmSet::N),
        _ => ArmSet::E.union(ArmSet::W), // non-corner cell on the top/bottom edge
    }
}

impl Tool for Rectangle {
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
        ToolCtx {
            layer: 0,
            glyph,
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
            mask,
            density: crate::brush::DensityMode::Fixed(crate::brush::Fixed(1.0)),
            ramp: Vec::new(),
        }
    }

    fn drag(doc: &Document, tctx: &ToolCtx, from: (u16, u16), to: (u16, u16)) -> Rectangle {
        let mut rect = Rectangle::new();
        rect.update(ToolEvent::Press { x: from.0, y: from.1 }, tctx, doc);
        rect.update(ToolEvent::Drag { x: to.0, y: to.1 }, tctx, doc);
        rect
    }

    fn chars_at(cells: &[crate::edit::CellEdit]) -> std::collections::HashMap<(u16, u16), char> {
        cells.iter().map(|c| ((c.x, c.y), c.after.ch)).collect()
    }

    #[test]
    fn empty_canvas_rectangle_produces_expected_border_glyphs() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = drag(&doc, &tctx, (2, 2), (5, 5));
        let resp = rect.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        let chars = chars_at(&cells);
        assert_eq!(chars[&(2, 2)], '┌');
        assert_eq!(chars[&(5, 2)], '┐');
        assert_eq!(chars[&(2, 5)], '└');
        assert_eq!(chars[&(5, 5)], '┘');
        assert_eq!(chars[&(3, 2)], '─');
        assert_eq!(chars[&(2, 3)], '│');
        // Interior untouched.
        assert!(!chars.contains_key(&(3, 3)));
    }

    #[test]
    fn rectangle_border_cell_count_matches_perimeter_with_no_duplicates() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = drag(&doc, &tctx, (1, 1), (4, 6)); // 4 wide, 6 tall
        let resp = rect.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        // perimeter of a w x h rect = 2*w + 2*h - 4
        assert_eq!(cells.len(), 2 * 4 + 2 * 6 - 4);
        let coords: std::collections::HashSet<(u16, u16)> = cells.iter().map(|c| (c.x, c.y)).collect();
        assert_eq!(coords.len(), cells.len(), "no coordinate must be duplicated");
    }

    #[test]
    fn rectangle_drawn_over_existing_box_art_joins_at_contact_cells() {
        let mut doc = Document::new(10, 10);
        // A vertical line crossing straight through where the rectangle's top edge will land.
        for y in 0..10u16 {
            doc.set_cell(0, 5, y, Cell { ch: '│', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        }
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = drag(&doc, &tctx, (2, 2), (8, 8));
        let resp = rect.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        let chars = chars_at(&cells);
        // (5,2) is a non-corner top-edge cell (E|W) crossing the existing vertical line (N|S).
        assert_eq!(chars[&(5, 2)], '┼');
        // (5,8) is a non-corner bottom-edge cell, same crossing.
        assert_eq!(chars[&(5, 8)], '┼');
    }

    #[test]
    fn one_wide_rectangle_degenerates_to_a_vertical_line() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = drag(&doc, &tctx, (4, 2), (4, 6));
        let resp = rect.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 5, "no duplicated coincident border cells");
        assert!(cells.iter().all(|c| c.after.ch == '│'));
    }

    #[test]
    fn one_tall_rectangle_degenerates_to_a_horizontal_line() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = drag(&doc, &tctx, (2, 4), (6, 4));
        let resp = rect.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 5);
        assert!(cells.iter().all(|c| c.after.ch == '─'));
    }

    #[test]
    fn strict_ascii_document_draws_plus_minus_pipe() {
        let mut doc = Document::new(10, 10);
        doc.settings = DocSettings { strict_ascii: true };
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = drag(&doc, &tctx, (2, 2), (5, 5));
        let resp = rect.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        let chars = chars_at(&cells);
        assert_eq!(chars[&(2, 2)], '+');
        assert_eq!(chars[&(3, 2)], '-');
        assert_eq!(chars[&(2, 3)], '|');
    }

    #[test]
    fn release_with_no_press_commits_none() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = Rectangle::new();
        let resp = rect.update(ToolEvent::Release, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Commit(None)));
    }

    #[test]
    fn cancel_discards_pending_and_returns_idle() {
        let doc = Document::new(10, 10);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = drag(&doc, &tctx, (2, 2), (5, 5));
        assert!(!rect.pending().is_empty());
        let resp = rect.update(ToolEvent::Cancel, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Idle));
        assert!(rect.pending().is_empty());
    }

    #[test]
    fn drag_beyond_document_bounds_is_clipped_without_panicking() {
        let doc = Document::new(5, 5);
        let tctx = ctx(PlaneMask::ALL, '#');
        let mut rect = drag(&doc, &tctx, (2, 2), (999, 999));
        let resp = rect.update(ToolEvent::Release, &tctx, &doc);
        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        for c in &cells {
            assert!(c.x < 5 && c.y < 5);
        }
    }
}
