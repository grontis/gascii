use super::{FreehandStroke, PendingCell, Tool, ToolCtx, ToolEvent, ToolResponse};
use crate::model::{Cell, Document};

/// Stamps `Cell::BLANK` through the plane mask along an interpolated path. With the
/// default mask (all planes on) this fully clears a cell to `Cell::BLANK`; disabling a plane
/// (e.g. bg off) leaves that plane's pre-existing value untouched for a selective erase.
pub struct Eraser {
    stroke: FreehandStroke,
}

impl Default for Eraser {
    fn default() -> Self {
        Eraser { stroke: FreehandStroke::new() }
    }
}

impl Eraser {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Tool for Eraser {
    fn update(&mut self, ev: ToolEvent, ctx: &ToolCtx, doc: &Document) -> ToolResponse {
        let proposed = Cell::BLANK;
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

    fn resync(&mut self, doc: &Document, layer: usize) {
        self.stroke.resync(doc, layer);
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
            shape: crate::tools::BrushShape::default(),
        }
    }

    fn painted_doc() -> Document {
        let mut doc = Document::new(20, 20);
        doc.set_cell(0, 5, 5, Cell { ch: 'x', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) });
        doc
    }

    #[test]
    fn full_erase_writes_blank() {
        let doc = painted_doc();
        let mut eraser = Eraser::new();
        let ctx = ctx(PlaneMask::ALL);
        eraser.update(ToolEvent::Press { x: 5, y: 5 }, &ctx, &doc);
        let resp = eraser.update(ToolEvent::Release, &ctx, &doc);

        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].after, Cell::BLANK);
    }

    #[test]
    fn glyph_only_erase_blanks_char_and_text_color_keeps_bg() {
        let doc = painted_doc();
        let existing = *doc.cell(0, 5, 5).unwrap();
        let mask = PlaneMask { glyph: true, bg: false };
        let mut eraser = Eraser::new();
        let ctx = ctx(mask);
        eraser.update(ToolEvent::Press { x: 5, y: 5 }, &ctx, &doc);
        let resp = eraser.update(ToolEvent::Release, &ctx, &doc);

        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells[0].after.ch, ' ');
        assert_eq!(cells[0].after.fg, Cell::BLANK.fg, "text color is cleared alongside the glyph");
        assert_eq!(cells[0].after.bg, existing.bg, "bg masked off");
    }

    #[test]
    fn bg_only_erase_clears_bg_keeps_glyph_fg() {
        let doc = painted_doc();
        let existing = *doc.cell(0, 5, 5).unwrap();
        let mask = PlaneMask { glyph: false, bg: true };
        let mut eraser = Eraser::new();
        let ctx = ctx(mask);
        eraser.update(ToolEvent::Press { x: 5, y: 5 }, &ctx, &doc);
        let resp = eraser.update(ToolEvent::Release, &ctx, &doc);

        let ToolResponse::Commit(Some(crate::edit::Edit::Cells(cells))) = resp else {
            panic!("expected a committed edit");
        };
        assert_eq!(cells[0].after.ch, existing.ch);
        assert_eq!(cells[0].after.fg, existing.fg);
        assert_eq!(cells[0].after.bg, Cell::BLANK.bg);
    }

    #[test]
    fn keyboard_events_are_harmless_no_ops_outside_a_stroke() {
        use crate::tools::Direction;
        let doc = Document::new(20, 20);
        let mut eraser = Eraser::new();
        let ctx = ctx(PlaneMask::ALL);
        for ev in [
            ToolEvent::Char('x'),
            ToolEvent::Backspace,
            ToolEvent::Enter,
            ToolEvent::Arrow(Direction::Left),
            ToolEvent::Commit,
        ] {
            let resp = eraser.update(ev, &ctx, &doc);
            assert!(matches!(resp, ToolResponse::Active));
            assert!(eraser.pending().is_empty());
        }
    }

    #[test]
    fn keyboard_events_are_harmless_no_ops_mid_stroke() {
        use crate::tools::Direction;
        let doc = painted_doc();
        let mut eraser = Eraser::new();
        let ctx = ctx(PlaneMask::ALL);
        eraser.update(ToolEvent::Press { x: 5, y: 5 }, &ctx, &doc);
        let pending_before: Vec<_> = eraser.pending().to_vec();
        for ev in [
            ToolEvent::Char('x'),
            ToolEvent::Backspace,
            ToolEvent::Enter,
            ToolEvent::Arrow(Direction::Left),
            ToolEvent::Commit,
        ] {
            let resp = eraser.update(ev, &ctx, &doc);
            assert!(matches!(resp, ToolResponse::Active));
            assert_eq!(eraser.pending(), pending_before.as_slice());
        }
    }

    #[test]
    fn erasing_already_blank_cell_commits_none() {
        let doc = Document::new(20, 20);
        let mut eraser = Eraser::new();
        let ctx = ctx(PlaneMask::ALL);
        eraser.update(ToolEvent::Press { x: 0, y: 0 }, &ctx, &doc);
        let resp = eraser.update(ToolEvent::Release, &ctx, &doc);
        assert!(matches!(resp, ToolResponse::Commit(None)));
    }
}
