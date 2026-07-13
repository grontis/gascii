//! Stroke pipeline: pointer press/drag/release events reduced to pending, then committed, cell
//! edits. Tools never mutate the `Document` directly — they accumulate `PendingCell`s for
//! an overlay and hand a finished `Edit` to the caller on release.

mod density_brush;
mod eraser;
mod fill;
mod line;
mod pencil;
mod rect;
mod select;
mod text;

pub use density_brush::DensityBrush;
pub use eraser::Eraser;
pub use fill::FloodFill;
pub use line::Line;
pub use pencil::Pencil;
pub use rect::Rectangle;
pub use select::SelectionTool;
pub use text::TextTool;

use std::collections::HashMap;

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

/// Footprint shape of a sized stroke: the cells one stamp covers around its center.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BrushShape {
    #[default]
    Square,
    Circle,
}

/// Upper bound for `ToolCtx::size`.
pub const MAX_TOOL_SIZE: u16 = 16;

/// Cells covered by one `size`-wide stamp centered on `center`, written into the caller-provided
/// `out` (cleared first). A size-N stamp spans an N×N box around the center (an even N biases
/// right/down, since a cell grid has no true center cell for it); `Circle` keeps only cells within
/// a disc of diameter N, shrunk a touch so the disc sheds its bounding box's corners (a size-3
/// circle is a plus, not the full 3×3). Cells that would fall off the u16 grid are dropped here;
/// document-bounds clipping stays the caller's job.
pub fn footprint(center: (u16, u16), size: u16, shape: BrushShape, out: &mut Vec<(u16, u16)>) {
    out.clear();
    let size = size.clamp(1, MAX_TOOL_SIZE) as i32;
    let lo = -((size - 1) / 2);
    let hi = size / 2;
    let c = (lo + hi) as f32 / 2.0;
    let r = size as f32 / 2.0 - 0.1;
    for dy in lo..=hi {
        for dx in lo..=hi {
            if shape == BrushShape::Circle {
                let (fx, fy) = (dx as f32 - c, dy as f32 - c);
                if fx * fx + fy * fy > r * r {
                    continue;
                }
            }
            let x = center.0 as i32 + dx;
            let y = center.1 as i32 + dy;
            if x < 0 || y < 0 || x > u16::MAX as i32 || y > u16::MAX as i32 {
                continue;
            }
            out.push((x as u16, y as u16));
        }
    }
}

/// Read-only draw state a `Tool` needs each event. App-level state — never recorded in history.
/// Not `Copy` — `ramp` is an owned `Vec<char>` (only the density brush reads `density`/`ramp`;
/// every other tool ignores them).
#[derive(Clone, Debug)]
pub struct ToolCtx {
    pub layer: usize,
    pub glyph: char,
    pub fg: Rgba,
    pub bg: Rgba,
    pub mask: PlaneMask,
    pub density: crate::brush::DensityMode,
    pub ramp: Vec<char>,
    /// Stamp width in cells for the sized tools (pencil, eraser, line, density brush); every
    /// other tool ignores it. Clamped to `1..=MAX_TOOL_SIZE` at the footprint.
    pub size: u16,
    /// Footprint shape for the sized tools; ignored wherever `size` is.
    pub shape: BrushShape,
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
    /// Cell where a text caret should render while this tool has one active. May sit one column
    /// past the document's right edge (a full row typed) — display clamping is the renderer's
    /// job. Default `None`; only `TextTool` overrides it.
    fn caret(&self) -> Option<(u16, u16)> {
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
        // The document can shrink between stamp and commit (a resize applied while a right-click
        // stroke is in flight) — a cell that fell outside is dropped, not committed as a phantom
        // out-of-bounds edit with a fabricated Blank `before`.
        if !doc.in_bounds(p.x, p.y) {
            continue;
        }
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

/// Shared freehand accumulator behind pencil/eraser/the density brush. Pencil/eraser propose a
/// constant cell for the whole gesture; the density brush's proposed cell varies per revisit
/// (Buildup advances one ramp step each pass), so a revisited cell is always recomputed and
/// (over)written rather than deduped by first touch.
pub(crate) struct FreehandStroke {
    pending: Vec<PendingCell>,
    /// `(x,y) -> position in `pending``, so a revisit updates the existing entry in place instead
    /// of appending a duplicate.
    index: HashMap<(u16, u16), usize>,
    /// The document's value at first touch this stroke, pinned so every revisit's `mask_apply`
    /// still references the true pre-stroke cell, not an intermediate in-stroke write.
    before: HashMap<(u16, u16), Cell>,
    last: Option<(u16, u16)>,
    buf: Vec<(u16, u16)>,
    fp: Vec<(u16, u16)>,
}

impl FreehandStroke {
    pub(crate) fn new() -> Self {
        FreehandStroke {
            pending: Vec::new(),
            index: HashMap::new(),
            before: HashMap::new(),
            last: None,
            buf: Vec::new(),
            fp: Vec::new(),
        }
    }

    fn begin(&mut self) {
        self.pending.clear();
        self.index.clear();
        self.before.clear();
        self.last = None;
    }

    /// Always recomputes and (over)writes `(x,y)`'s pending entry. Behaviorally identical to the
    /// old first-write-wins dedup for a constant `proposed` (pencil/eraser: overwriting with the
    /// same masked result is a no-op); load-bearing for a `proposed` that varies per revisit
    /// (the density brush). Returns whether `(x, y)` was in-bounds.
    fn stamp(&mut self, x: u16, y: u16, proposed: Cell, mask: PlaneMask, doc: &Document, layer: usize) -> bool {
        if !doc.in_bounds(x, y) {
            return false;
        }
        let before = *self
            .before
            .entry((x, y))
            .or_insert_with(|| doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK));
        let after = mask_apply(before, proposed, mask);
        match self.index.get(&(x, y)) {
            Some(&i) => self.pending[i].cell = after,
            None => {
                self.index.insert((x, y), self.pending.len());
                self.pending.push(PendingCell { x, y, cell: after });
            }
        }
        true
    }

    /// The stroke's in-progress value for `(x,y)`: the pending overlay's value if touched already
    /// this stroke, else the document's untouched value. What `Buildup` reads to know "one step
    /// higher than what."
    pub(crate) fn current(&self, x: u16, y: u16, doc: &Document, layer: usize) -> Cell {
        self.index
            .get(&(x, y))
            .map(|&i| self.pending[i].cell)
            .unwrap_or_else(|| doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK))
    }

    /// Stamps the full `ctx.size`/`ctx.shape` footprint around `(x, y)` — the sized-tool
    /// counterpart of a single `stamp`.
    fn stamp_footprint(&mut self, x: u16, y: u16, proposed: Cell, ctx: &ToolCtx, doc: &Document) {
        let mut fp = std::mem::take(&mut self.fp);
        footprint((x, y), ctx.size, ctx.shape, &mut fp);
        for &(fx, fy) in fp.iter() {
            self.stamp(fx, fy, proposed, ctx.mask, doc, ctx.layer);
        }
        self.fp = fp;
    }

    pub(crate) fn press(&mut self, x: u16, y: u16, proposed: Cell, ctx: &ToolCtx, doc: &Document) {
        self.begin();
        self.stamp_footprint(x, y, proposed, ctx, doc);
        if doc.in_bounds(x, y) {
            self.last = Some((x, y));
        }
    }

    pub(crate) fn drag(&mut self, x: u16, y: u16, proposed: Cell, ctx: &ToolCtx, doc: &Document) {
        let from = self.last.unwrap_or((x, y));
        let mut buf = std::mem::take(&mut self.buf);
        line_cells(from, (x, y), &mut buf);
        // Skip buf[0]: it always duplicates the previous call's (or Press's) last-stamped cell,
        // whose footprint is already fully stamped — re-stamping is a no-op for the constant
        // proposed cell here, just size²-cells' worth of wasted hash traffic every frame the
        // pointer holds still (Drag fires every frame the button is down, not only on movement).
        for &(cx, cy) in buf.iter().skip(1) {
            self.stamp_footprint(cx, cy, proposed, ctx, doc);
        }
        self.buf = buf;
        self.last = Some((x, y));
    }

    /// Finishes the stroke: cells whose masked result equals the pre-stroke value are dropped, so
    /// a no-op stroke (re-painting identical content) yields `None` (no empty undo entry).
    pub(crate) fn finish(&mut self, doc: &Document, layer: usize) -> Option<Edit> {
        let mut cell_edits = Vec::with_capacity(self.pending.len());
        for p in &self.pending {
            // Same shrink-between-stamp-and-commit guard as `diff_pending`.
            if !doc.in_bounds(p.x, p.y) {
                continue;
            }
            let before = doc.cell(layer, p.x, p.y).copied().unwrap_or(Cell::BLANK);
            if before == p.cell {
                continue;
            }
            cell_edits.push(CellEdit { layer, x: p.x, y: p.y, before, after: p.cell });
        }
        self.pending.clear();
        self.index.clear();
        self.before.clear();
        self.last = None;
        if cell_edits.is_empty() {
            None
        } else {
            Some(Edit::Cells(cell_edits))
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.pending.clear();
        self.index.clear();
        self.before.clear();
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

    // --- shrink-between-stamp-and-commit guards ---

    fn sized_ctx() -> ToolCtx {
        ToolCtx {
            layer: 0,
            glyph: '#',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
            mask: PlaneMask::ALL,
            density: crate::brush::DensityMode::Fixed(crate::brush::Fixed(1.0)),
            ramp: Vec::new(),
            size: 1,
            shape: BrushShape::Square,
        }
    }

    #[test]
    fn freehand_finish_drops_cells_beyond_a_shrunken_document() {
        let big = Document::new(10, 10);
        let small = Document::new(5, 5);
        let ctx = sized_ctx();
        let proposed = c('#', Rgba::WHITE, Rgba::TRANSPARENT);
        let mut stroke = FreehandStroke::new();
        stroke.press(2, 2, proposed, &ctx, &big);
        stroke.drag(8, 2, proposed, &ctx, &big);
        let edit = stroke.finish(&small, 0).expect("in-bounds cells still commit");
        let Edit::Cells(cells) = edit else { panic!("expected Edit::Cells") };
        assert!(cells.iter().all(|e| e.x < 5 && e.y < 5), "no phantom out-of-bounds edits");
        let xs: Vec<u16> = cells.iter().map(|e| e.x).collect();
        assert_eq!(xs.len(), 3, "only the surviving columns 2..=4 commit");
    }

    #[test]
    fn diff_pending_drops_cells_beyond_a_shrunken_document() {
        let small = Document::new(5, 5);
        let pending = vec![
            PendingCell { x: 2, y: 2, cell: c('#', Rgba::WHITE, Rgba::TRANSPARENT) },
            PendingCell { x: 8, y: 2, cell: c('#', Rgba::WHITE, Rgba::TRANSPARENT) },
        ];
        let edit = diff_pending(&pending, &small, 0).expect("the in-bounds cell still commits");
        let Edit::Cells(cells) = edit else { panic!("expected Edit::Cells") };
        assert_eq!(cells.len(), 1);
        assert_eq!((cells[0].x, cells[0].y), (2, 2));
    }

    // --- footprint ---

    #[test]
    fn footprint_size_1_is_the_center_cell_for_both_shapes() {
        let mut out = Vec::new();
        for shape in [BrushShape::Square, BrushShape::Circle] {
            footprint((5, 5), 1, shape, &mut out);
            assert_eq!(out, vec![(5, 5)]);
        }
    }

    #[test]
    fn footprint_size_3_square_is_the_full_3x3_box() {
        let mut out = Vec::new();
        footprint((5, 5), 3, BrushShape::Square, &mut out);
        assert_eq!(out.len(), 9);
        for x in 4..=6u16 {
            for y in 4..=6u16 {
                assert!(out.contains(&(x, y)));
            }
        }
    }

    #[test]
    fn footprint_size_3_circle_is_a_plus_not_the_full_box() {
        let mut out = Vec::new();
        footprint((5, 5), 3, BrushShape::Circle, &mut out);
        let cells = set_of(&out);
        assert_eq!(cells, set_of(&[(5, 5), (4, 5), (6, 5), (5, 4), (5, 6)]));
    }

    #[test]
    fn footprint_even_size_biases_right_and_down() {
        let mut out = Vec::new();
        footprint((5, 5), 2, BrushShape::Square, &mut out);
        assert_eq!(set_of(&out), set_of(&[(5, 5), (6, 5), (5, 6), (6, 6)]));
    }

    #[test]
    fn footprint_clips_at_the_grid_origin() {
        let mut out = Vec::new();
        footprint((0, 0), 3, BrushShape::Square, &mut out);
        assert_eq!(set_of(&out), set_of(&[(0, 0), (1, 0), (0, 1), (1, 1)]));
    }

    #[test]
    fn footprint_circle_size_5_sheds_bounding_box_corners() {
        let mut out = Vec::new();
        footprint((10, 10), 5, BrushShape::Circle, &mut out);
        let cells = set_of(&out);
        assert!(!cells.contains(&(8, 8)), "corner must be outside the disc");
        assert!(cells.contains(&(8, 10)), "edge midpoints are inside");
        assert!(cells.contains(&(9, 9)), "inner diagonal is inside");
    }

    #[test]
    fn eyedrop_returns_fg_and_bg() {
        let cell = c('x', Rgba(9, 8, 7, 255), Rgba(1, 2, 3, 128));
        assert_eq!(eyedrop(&cell), (cell.fg, cell.bg));
    }
}
