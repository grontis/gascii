//! Stroke pipeline: pointer press/drag/release events reduced to pending, then committed, cell
//! edits. Tools never mutate the `Document` directly — they accumulate `PendingCell`s for
//! an overlay and hand a finished `Edit` to the caller on release.

mod eraser;
mod fill;
mod line;
mod pencil;
mod rect;
mod select;
mod text;

pub use eraser::Eraser;
pub use fill::FloodFill;
pub use line::Line;
pub use pencil::Pencil;
pub use rect::Rectangle;
pub use select::SelectionTool;
pub use text::TextTool;

use std::collections::HashSet;

use crate::clipboard::CellPatch;
use crate::edit::{CellEdit, Edit};
use crate::model::{Cell, Document, Rgba};

/// Filters what a stroke *writes*: glyph / fg / bg, independently. It only ever gates writes —
/// anything that reads or compares cells does so unmasked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlaneMask {
    pub glyph: bool,
    pub fg: bool,
    pub bg: bool,
}

impl PlaneMask {
    pub const ALL: PlaneMask = PlaneMask { glyph: true, fg: true, bg: true };
}

impl Default for PlaneMask {
    /// All planes on — a stroke fully replaces the cell (glyph, fg, and bg), matching the
    /// REXPaint/ANSI-editor convention that drawing on top of existing art doesn't leave stray
    /// old-bg fringe behind. Individual planes can still be toggled off for selective drawing.
    fn default() -> Self {
        PlaneMask::ALL
    }
}

/// Applies `mask` to decide, per plane, whether `proposed` or the pre-existing `before` value
/// wins.
pub fn mask_apply(before: Cell, proposed: Cell, mask: PlaneMask) -> Cell {
    Cell {
        ch: if mask.glyph { proposed.ch } else { before.ch },
        fg: if mask.fg { proposed.fg } else { before.fg },
        bg: if mask.bg { proposed.bg } else { before.bg },
    }
}

/// Read-only draw state a `Tool` needs each event. App-level state — never recorded in history.
#[derive(Clone, Copy, Debug)]
pub struct ToolCtx {
    pub layer: usize,
    pub glyph: char,
    pub fg: Rgba,
    pub bg: Rgba,
    pub mask: PlaneMask,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// UI-agnostic pointer/keyboard gesture, already resolved to a document cell where relevant.
/// Deliberately not `#[non_exhaustive]`: `gascii` (the app crate) constructs these directly via
/// literal syntax, which `#[non_exhaustive]` would forbid from outside the defining crate.
#[derive(Clone, Copy, Debug)]
pub enum ToolEvent {
    Press { x: u16, y: u16 },
    Drag { x: u16, y: u16 },
    Release,
    Cancel,
    Char(char),
    Backspace,
    Enter,
    Arrow(Direction),
    /// Finalize whatever is pending into one `Edit` now; the tool stays active/ready for more
    /// input. Distinct from `Release` (pointer-up) and `Cancel` (discard).
    Commit,
    /// Clear the active selection/float to Blank. Only `SelectionTool` gives this meaning; other
    /// tools ignore it like any other irrelevant event.
    Delete,
}

/// Inclusive cell rectangle (`x0..=x1`, `y0..=y1`), normalized so `x0<=x1` and `y0<=y1`. Shared by
/// `SelectionTool`, the renderer's selection overlay, and `CellPatch::from_region`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellRect {
    pub x0: u16,
    pub y0: u16,
    pub x1: u16,
    pub y1: u16,
}

impl CellRect {
    pub fn from_corners(a: (u16, u16), b: (u16, u16)) -> CellRect {
        CellRect {
            x0: a.0.min(b.0),
            y0: a.1.min(b.1),
            x1: a.0.max(b.0),
            y1: a.1.max(b.1),
        }
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    pub fn width(&self) -> u16 {
        self.x1 - self.x0 + 1
    }

    pub fn height(&self) -> u16 {
        self.y1 - self.y0 + 1
    }
}

/// What the renderer needs beyond `pending` to draw a selection: the marquee outline (current
/// selection rect, or the floating stamp's current position while one is floating) and the
/// lifted-source region to paint as vacated background while a stamp floats over it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelectionView {
    pub marquee: Option<CellRect>,
    pub lifted_source: Option<CellRect>,
}

/// One overlay cell: already the masked *result* cell (what the user will actually get), not the
/// raw proposed cell — so the renderer can stay dumb (blit) and still respect plane toggles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PendingCell {
    pub x: u16,
    pub y: u16,
    pub cell: Cell,
}

#[derive(Debug)]
pub enum ToolResponse {
    Active,
    Idle,
    Commit(Option<Edit>),
}

pub trait Tool {
    fn update(&mut self, ev: ToolEvent, ctx: &ToolCtx, doc: &Document) -> ToolResponse;
    fn pending(&self) -> &[PendingCell];
    /// Called whenever `doc` was just mutated through some path other than this tool's own
    /// `update` calls — currently only a `History::redo` run while a gesture is still pending and
    /// uncommitted. Default no-op: only `TextTool` can straddle an external mutation like this
    /// (its burst spans multiple frames while idle, unlike `FreehandStroke`, which commits
    /// atomically on release), so it's the only implementor that needs to override this.
    fn resync(&mut self, _doc: &Document, _layer: usize) {}
    /// Inject a floating stamp (paste) at `at`. Default no-op; only `SelectionTool` overrides it —
    /// mirrors `resync`'s precedent of a default-no-op hook taking non-`Copy` args that serves a
    /// single implementor.
    fn accept_stamp(&mut self, _patch: CellPatch, _at: (u16, u16), _doc: &Document) {}
    /// Marquee and lifted-source rects for the renderer's selection overlay. Default `None`; only
    /// `SelectionTool` overrides it.
    fn selection_overlay(&self) -> Option<SelectionView> {
        None
    }
}

/// Converts a set of overlay cells into a committed `Edit`, dropping any cell whose masked result
/// already matches the document (so a no-op gesture yields no empty undo entry). Shared by every
/// tool whose commit is "diff the pending overlay against the current document" — fill,
/// rectangle, line.
pub(crate) fn diff_pending(pending: &[PendingCell], doc: &Document, layer: usize) -> Option<Edit> {
    let mut cell_edits = Vec::with_capacity(pending.len());
    for p in pending {
        let before = doc.cell(layer, p.x, p.y).copied().unwrap_or(Cell::BLANK);
        if before == p.cell {
            continue;
        }
        cell_edits.push(CellEdit { layer, x: p.x, y: p.y, before, after: p.cell });
    }
    (!cell_edits.is_empty()).then_some(Edit::Cells(cell_edits))
}

/// Interpolates cell coordinates from `a` to `b` inclusive, 8-connected, so fast drags don't skip
/// cells regardless of zoom (interpolation happens in cell space, not pixel space). Writes into
/// the caller-provided `out` (cleared first) to avoid a per-drag allocation.
pub fn line_cells(a: (u16, u16), b: (u16, u16), out: &mut Vec<(u16, u16)>) {
    out.clear();
    let (x0, y0) = (a.0 as i32, a.1 as i32);
    let (x1, y1) = (b.0 as i32, b.1 as i32);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        out.push((x as u16, y as u16));
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// Shared freehand accumulator behind pencil/eraser — they differ only in the proposed cell.
pub(crate) struct FreehandStroke {
    pending: Vec<PendingCell>,
    seen: HashSet<(u16, u16)>,
    last: Option<(u16, u16)>,
    buf: Vec<(u16, u16)>,
}

impl FreehandStroke {
    pub(crate) fn new() -> Self {
        FreehandStroke {
            pending: Vec::new(),
            seen: HashSet::new(),
            last: None,
            buf: Vec::new(),
        }
    }

    fn begin(&mut self) {
        self.pending.clear();
        self.seen.clear();
        self.last = None;
    }

    /// First-write-wins: a cell revisited within one stroke is not re-stamped, so a constant
    /// proposed cell (pencil/eraser) never yields a duplicate `CellEdit`. Returns whether `(x, y)`
    /// was in-bounds (and therefore a candidate for tracking as the stroke's last-visited cell),
    /// regardless of whether it was actually a fresh stamp or a `seen`-set repeat.
    fn stamp(&mut self, x: u16, y: u16, proposed: Cell, mask: PlaneMask, doc: &Document, layer: usize) -> bool {
        if !doc.in_bounds(x, y) {
            return false;
        }
        if !self.seen.insert((x, y)) {
            return true;
        }
        let before = doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK);
        let after = mask_apply(before, proposed, mask);
        self.pending.push(PendingCell { x, y, cell: after });
        true
    }

    pub(crate) fn press(
        &mut self,
        x: u16,
        y: u16,
        proposed: Cell,
        mask: PlaneMask,
        doc: &Document,
        layer: usize,
    ) {
        self.begin();
        if self.stamp(x, y, proposed, mask, doc, layer) {
            self.last = Some((x, y));
        }
    }

    pub(crate) fn drag(
        &mut self,
        x: u16,
        y: u16,
        proposed: Cell,
        mask: PlaneMask,
        doc: &Document,
        layer: usize,
    ) {
        let from = self.last.unwrap_or((x, y));
        let mut buf = std::mem::take(&mut self.buf);
        line_cells(from, (x, y), &mut buf);
        for &(cx, cy) in buf.iter() {
            self.stamp(cx, cy, proposed, mask, doc, layer);
        }
        self.buf = buf;
        self.last = Some((x, y));
    }

    /// Finishes the stroke: cells whose masked result equals the pre-stroke value are dropped, so
    /// a no-op stroke (re-painting identical content) yields `None` (no empty undo entry).
    pub(crate) fn finish(&mut self, doc: &Document, layer: usize) -> Option<Edit> {
        let mut cell_edits = Vec::with_capacity(self.pending.len());
        for p in &self.pending {
            let before = doc.cell(layer, p.x, p.y).copied().unwrap_or(Cell::BLANK);
            if before == p.cell {
                continue;
            }
            cell_edits.push(CellEdit { layer, x: p.x, y: p.y, before, after: p.cell });
        }
        self.pending.clear();
        self.seen.clear();
        self.last = None;
        if cell_edits.is_empty() {
            None
        } else {
            Some(Edit::Cells(cell_edits))
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.pending.clear();
        self.seen.clear();
        self.last = None;
    }

    pub(crate) fn pending(&self) -> &[PendingCell] {
        &self.pending
    }
}

/// Reads a cell's fg and bg for the eyedropper. Produces no `Edit` — it feeds app color state,
/// not the document — so it is intentionally not a `Tool`.
pub fn eyedrop(cell: &Cell) -> (Rgba, Rgba) {
    (cell.fg, cell.bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(ch: char, fg: Rgba, bg: Rgba) -> Cell {
        Cell { ch, fg, bg }
    }

    #[test]
    fn mask_apply_all_false_is_identity_over_before() {
        let before = c('a', Rgba::WHITE, Rgba::TRANSPARENT);
        let proposed = c('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        let mask = PlaneMask { glyph: false, fg: false, bg: false };
        assert_eq!(mask_apply(before, proposed, mask), before);
    }

    #[test]
    fn mask_apply_all_true_is_full_replace() {
        let before = c('a', Rgba::WHITE, Rgba::TRANSPARENT);
        let proposed = c('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        assert_eq!(mask_apply(before, proposed, PlaneMask::ALL), proposed);
    }

    #[test]
    fn mask_apply_glyph_only() {
        let before = c('a', Rgba::WHITE, Rgba::TRANSPARENT);
        let proposed = c('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        let mask = PlaneMask { glyph: true, fg: false, bg: false };
        let result = mask_apply(before, proposed, mask);
        assert_eq!(result.ch, 'b');
        assert_eq!(result.fg, before.fg);
        assert_eq!(result.bg, before.bg);
    }

    #[test]
    fn mask_apply_fg_only() {
        let before = c('a', Rgba::WHITE, Rgba::TRANSPARENT);
        let proposed = c('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        let mask = PlaneMask { glyph: false, fg: true, bg: false };
        let result = mask_apply(before, proposed, mask);
        assert_eq!(result.ch, before.ch);
        assert_eq!(result.fg, proposed.fg);
        assert_eq!(result.bg, before.bg);
    }

    #[test]
    fn mask_apply_bg_only() {
        let before = c('a', Rgba::WHITE, Rgba::TRANSPARENT);
        let proposed = c('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        let mask = PlaneMask { glyph: false, fg: false, bg: true };
        let result = mask_apply(before, proposed, mask);
        assert_eq!(result.ch, before.ch);
        assert_eq!(result.fg, before.fg);
        assert_eq!(result.bg, proposed.bg);
    }

    #[test]
    fn mask_apply_glyph_fg_combo() {
        let before = c('a', Rgba::WHITE, Rgba::TRANSPARENT);
        let proposed = c('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        let mask = PlaneMask { glyph: true, fg: true, bg: false };
        let result = mask_apply(before, proposed, mask);
        assert_eq!(result.ch, 'b');
        assert_eq!(result.fg, proposed.fg);
        assert_eq!(result.bg, before.bg);
    }

    #[test]
    fn mask_apply_glyph_bg_combo() {
        let before = c('a', Rgba::WHITE, Rgba::TRANSPARENT);
        let proposed = c('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        let mask = PlaneMask { glyph: true, fg: false, bg: true };
        let result = mask_apply(before, proposed, mask);
        assert_eq!(result.ch, 'b');
        assert_eq!(result.fg, before.fg);
        assert_eq!(result.bg, proposed.bg);
    }

    #[test]
    fn mask_apply_fg_bg_combo() {
        let before = c('a', Rgba::WHITE, Rgba::TRANSPARENT);
        let proposed = c('b', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255));
        let mask = PlaneMask { glyph: false, fg: true, bg: true };
        let result = mask_apply(before, proposed, mask);
        assert_eq!(result.ch, before.ch);
        assert_eq!(result.fg, proposed.fg);
        assert_eq!(result.bg, proposed.bg);
    }

    #[test]
    fn plane_mask_default_is_all_planes_on() {
        let mask = PlaneMask::default();
        assert!(mask.glyph);
        assert!(mask.fg);
        assert!(mask.bg);
        assert_eq!(mask, PlaneMask::ALL);
    }

    // --- line_cells (Bresenham) ---

    fn set_of(cells: &[(u16, u16)]) -> std::collections::HashSet<(u16, u16)> {
        cells.iter().copied().collect()
    }

    #[test]
    fn line_cells_single_point() {
        let mut out = Vec::new();
        line_cells((5, 5), (5, 5), &mut out);
        assert_eq!(out, vec![(5, 5)]);
    }

    #[test]
    fn line_cells_horizontal() {
        let mut out = Vec::new();
        line_cells((2, 4), (6, 4), &mut out);
        assert_eq!(out, vec![(2, 4), (3, 4), (4, 4), (5, 4), (6, 4)]);
    }

    #[test]
    fn line_cells_vertical() {
        let mut out = Vec::new();
        line_cells((4, 2), (4, 6), &mut out);
        assert_eq!(out, vec![(4, 2), (4, 3), (4, 4), (4, 5), (4, 6)]);
    }

    #[test]
    fn line_cells_diagonal_45deg() {
        let mut out = Vec::new();
        line_cells((0, 0), (4, 4), &mut out);
        assert_eq!(out, vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4)]);
    }

    #[test]
    fn line_cells_endpoints_inclusive() {
        let mut out = Vec::new();
        line_cells((1, 1), (10, 3), &mut out);
        assert_eq!(*out.first().unwrap(), (1, 1));
        assert_eq!(*out.last().unwrap(), (10, 3));
    }

    #[test]
    fn line_cells_adjacency_no_gaps() {
        let mut out = Vec::new();
        line_cells((1, 1), (17, 6), &mut out);
        for w in out.windows(2) {
            let (x0, y0) = w[0];
            let (x1, y1) = w[1];
            let dx = (x1 as i32 - x0 as i32).abs();
            let dy = (y1 as i32 - y0 as i32).abs();
            assert!(dx <= 1 && dy <= 1, "gap between {:?} and {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn line_cells_reversibility() {
        let mut fwd = Vec::new();
        let mut bwd = Vec::new();
        line_cells((2, 9), (13, 2), &mut fwd);
        line_cells((13, 2), (2, 9), &mut bwd);
        assert_eq!(set_of(&fwd), set_of(&bwd));
    }

    #[test]
    fn line_cells_known_non_45_slope() {
        // dx=8, dy=3 — a shallow, non-45deg slope.
        let mut out = Vec::new();
        line_cells((0, 0), (8, 3), &mut out);
        assert_eq!(*out.first().unwrap(), (0, 0));
        assert_eq!(*out.last().unwrap(), (8, 3));
        // No gaps, monotonic in x.
        for w in out.windows(2) {
            assert!(w[1].0 >= w[0].0);
            let dx = w[1].0 as i32 - w[0].0 as i32;
            let dy = (w[1].1 as i32 - w[0].1 as i32).abs();
            assert!(dx <= 1 && dy <= 1);
        }
    }

    #[test]
    fn line_cells_steep_slope_no_gaps() {
        // dx=3, dy=8 — a steep, y-dominant slope (mirror of `line_cells_known_non_45_slope`).
        let mut out = Vec::new();
        line_cells((0, 0), (3, 8), &mut out);
        assert_eq!(*out.first().unwrap(), (0, 0));
        assert_eq!(*out.last().unwrap(), (3, 8));
        // No gaps, monotonic in y.
        for w in out.windows(2) {
            assert!(w[1].1 >= w[0].1);
            let dx = (w[1].0 as i32 - w[0].0 as i32).abs();
            let dy = w[1].1 as i32 - w[0].1 as i32;
            assert!(dx <= 1 && dy <= 1, "gap between {:?} and {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn eyedrop_returns_fg_and_bg() {
        let cell = c('x', Rgba(9, 8, 7, 255), Rgba(1, 2, 3, 128));
        assert_eq!(eyedrop(&cell), (cell.fg, cell.bg));
    }
}
