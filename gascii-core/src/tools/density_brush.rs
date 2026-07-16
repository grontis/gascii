//! Density brush: stamps a ramp glyph whose index is driven by an `IntensitySource` (Fixed or
//! Buildup) rather than a constant glyph. Reuses `FreehandStroke`'s pending/before/finish/cancel
//! machinery but runs its own interpolation loop — `FreehandStroke::press`/`drag` assume one
//! constant proposed cell per interpolated segment, which doesn't hold here since Buildup's
//! proposed glyph depends on the cell's current ramp index at the moment it's touched.

use std::time::Instant;

use std::collections::HashSet;

use super::{footprint, line_cells, FreehandStroke, PendingCell, Tool, ToolCtx, ToolEvent, ToolResponse};
use crate::brush::{intensity_to_index, DensityMode, IntensitySource, StrokeSample};
use crate::model::{Cell, Document};

pub struct DensityBrush {
    stroke: FreehandStroke,
    last: Option<(u16, u16)>,
    buf: Vec<(u16, u16)>,
    fp: Vec<(u16, u16)>,
    /// Cells covered by the previous path step's footprint. Consecutive footprints along a drag
    /// overlap almost entirely; without masking that overlap out, every path step would
    /// re-advance the shared cells and a size-N Buildup drag would saturate the ramp in one pass.
    prev_fp: HashSet<(u16, u16)>,
    started: Option<Instant>,
}

impl Default for DensityBrush {
    fn default() -> Self {
        DensityBrush {
            stroke: FreehandStroke::new(),
            last: None,
            buf: Vec::new(),
            fp: Vec::new(),
            prev_fp: HashSet::new(),
            started: None,
        }
    }
}

impl DensityBrush {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stamps the footprint around `(x, y)`, sampling intensity per covered cell (each cell has
    /// its own current ramp index). A cell is stamped only when the footprint newly enters it —
    /// cells still covered from the previous path step are skipped, preserving Buildup's
    /// one-step-per-pass feel at every size. A cell the brush leaves and re-enters counts as a
    /// genuine revisit and advances again, matching the size-1 backtracking behavior.
    fn stamp_cell(&mut self, x: u16, y: u16, ctx: &ToolCtx, doc: &Document) {
        let timing = self.started.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
        let mut fp = std::mem::take(&mut self.fp);
        footprint((x, y), ctx.size, ctx.shape, &mut fp);
        for &(fx, fy) in fp.iter() {
            if self.prev_fp.contains(&(fx, fy)) {
                continue;
            }
            let current = self.stroke.current(fx, fy, doc, ctx.layer);
            let current_ramp_index = ctx.ramp.iter().position(|&c| c == current.ch);
            let sample =
                StrokeSample { position: (fx, fy), timing, current_ramp_index, ramp_len: ctx.ramp.len() };

            let mut density = ctx.density; // Copy — a local mutable copy is fine for &mut self.sample
            let intensity = match &mut density {
                DensityMode::Fixed(f) => f.sample(&sample),
                DensityMode::Buildup(b) => b.sample(&sample),
            };
            let idx = intensity_to_index(intensity, ctx.ramp.len());
            let ch = ctx.ramp.get(idx).copied().unwrap_or(ctx.glyph); // defensive: empty-ramp fallback
            let proposed = Cell { ch, fg: ctx.fg, bg: ctx.bg };
            self.stroke.stamp(fx, fy, proposed, ctx.mask, doc, ctx.layer);
        }
        self.prev_fp.clear();
        self.prev_fp.extend(fp.iter().copied());
        self.fp = fp;
    }
}

impl Tool for DensityBrush {
    fn update(&mut self, ev: ToolEvent, ctx: &ToolCtx, doc: &Document) -> ToolResponse {
        match ev {
            ToolEvent::Press { x, y } => {
                self.stroke.begin();
                self.prev_fp.clear();
                self.started = Some(Instant::now());
                self.stamp_cell(x, y, ctx, doc);
                self.last = Some((x, y));
                ToolResponse::Active
            }
            ToolEvent::Drag { x, y } => {
                let from = self.last.unwrap_or((x, y));
                let mut buf = std::mem::take(&mut self.buf);
                line_cells(from, (x, y), &mut buf);
                // Skip buf[0]: it always duplicates the previous call's (or Press's) last-stamped
                // cell. Without this, a stationary held pointer would re-stamp — and for Buildup,
                // re-advance — the same cell every single frame, since Drag fires every frame the
                // primary button is down, not only on cell change.
                for &(cx, cy) in buf.iter().skip(1) {
                    self.stamp_cell(cx, cy, ctx, doc);
                }
                self.buf = buf;
                self.last = Some((x, y));
                ToolResponse::Active
            }
            ToolEvent::Release => {
                let edit = self.stroke.finish(doc, ctx.layer);
                self.last = None;
                self.prev_fp.clear();
                self.started = None;
                ToolResponse::Commit(edit)
            }
            ToolEvent::Cancel => {
                self.stroke.cancel();
                self.last = None;
                self.prev_fp.clear();
                self.started = None;
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
    use crate::brush::{Buildup, Fixed};
    use crate::model::Rgba;
    use crate::tools::PlaneMask;

    fn ctx(density: DensityMode, ramp: &str) -> ToolCtx {
        ToolCtx {
            layer: 0,
            glyph: '#',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
            mask: PlaneMask::ALL,
            density,
            ramp: ramp.chars().collect(),
            size: 1,
            shape: crate::tools::BrushShape::Square,
        }
    }

    fn ch_at(doc: &Document, x: u16, y: u16) -> char {
        doc.cell(0, x, y).unwrap().ch
    }

    fn apply(doc: &mut Document, resp: ToolResponse) {
        if let ToolResponse::Commit(Some(edit)) = resp {
            let mut history = crate::edit::History::new();
            history.apply(doc, edit);
        }
    }

    #[test]
    fn fixed_mode_stamps_the_same_index_regardless_of_revisits() {
        let mut doc = Document::new(10, 10);
        let tctx = ctx(DensityMode::Fixed(Fixed(0.5)), " .:-=+*#%@"); // len 10, 0.5 -> index 5 '+'
        let mut brush = DensityBrush::new();
        brush.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        brush.update(ToolEvent::Drag { x: 1, y: 0 }, &tctx, &doc);
        brush.update(ToolEvent::Drag { x: 0, y: 0 }, &tctx, &doc); // revisit
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, resp);
        assert_eq!(ch_at(&doc, 0, 0), '+');
        assert_eq!(ch_at(&doc, 1, 0), '+');
    }

    #[test]
    fn dwelling_at_a_stationary_cell_advances_buildup_nothing() {
        // A ramp that does not itself contain a plain space keeps a Blank cell genuinely
        // "off-ramp" (`current_ramp_index == None`) until first touched — unlike the built-in
        // ASCII-shading ramp, whose own lightest character IS a space, making a Blank cell
        // already coincide with ramp index 0 before any touch (see the module-level note on
        // `off_ramp_cell_including_a_glyph_not_on_the_ramp_starts_buildup_at_step_zero` below).
        let mut doc = Document::new(10, 10);
        let tctx = ctx(DensityMode::Buildup(Buildup), "abcd");
        let mut brush = DensityBrush::new();
        brush.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc); // off-ramp -> step 0 -> 'a'
        // Repeated Drag calls at the SAME cell simulate a stationary held pointer (Drag fires
        // every frame while the button is down, not only on cell change). `line_cells` from a
        // point to itself yields a single-element buffer, and the skip-first-element rule means
        // none of these calls re-stamp anything.
        for _ in 0..10 {
            brush.update(ToolEvent::Drag { x: 5, y: 5 }, &tctx, &doc);
        }
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, resp);
        assert_eq!(ch_at(&doc, 5, 5), 'a', "dwelling must not advance past the single Press touch");
    }

    #[test]
    fn straight_drag_advances_each_newly_crossed_cell_exactly_once() {
        let mut doc = Document::new(10, 10);
        let tctx = ctx(DensityMode::Buildup(Buildup), "abcd");
        let mut brush = DensityBrush::new();
        brush.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        brush.update(ToolEvent::Drag { x: 3, y: 0 }, &tctx, &doc);
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, resp);
        // Every cell touched exactly once this stroke, each starting off-ramp: all land on step 0.
        for x in 0..=3u16 {
            assert_eq!(ch_at(&doc, x, 0), 'a');
        }
    }

    #[test]
    fn genuine_revisit_advances_a_cell_again() {
        let mut doc = Document::new(10, 10);
        let tctx = ctx(DensityMode::Buildup(Buildup), "abcd");
        let mut brush = DensityBrush::new();
        brush.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc); // (0,0) -> step 0 'a'
        brush.update(ToolEvent::Drag { x: 1, y: 0 }, &tctx, &doc); // (1,0) -> step 0 'a'
        brush.update(ToolEvent::Drag { x: 2, y: 0 }, &tctx, &doc); // (2,0) -> step 0 'a'
        // Backtracking to (0,0) interpolates back through (1,0): both are genuinely revisited
        // (touched a second time within this stroke) and advance one more step; (2,0) was only
        // ever touched once and stays at step 0.
        brush.update(ToolEvent::Drag { x: 0, y: 0 }, &tctx, &doc);
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, resp);
        assert_eq!(ch_at(&doc, 0, 0), 'b', "revisited cell must advance one more ramp step");
        assert_eq!(ch_at(&doc, 1, 0), 'b', "the backtrack path revisits this cell too");
        assert_eq!(ch_at(&doc, 2, 0), 'a', "touched exactly once, must not advance further");
    }

    #[test]
    fn buildup_continues_across_separate_strokes_by_reading_the_document() {
        let mut doc = Document::new(10, 10);
        let tctx = ctx(DensityMode::Buildup(Buildup), "abcd");
        let mut brush = DensityBrush::new();

        // First stroke: single touch, off-ramp -> step 0 'a'.
        let r1 = brush.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
        let r2 = brush.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, r1);
        apply(&mut doc, r2);
        assert_eq!(ch_at(&doc, 2, 2), 'a');

        // Force the cell onto the ramp at a known index for a clean assertion.
        doc.set_cell(0, 2, 2, Cell { ch: 'b', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }); // index 1

        // Fresh brush + fresh stroke: must read the document's current index, not restart at 0.
        let mut brush2 = DensityBrush::new();
        brush2.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
        let resp = brush2.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, resp);
        assert_eq!(ch_at(&doc, 2, 2), 'c', "a fresh stroke must continue from the doc's current step");
    }

    #[test]
    fn wide_buildup_drag_advances_each_covered_cell_exactly_once_per_pass() {
        let mut doc = Document::new(20, 20);
        let mut tctx = ctx(DensityMode::Buildup(Buildup), "abcd");
        tctx.size = 3;
        let mut brush = DensityBrush::new();
        brush.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
        for x in 3..=8u16 {
            brush.update(ToolEvent::Drag { x, y: 2 }, &tctx, &doc);
        }
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, resp);
        // Consecutive aspect-corrected footprints (6 wide, 3 tall) overlap most of their cells;
        // only newly entered cells advance, so one straight pass leaves the whole swept band
        // (cols 0..=11, rows 1..=3) at step 0 — never saturated.
        for y in 1..=3u16 {
            for x in 0..=11u16 {
                assert_eq!(ch_at(&doc, x, y), 'a', "cell ({x},{y}) must advance exactly once");
            }
        }
    }

    #[test]
    fn wide_buildup_reentry_after_leaving_advances_again() {
        let mut doc = Document::new(30, 30);
        let mut tctx = ctx(DensityMode::Buildup(Buildup), "abcd");
        tctx.size = 3;
        let mut brush = DensityBrush::new();
        // Out and far enough back that the start cell leaves the footprint before re-entry.
        brush.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
        for x in 3..=10u16 {
            brush.update(ToolEvent::Drag { x, y: 2 }, &tctx, &doc);
        }
        for x in (2..=9u16).rev() {
            brush.update(ToolEvent::Drag { x, y: 2 }, &tctx, &doc);
        }
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, resp);
        assert_eq!(ch_at(&doc, 2, 2), 'b', "left and re-entered: advances a second time");
    }

    #[test]
    fn plane_mask_gates_the_computed_glyph_like_pencil() {
        let mut doc = Document::new(10, 10);
        let existing = Cell { ch: 'x', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) };
        doc.set_cell(0, 1, 1, existing);
        let mut tctx = ctx(DensityMode::Fixed(Fixed(1.0)), " .:-=+*#%@");
        tctx.mask = PlaneMask { glyph: false, bg: false };
        let mut brush = DensityBrush::new();
        brush.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &doc);
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Commit(None)), "glyph-off mask must leave the cell untouched");
    }

    #[test]
    fn release_with_no_touches_commits_none() {
        let doc = Document::new(10, 10);
        let tctx = ctx(DensityMode::Fixed(Fixed(1.0)), " .:-=+*#%@");
        let mut brush = DensityBrush::new();
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Commit(None)));
    }

    #[test]
    fn cancel_discards_pending_with_no_doc_mutation() {
        let doc = Document::new(10, 10);
        let before = doc.clone();
        let tctx = ctx(DensityMode::Fixed(Fixed(1.0)), " .:-=+*#%@");
        let mut brush = DensityBrush::new();
        brush.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        assert!(!brush.pending().is_empty());
        let resp = brush.update(ToolEvent::Cancel, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Idle));
        assert!(brush.pending().is_empty());
        assert_eq!(doc, before);
    }

    #[test]
    fn off_ramp_cell_including_a_glyph_not_on_the_ramp_starts_buildup_at_step_zero() {
        let mut doc = Document::new(10, 10);
        // A glyph that exists but isn't a character of the active ramp.
        doc.set_cell(0, 0, 0, Cell { ch: '?', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        let tctx = ctx(DensityMode::Buildup(Buildup), " .:-=+*#%@");
        let mut brush = DensityBrush::new();
        brush.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        let resp = brush.update(ToolEvent::Release, &tctx, &doc);
        apply(&mut doc, resp);
        assert_eq!(ch_at(&doc, 0, 0), ' ', "an off-ramp glyph must be treated as no current index, landing on step 0");
    }
}
