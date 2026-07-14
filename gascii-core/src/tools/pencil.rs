use super::{FreehandStroke, PendingCell, Tool, ToolCtx, ToolEvent, ToolResponse};
use crate::model::{Cell, Document};

/// Stamps the active glyph/fg/bg through the plane mask along an interpolated path.
pub struct Pencil {
    stroke: FreehandStroke,
}

impl Default for Pencil {
    fn default() -> Self {
        Pencil { stroke: FreehandStroke::new() }
    }
}

impl Pencil {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Tool for Pencil {
    fn update(&mut self, ev: ToolEvent, ctx: &ToolCtx, doc: &Document) -> ToolResponse {
        let proposed = Cell { ch: ctx.glyph, fg: ctx.fg, bg: ctx.bg };
        match ev {
            ToolEvent::Press { x, y } => {
                self.stroke.press(x, y, proposed, ctx, doc);
                ToolResponse::Active
            }
            ToolEvent::Drag { x, y } => {
                self.stroke.drag(x, y, proposed, ctx, doc);
                ToolResponse::Active
            }
            ToolEvent::Release => ToolResponse::Commit(self.stroke.finish(doc, ctx.layer)),
            ToolEvent::Cancel => {
                self.stroke.cancel();
                ToolResponse::Idle
            }
            _ => ToolResponse::Active, // keyboard events are irrelevant to a pointer-driven tool
        }
    }

    fn pending(&self) -> &[PendingCell] {
        self.stroke.pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgba;
    use crate::tools::PlaneMask;

    fn ctx(mask: PlaneMask) -> ToolCtx {
        ToolCtx {
            layer: 0,
            glyph: '#',
            fg: Rgba(1, 2, 3, 255),
            bg: Rgba(4, 5, 6, 255),
            mask,
            density: crate::brush::DensityMode::Fixed(crate::brush::Fixed(1.0)),
            ramp: Vec::new(),
            size: 1,
            shape: crate::tools::BrushShape::Square,
        }
    }

    #[test]
    fn press_drag_release_emits_interpolated_edit() {
        let doc = Document::new(20, 20);
        let mut pencil = Pencil::new();
        let ctx = ctx(PlaneMask::ALL);

        pencil.update(ToolEvent::Press { x: 0, y: 0 }, &ctx, &doc);
        pencil.update(ToolEvent::Drag { x: 3, y: 0 }, &ctx, &doc);
        let resp = pencil.update(ToolEvent::Release, &ctx, &doc);

        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed multi-cell edit");
        };
        let mut coords: Vec<(u16, u16)> = cells.iter().map(|c| (c.x, c.y)).collect();
        coords.sort();
        assert_eq!(coords, vec![(0, 0), (1, 0), (2, 0), (3, 0)]);
        for c in &cells {
            assert_eq!(c.after.ch, '#');
        }
    }

    #[test]
    fn cell_revisited_within_stroke_yields_one_cell_edit() {
        let doc = Document::new(20, 20);
        let mut pencil = Pencil::new();
        let ctx = ctx(PlaneMask::ALL);

        pencil.update(ToolEvent::Press { x: 5, y: 5 }, &ctx, &doc);
        pencil.update(ToolEvent::Drag { x: 6, y: 5 }, &ctx, &doc);
        pencil.update(ToolEvent::Drag { x: 5, y: 5 }, &ctx, &doc); // revisit (5,5)
        let resp = pencil.update(ToolEvent::Release, &ctx, &doc);

        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        let count_5_5 = cells.iter().filter(|c| (c.x, c.y) == (5, 5)).count();
        assert_eq!(count_5_5, 1, "cell (5,5) must appear exactly once");
    }

    #[test]
    fn painting_identical_existing_content_commits_none() {
        let mut doc = Document::new(20, 20);
        let existing = Cell { ch: '#', fg: Rgba(1, 2, 3, 255), bg: Rgba(4, 5, 6, 255) };
        doc.set_cell(0, 2, 2, existing);

        let mut pencil = Pencil::new();
        let ctx = ctx(PlaneMask::ALL);
        pencil.update(ToolEvent::Press { x: 2, y: 2 }, &ctx, &doc);
        let resp = pencil.update(ToolEvent::Release, &ctx, &doc);

        assert!(matches!(resp, ToolResponse::Commit(None)));
    }

    #[test]
    fn glyph_only_mask_writes_glyph_and_text_color_but_keeps_bg() {
        let mut doc = Document::new(20, 20);
        let existing = Cell { ch: 'x', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) };
        doc.set_cell(0, 1, 1, existing);

        let mask = PlaneMask { glyph: true, bg: false };
        let mut pencil = Pencil::new();
        let ctx = ctx(mask);
        pencil.update(ToolEvent::Press { x: 1, y: 1 }, &ctx, &doc);
        let resp = pencil.update(ToolEvent::Release, &ctx, &doc);

        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].after.ch, '#');
        assert_eq!(cells[0].after.fg, ctx.fg, "text color follows the glyph plane");
        assert_eq!(cells[0].after.bg, existing.bg, "bg masked off");
    }

    #[test]
    fn pending_reflects_masked_result_mid_stroke() {
        let doc = Document::new(20, 20);
        let mask = PlaneMask { glyph: true, bg: false };
        let mut pencil = Pencil::new();
        let ctx = ctx(mask);
        pencil.update(ToolEvent::Press { x: 4, y: 4 }, &ctx, &doc);

        let pending = pencil.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].x, 4);
        assert_eq!(pending[0].y, 4);
        assert_eq!(pending[0].cell.ch, '#');
        assert_eq!(pending[0].cell.fg, ctx.fg); // text color follows the glyph plane
    }

    #[test]
    fn keyboard_events_are_harmless_no_ops_outside_a_stroke() {
        use crate::tools::Direction;
        let doc = Document::new(20, 20);
        let mut pencil = Pencil::new();
        let ctx = ctx(PlaneMask::ALL);
        for ev in [
            ToolEvent::Char('x'),
            ToolEvent::Backspace,
            ToolEvent::Enter,
            ToolEvent::Arrow(Direction::Left),
            ToolEvent::Commit,
        ] {
            let resp = pencil.update(ev, &ctx, &doc);
            assert!(matches!(resp, ToolResponse::Active));
            assert!(pencil.pending().is_empty());
        }
    }

    #[test]
    fn keyboard_events_are_harmless_no_ops_mid_stroke() {
        use crate::tools::Direction;
        let doc = Document::new(20, 20);
        let mut pencil = Pencil::new();
        let ctx = ctx(PlaneMask::ALL);
        pencil.update(ToolEvent::Press { x: 3, y: 3 }, &ctx, &doc);
        let pending_before: Vec<_> = pencil.pending().to_vec();
        for ev in [
            ToolEvent::Char('x'),
            ToolEvent::Backspace,
            ToolEvent::Enter,
            ToolEvent::Arrow(Direction::Left),
            ToolEvent::Commit,
        ] {
            let resp = pencil.update(ev, &ctx, &doc);
            assert!(matches!(resp, ToolResponse::Active));
            assert_eq!(pencil.pending(), pending_before.as_slice());
        }
    }

    #[test]
    fn sized_press_stamps_the_full_footprint() {
        let doc = Document::new(20, 20);
        let mut pencil = Pencil::new();
        let mut tctx = ctx(PlaneMask::ALL);
        tctx.size = 3;
        pencil.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
        let resp = pencil.update(ToolEvent::Release, &tctx, &doc);

        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 9, "size-3 square press covers the 3x3 box");
    }

    #[test]
    fn sized_stroke_clips_at_the_document_edge() {
        let doc = Document::new(20, 20);
        let mut pencil = Pencil::new();
        let mut tctx = ctx(PlaneMask::ALL);
        tctx.size = 3;
        pencil.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        let resp = pencil.update(ToolEvent::Release, &tctx, &doc);

        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 4, "corner press keeps only the in-bounds quadrant");
    }

    #[test]
    fn cancel_clears_pending_and_returns_idle() {
        let doc = Document::new(20, 20);
        let mut pencil = Pencil::new();
        let ctx = ctx(PlaneMask::ALL);
        pencil.update(ToolEvent::Press { x: 0, y: 0 }, &ctx, &doc);
        assert!(!pencil.pending().is_empty());

        let resp = pencil.update(ToolEvent::Cancel, &ctx, &doc);
        assert!(matches!(resp, ToolResponse::Idle));
        assert!(pencil.pending().is_empty());
    }
}
