//! Click-to-place-cursor, then type: a keyboard-driven `Tool`. A whole click-through-interruption
//! typing session coalesces into one `Edit`, the same "one gesture, one undo entry" contract
//! freehand strokes already honor.

use std::collections::HashMap;

use super::{mask_apply, Direction, PendingCell, PlaneMask, Tool, ToolCtx, ToolEvent, ToolResponse};
use crate::edit::{CellEdit, Edit};
use crate::model::{Cell, Document};

/// Differs from `FreehandStroke` in one essential way: a freehand stroke's proposed cell is
/// constant across the whole gesture, so first-write-wins dedup is correct. A text burst's
/// proposed cell changes per keystroke (typing `'a'` then backspacing and typing `'b'` at the
/// same cell must show `'b'`), so writes update in place via `index` rather than skipping
/// revisits.
#[derive(Default)]
pub(crate) struct TextBurst {
    pending: Vec<PendingCell>,
    index: HashMap<(u16, u16), usize>,
    before: HashMap<(u16, u16), Cell>,
    /// Each pending entry's `(proposed, mask)` inputs, aligned with `pending` — what `resync`
    /// recomposes from when the document changes underneath the burst. See `resync_pending`.
    sources: Vec<(Cell, PlaneMask)>,
}

impl TextBurst {
    fn write(&mut self, x: u16, y: u16, proposed: Cell, mask: PlaneMask, doc: &Document, layer: usize) {
        if !doc.in_bounds(x, y) {
            return;
        }
        let doc_before = doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK);
        // mask_apply always references doc_before (the pre-burst value), never a prior in-burst
        // write, so a masked-off plane shows the untouched original regardless of how many times
        // the unmasked plane(s) get overwritten within one burst.
        self.before.entry((x, y)).or_insert(doc_before);
        let masked = mask_apply(doc_before, proposed, mask);
        if let Some(&i) = self.index.get(&(x, y)) {
            self.pending[i].cell = masked;
            self.sources[i] = (proposed, mask);
        } else {
            self.index.insert((x, y), self.pending.len());
            self.pending.push(PendingCell { x, y, cell: masked });
            self.sources.push((proposed, mask));
        }
    }

    fn finish(&mut self, layer: usize) -> Option<Edit> {
        let mut cell_edits = Vec::with_capacity(self.pending.len());
        for p in &self.pending {
            let before = self.before[&(p.x, p.y)];
            if before == p.cell {
                continue;
            }
            cell_edits.push(CellEdit { layer, x: p.x, y: p.y, before, after: p.cell });
        }
        self.pending.clear();
        self.index.clear();
        self.before.clear();
        self.sources.clear();
        (!cell_edits.is_empty()).then_some(Edit::Cells(cell_edits))
    }

    fn pending(&self) -> &[PendingCell] {
        &self.pending
    }

    /// Re-pins every already-touched cell's `before` to `doc`'s current value and recomposes its
    /// pending result — see `resync_pending` for why the recompose half is load-bearing. Must be
    /// called whenever `doc` changes underneath this burst via a path other than the burst's own
    /// writes (a redo, or another binding's commit or flush).
    fn resync(&mut self, doc: &Document, layer: usize) {
        super::resync_pending(&mut self.before, &self.index, &mut self.pending, &self.sources, doc, layer);
    }
}

/// Click places a cursor; typing writes width-validated glyphs through the plane mask at the
/// cursor, advancing right (no wrap — stops at the right edge); Backspace deletes leftward and
/// no-ops at the anchor column; Enter returns to the anchor column on the next row (stops, cursor
/// goes inert, at the bottom edge); arrows navigate without writing.
#[derive(Default)]
pub struct TextTool {
    cursor: Option<(u16, u16)>,
    start_x: u16,
    burst: TextBurst,
}

impl TextTool {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Tool for TextTool {
    fn update(&mut self, ev: ToolEvent, ctx: &ToolCtx, doc: &Document) -> ToolResponse {
        match ev {
            ToolEvent::Press { x, y } => {
                if !doc.in_bounds(x, y) {
                    return ToolResponse::Active;
                }
                let edit = self.burst.finish(ctx.layer); // flush any prior session in the same call
                self.cursor = Some((x, y));
                self.start_x = x;
                ToolResponse::Commit(edit)
            }
            ToolEvent::Char(ch) => {
                let Some((cx, cy)) = self.cursor else { return ToolResponse::Idle };
                if crate::palette::validate_width(ch).is_err() {
                    return ToolResponse::Active; // rejected: cursor does not advance
                }
                if cx >= doc.width {
                    return ToolResponse::Active; // stopped at right edge, no wrap
                }
                let proposed = Cell { ch, fg: ctx.fg, bg: ctx.bg };
                self.burst.write(cx, cy, proposed, ctx.mask, doc, ctx.layer);
                self.cursor = Some((cx + 1, cy));
                ToolResponse::Active
            }
            ToolEvent::Backspace => {
                let Some((cx, cy)) = self.cursor else { return ToolResponse::Idle };
                if cx == self.start_x {
                    return ToolResponse::Active; // anchor column: no-op
                }
                let nx = cx - 1;
                self.burst.write(nx, cy, Cell::BLANK, ctx.mask, doc, ctx.layer);
                self.cursor = Some((nx, cy));
                ToolResponse::Active
            }
            ToolEvent::Enter => {
                let Some((_, cy)) = self.cursor else { return ToolResponse::Idle };
                self.cursor = if cy + 1 < doc.height {
                    Some((self.start_x, cy + 1))
                } else {
                    None // stop at bottom edge, same policy as the right edge
                };
                ToolResponse::Active
            }
            ToolEvent::Arrow(dir) => {
                let Some((cx, cy)) = self.cursor else { return ToolResponse::Idle };
                self.cursor = Some(match dir {
                    Direction::Left => (cx.saturating_sub(1), cy),
                    Direction::Right => ((cx + 1).min(doc.width.saturating_sub(1)), cy),
                    Direction::Up => (cx, cy.saturating_sub(1)),
                    Direction::Down => (cx, (cy + 1).min(doc.height.saturating_sub(1))),
                });
                ToolResponse::Active // pure navigation — never touches the burst
            }
            ToolEvent::Commit => ToolResponse::Commit(self.burst.finish(ctx.layer)), // cursor unchanged, tool stays active
            ToolEvent::Cancel => {
                self.burst = TextBurst::default();
                self.cursor = None;
                ToolResponse::Idle
            }
            ToolEvent::Drag { .. } | ToolEvent::Release | ToolEvent::Delete => ToolResponse::Active, // irrelevant here
        }
    }

    fn pending(&self) -> &[PendingCell] {
        self.burst.pending()
    }

    fn resync(&mut self, doc: &Document, layer: usize) {
        self.burst.resync(doc, layer);
    }

    fn caret(&self) -> Option<(u16, u16)> {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgba;

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

    fn commit_edit(resp: ToolResponse) -> Option<Edit> {
        match resp {
            ToolResponse::Commit(edit) => edit,
            other => panic!("expected Commit, got {other:?}"),
        }
    }

    #[test]
    fn click_places_cursor_and_char_writes_and_advances() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
        tool.update(ToolEvent::Char('a'), &tctx, &doc);
        assert_eq!(tool.pending().len(), 1);
        assert_eq!(tool.pending()[0], PendingCell { x: 5, y: 5, cell: Cell { ch: 'a', fg: tctx.fg, bg: tctx.bg } });
    }

    #[test]
    fn typing_abc_then_commit_yields_one_edit_with_three_cell_edits() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        for ch in ['a', 'b', 'c'] {
            tool.update(ToolEvent::Char(ch), &tctx, &doc);
        }
        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        let edit = commit_edit(resp).expect("expected a committed edit");
        let Edit::Cells(cells) = edit else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 3);
        let mut chars: Vec<char> = cells.iter().map(|c| c.after.ch).collect();
        chars.sort();
        assert_eq!(chars, vec!['a', 'b', 'c']);
    }

    #[test]
    fn enter_returns_to_start_x_not_column_zero() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 5, y: 0 }, &tctx, &doc);
        tool.update(ToolEvent::Char('a'), &tctx, &doc); // cursor now (6, 0)
        tool.update(ToolEvent::Enter, &tctx, &doc);
        tool.update(ToolEvent::Char('b'), &tctx, &doc);
        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        let edit = commit_edit(resp).unwrap();
        let Edit::Cells(cells) = edit else { panic!("expected an Edit::Cells") };
        let b_cell = cells.iter().find(|c| c.after.ch == 'b').unwrap();
        assert_eq!((b_cell.x, b_cell.y), (5, 1), "Enter must return to the anchor column, not 0");
    }

    #[test]
    fn arrow_only_session_yields_commit_none() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
        tool.update(ToolEvent::Arrow(Direction::Right), &tctx, &doc);
        tool.update(ToolEvent::Arrow(Direction::Down), &tctx, &doc);
        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        assert!(commit_edit(resp).is_none());
    }

    #[test]
    fn backspace_deletes_previous_cell_and_moves_left() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &doc);
        tool.update(ToolEvent::Char('x'), &tctx, &doc); // cursor at (3,2)
        tool.update(ToolEvent::Backspace, &tctx, &doc); // deletes (2,2), cursor back to (2,2)
        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        assert!(commit_edit(resp).is_none(), "typed then backspaced back to Blank is a no-op edit");
    }

    #[test]
    fn backspace_at_anchor_column_is_a_no_op() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 4, y: 4 }, &tctx, &doc);
        tool.update(ToolEvent::Backspace, &tctx, &doc);
        assert!(tool.pending().is_empty());
        // cursor unchanged: typing next still lands at (4,4)
        tool.update(ToolEvent::Char('z'), &tctx, &doc);
        assert_eq!(tool.pending()[0].x, 4);
        assert_eq!(tool.pending()[0].y, 4);
    }

    #[test]
    fn typing_past_the_last_column_drops_further_chars_no_wrap() {
        let doc = Document::new(3, 3);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 2, y: 0 }, &tctx, &doc); // last column
        tool.update(ToolEvent::Char('a'), &tctx, &doc); // cursor -> (3,0), out of bounds
        tool.update(ToolEvent::Char('b'), &tctx, &doc); // no-op: cursor.x >= width
        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        let edit = commit_edit(resp).unwrap();
        let Edit::Cells(cells) = edit else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].after.ch, 'a', "second Char must not overwrite — cursor already past width");
    }

    #[test]
    fn enter_at_last_row_goes_inert() {
        let doc = Document::new(5, 1);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        tool.update(ToolEvent::Enter, &tctx, &doc);
        let resp = tool.update(ToolEvent::Char('x'), &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Idle));
        assert!(tool.pending().is_empty());
    }

    #[test]
    fn single_width_non_ascii_char_accepted() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        tool.update(ToolEvent::Char('│'), &tctx, &doc);
        assert_eq!(tool.pending().len(), 1);
        assert_eq!(tool.pending()[0].cell.ch, '│');
    }

    #[test]
    fn wide_and_combining_chars_rejected() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        for ch in ['😀', '\u{0301}'] {
            let resp = tool.update(ToolEvent::Char(ch), &tctx, &doc);
            assert!(matches!(resp, ToolResponse::Active));
        }
        assert!(tool.pending().is_empty());
    }

    #[test]
    fn click_away_commits_old_burst_and_relocates_cursor() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        tool.update(ToolEvent::Char('a'), &tctx, &doc);
        let resp = tool.update(ToolEvent::Press { x: 10, y: 10 }, &tctx, &doc);
        let edit = commit_edit(resp).expect("click-away must flush the old burst");
        let Edit::Cells(cells) = edit else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 1);
        assert_eq!((cells[0].x, cells[0].y), (0, 0));

        // Cursor is now at the new cell.
        tool.update(ToolEvent::Char('z'), &tctx, &doc);
        assert_eq!(tool.pending()[0].x, 10);
        assert_eq!(tool.pending()[0].y, 10);
    }

    #[test]
    fn mid_burst_commit_stays_active_and_a_following_char_starts_a_second_burst() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        tool.update(ToolEvent::Char('a'), &tctx, &doc); // cursor -> (1,0)
        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        let first_edit = commit_edit(resp).unwrap();
        let Edit::Cells(first_cells) = first_edit else { panic!("expected an Edit::Cells") };
        assert_eq!(first_cells.len(), 1);

        // Cursor stays put (no Press happened), so typing lands at (1,0), a fresh burst.
        tool.update(ToolEvent::Char('b'), &tctx, &doc);
        let resp2 = tool.update(ToolEvent::Commit, &tctx, &doc);
        let second_edit = commit_edit(resp2).unwrap();
        let Edit::Cells(second_cells) = second_edit else { panic!("expected an Edit::Cells") };
        assert_eq!(second_cells.len(), 1);
        assert_eq!((second_cells[0].x, second_cells[0].y), (1, 0));
        assert_eq!(second_cells[0].after.ch, 'b');
    }

    #[test]
    fn caret_tracks_the_cursor_through_a_session_and_clears_on_cancel() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        assert_eq!(tool.caret(), None, "no caret before the first click");
        tool.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
        assert_eq!(tool.caret(), Some((5, 5)));
        tool.update(ToolEvent::Char('a'), &tctx, &doc);
        assert_eq!(tool.caret(), Some((6, 5)), "caret advances with typing");
        tool.update(ToolEvent::Enter, &tctx, &doc);
        assert_eq!(tool.caret(), Some((5, 6)), "Enter returns to the anchor column");
        tool.update(ToolEvent::Cancel, &tctx, &doc);
        assert_eq!(tool.caret(), None, "cancel clears the caret");
    }

    #[test]
    fn cancel_discards_pending_without_an_edit_and_resets_to_idle() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &doc);
        tool.update(ToolEvent::Char('a'), &tctx, &doc);
        let resp = tool.update(ToolEvent::Cancel, &tctx, &doc);
        assert!(matches!(resp, ToolResponse::Idle));
        assert!(tool.pending().is_empty());

        // Tool is idle: further Char events are ignored until a new Press.
        let resp2 = tool.update(ToolEvent::Char('b'), &tctx, &doc);
        assert!(matches!(resp2, ToolResponse::Idle));
    }

    #[test]
    fn pending_reflects_masked_result_mid_burst() {
        let mut doc = Document::new(20, 20);
        doc.set_cell(0, 4, 4, Cell { ch: 'x', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) });
        let mask = PlaneMask { glyph: true, bg: false };
        let mut tool = TextTool::new();
        let tctx = ctx(mask);
        tool.update(ToolEvent::Press { x: 4, y: 4 }, &tctx, &doc);
        tool.update(ToolEvent::Char('Q'), &tctx, &doc);
        let pending = tool.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].cell.ch, 'Q');
        assert_eq!(pending[0].cell.fg, Rgba(1, 2, 3, 255), "text color follows the glyph plane");
        assert_eq!(pending[0].cell.bg, Rgba(8, 8, 8, 255), "bg masked off: keeps existing");
    }

    /// Targeted unit test for `resync` itself: after an external mutation to a cell the burst has
    /// already touched (standing in for a mid-burst `History::redo`), `resync` must re-pin that
    /// cell's `before` to the mutated value, so `finish` produces a `before` matching `doc`'s
    /// actual current state rather than the stale value pinned at first touch.
    #[test]
    fn resync_repins_before_to_an_externally_mutated_cells_current_value() {
        let mut doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
        tool.update(ToolEvent::Char('a'), &tctx, &doc); // pins before=(5,5)'s current value, Blank

        // Simulate an external mutation (e.g. a mid-burst History::redo) landing on the same cell,
        // bypassing the burst entirely.
        let externally_written = Cell { ch: 'Z', fg: Rgba(9, 9, 9, 255), bg: Rgba(8, 8, 8, 255) };
        doc.set_cell(0, 5, 5, externally_written);
        tool.resync(&doc, 0);

        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        let edit = commit_edit(resp).unwrap();
        let Edit::Cells(cells) = edit else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].before, externally_written, "resync must re-pin before to doc's post-mutation value");
        assert_eq!(cells[0].after.ch, 'a');
    }

    /// A cell the burst has *not* touched must be unaffected by `resync` — only already-pinned
    /// cells are re-read.
    #[test]
    fn resync_does_not_pin_a_before_for_untouched_cells() {
        let mut doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
        tool.update(ToolEvent::Char('a'), &tctx, &doc); // touches only (5,5)

        doc.set_cell(0, 6, 6, Cell { ch: 'Z', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        tool.resync(&doc, 0); // must not start tracking (6,6)

        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        let edit = commit_edit(resp).unwrap();
        let Edit::Cells(cells) = edit else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 1, "resync must not pull in cells the burst never touched");
        assert_eq!((cells[0].x, cells[0].y), (5, 5));
    }

    #[test]
    fn revisiting_the_same_cell_within_one_burst_keeps_original_before_and_latest_after() {
        let doc = Document::new(20, 20);
        let mut tool = TextTool::new();
        let tctx = ctx(PlaneMask::ALL);
        tool.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &doc);
        tool.update(ToolEvent::Char('a'), &tctx, &doc); // writes (5,5)='a', cursor -> (6,5)
        tool.update(ToolEvent::Backspace, &tctx, &doc); // rewrites (5,5)=Blank, cursor -> (5,5)
        tool.update(ToolEvent::Char('b'), &tctx, &doc); // rewrites (5,5)='b' again
        let resp = tool.update(ToolEvent::Commit, &tctx, &doc);
        let edit = commit_edit(resp).unwrap();
        let Edit::Cells(cells) = edit else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 1, "one cell touched three times within a burst is still one CellEdit");
        assert_eq!(cells[0].before, Cell::BLANK, "before must be the pre-burst value, not an intermediate");
        assert_eq!(cells[0].after.ch, 'b');
    }
}
