//! Cell-diff undo/redo. `History` is the sole choke point for committed document mutation: it is
//! the only thing that ever writes `Edit::after`/`before` cells back into a `Document`, so the doc
//! and the undo/redo stacks can never drift out of sync.
//!
//! Positional (index-based) frame addressing in `CellEdit`/`Edit::AddFrame`/`RemoveFrame`/
//! `ReorderFrame` is sound only because `History` is a single, strictly-LIFO stack and the sole
//! path any of these variants are ever applied through. Undo always reverses the most recently
//! applied edit first, so an older `CellEdit`'s stored `frame` index is always restored to its
//! original content by any intervening frame-structure edit's own undo before the `CellEdit`
//! itself is undone. Do not add a second mutation path for `frames` (a compacting/compressing
//! history, selective per-frame undo, etc.) without re-deriving this argument — see the pinned
//! test `history_is_a_single_strictly_lifo_stack_across_mixed_edit_kinds` and
//! `undoing_a_reorder_before_an_older_cell_edit_targets_the_correct_frame`.
//!
//! `History::apply` validates a structural frame edit's index against `doc`'s current frame count
//! before touching anything — see `structural_edit_is_valid`. An out-of-range structural edit
//! (reachable from outside this crate through `PanelOutcome.edits`, a public plugin channel) is a
//! silent no-op, never a panic, mirroring `Edit::Cells`' own graceful out-of-range handling via
//! `set_cell_at`.

use crate::model::{Cell, DocExtent, Document, Frame};

/// A single cell's before/after value, addressed by frame + layer + coordinate. `frame` is safe
/// to address positionally (not by a stable id) — see the module doc's LIFO argument.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellEdit {
    pub frame: usize,
    pub layer: usize,
    pub x: u16,
    pub y: u16,
    pub before: Cell,
    pub after: Cell,
}

/// A full-document snapshot: extent plus every frame's contents. Backs `Edit::Resize`'s
/// before/after — deliberately a whole-snapshot swap rather than a diff (resize is a rare,
/// deliberate action, not a per-frame hot path; see `resize_document`'s own docs). Cost scales
/// with frame count now that a document can carry more than one — an extension of the same
/// accepted unbounded-`History`-memory tradeoff already documented below, not a new one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocSnapshot {
    pub extent: DocExtent,
    pub frames: Vec<Frame>,
}

/// A single undoable Document mutation. `#[non_exhaustive]` because further mutation kinds join
/// as new variants without touching existing paths or `History`'s apply/undo/redo mechanics,
/// which are already variant-agnostic.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Edit {
    Cells(Vec<CellEdit>),
    /// A document-extent change (grow or shrink), top-left anchored. `before`/`after` are full
    /// snapshots so undo/redo restore cropped-away cells exactly, not just the extent. Frame count
    /// is invariant under resize, so `active_frame` is not part of this variant.
    Resize { before: DocSnapshot, after: DocSnapshot },
    /// Inserts `frame` at `index`. `active_frame_before`/`active_frame_after` are the cursor's
    /// exact value before/after this insert (computed once by `frame_ops.rs`'s index-shift rules,
    /// never recomputed by `History`) — inserting can shift which frame a numeric `active_frame`
    /// index refers to, exactly like inserting text shifts a cursor position.
    AddFrame { index: usize, frame: Frame, active_frame_before: usize, active_frame_after: usize },
    /// Removes the frame at `index`, keeping its content so undo can reinsert it exactly.
    RemoveFrame { index: usize, frame: Frame, active_frame_before: usize, active_frame_after: usize },
    /// Moves the frame at `from` to `to`. Never changes frame *count*, but can still shift the
    /// index a still-valid `active_frame` should track.
    ReorderFrame { from: usize, to: usize, active_frame_before: usize, active_frame_after: usize },
    /// Sets frame `index`'s duration override (`None` clears it, falling back to the document
    /// default).
    SetFrameDuration { index: usize, before: Option<u32>, after: Option<u32> },
}

fn apply_forward(doc: &mut Document, edit: &Edit) {
    match edit {
        Edit::Cells(cells) => {
            for c in cells {
                doc.set_cell_at(c.frame, c.layer, c.x, c.y, c.after);
            }
        }
        Edit::Resize { after, .. } => {
            doc.width = after.extent.width;
            doc.height = after.extent.height;
            doc.frames = after.frames.clone();
        }
        Edit::AddFrame { index, frame, active_frame_after, .. } => {
            doc.frames.insert(*index, frame.clone());
            doc.active_frame = *active_frame_after;
        }
        Edit::RemoveFrame { index, active_frame_after, .. } => {
            doc.frames.remove(*index);
            doc.active_frame = *active_frame_after;
        }
        Edit::ReorderFrame { from, to, active_frame_after, .. } => {
            let f = doc.frames.remove(*from);
            doc.frames.insert(*to, f);
            doc.active_frame = *active_frame_after;
        }
        Edit::SetFrameDuration { index, after, .. } => {
            doc.frames[*index].duration_override = *after;
        }
    }
}

/// Whether `edit`'s structural frame variants (`AddFrame`/`RemoveFrame`/`ReorderFrame`/
/// `SetFrameDuration`) address a frame that actually exists in `doc` right now. `Edit::Cells`
/// already no-ops gracefully per out-of-range cell via `set_cell_at`, and `Edit::Resize` carries a
/// full snapshot with nothing to index — both are unconditionally valid here. `PanelOutcome.edits`
/// is a public plugin channel (see `gascii-plugin-api`), so a buggy or adversarial plugin can hand
/// `History::apply` an `Edit` built against a document shape that no longer matches — this is the
/// one place every such edit is checked before `frames.insert`/`frames.remove`/`frames[..]` would
/// otherwise panic on it.
fn structural_edit_is_valid(doc: &Document, edit: &Edit) -> bool {
    let len = doc.frames.len();
    match edit {
        Edit::Cells(_) | Edit::Resize { .. } => true,
        // `Vec::insert` accepts `index == len` (append) but panics past it.
        Edit::AddFrame { index, .. } => *index <= len,
        // A document with zero frames must stay unreachable, mirroring `frame_ops::remove_frame`'s
        // own `LastFrame` guard for the same invariant at the pure-fn level.
        Edit::RemoveFrame { index, .. } => *index < len && len > 1,
        Edit::ReorderFrame { from, to, .. } => *from < len && *to < len,
        Edit::SetFrameDuration { index, .. } => *index < len,
    }
}

fn apply_backward(doc: &mut Document, edit: &Edit) {
    match edit {
        Edit::Cells(cells) => {
            for c in cells {
                doc.set_cell_at(c.frame, c.layer, c.x, c.y, c.before);
            }
        }
        Edit::Resize { before, .. } => {
            doc.width = before.extent.width;
            doc.height = before.extent.height;
            doc.frames = before.frames.clone();
        }
        Edit::AddFrame { index, active_frame_before, .. } => {
            doc.frames.remove(*index);
            doc.active_frame = *active_frame_before;
        }
        Edit::RemoveFrame { index, frame, active_frame_before, .. } => {
            doc.frames.insert(*index, frame.clone());
            doc.active_frame = *active_frame_before;
        }
        Edit::ReorderFrame { from, to, active_frame_before, .. } => {
            let f = doc.frames.remove(*to);
            doc.frames.insert(*from, f);
            doc.active_frame = *active_frame_before;
        }
        Edit::SetFrameDuration { index, before, .. } => {
            doc.frames[*index].duration_override = *before;
        }
    }
}

/// Single undo/redo history over a `Document`. App-level state (active tool, color, zoom, plane
/// mask) is never represented here — only committed `Edit`s. Every applied edit is tagged with a
/// monotonically increasing id (`next_id`), used to identify *which* edit currently sits on top of
/// the undo stack — see `top_edit_id`.
///
/// The stacks are deliberately unbounded and `Edit::Cells` is an uncompressed per-cell diff: a
/// full-canvas fill at the 1024×1024 cap is ~1M `CellEdit`s (~tens of MB) held for the session.
/// Acceptable at current extents — depth-capping would silently discard undo steps and
/// compression would complicate the choke point for a problem no real document yet has. Revisit
/// (byte-budget with oldest-entry eviction, or region/RLE encoding) before raising
/// `MAX_WIDTH`/`MAX_HEIGHT` or shipping long-session multi-layer workflows. `Edit::Resize`'s
/// `DocSnapshot` extends the same accepted tradeoff with one more multiplier: its clone cost now
/// scales with frame count too, not just layer count — not a new design problem, the same
/// unbounded-memory acceptance applied to a document that can now have more than one frame.
#[derive(Default)]
pub struct History {
    undo_stack: Vec<(u64, Edit)>,
    redo_stack: Vec<(u64, Edit)>,
    next_id: u64,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes `edit`'s `after` cells into `doc`, pushes it onto the undo stack under a fresh id,
    /// and clears redo.
    ///
    /// A structural frame edit (`AddFrame`/`RemoveFrame`/`ReorderFrame`/`SetFrameDuration`)
    /// addressing a frame `doc` no longer has is a silent no-op — never applied, never pushed,
    /// undo/redo left completely untouched — rather than panicking. Rejecting here, before
    /// `apply_forward` ever runs, is what keeps the undo stack from ever holding a no-op structural
    /// edit (see `structural_edit_is_valid`'s doc comment for why this channel needs the guard).
    pub fn apply(&mut self, doc: &mut Document, edit: Edit) {
        if !structural_edit_is_valid(doc, &edit) {
            return;
        }
        apply_forward(doc, &edit);
        let id = self.next_id;
        self.next_id += 1;
        self.undo_stack.push((id, edit));
        self.redo_stack.clear();
    }

    /// Restores the most recently applied edit's `before` cells. Returns `false` (no-op) if the
    /// undo stack is empty.
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some((id, edit)) = self.undo_stack.pop() else {
            return false;
        };
        apply_backward(doc, &edit);
        self.redo_stack.push((id, edit));
        true
    }

    /// Re-applies the most recently undone edit's `after` cells. Returns `false` (no-op) if the
    /// redo stack is empty.
    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some((id, edit)) = self.redo_stack.pop() else {
            return false;
        };
        apply_forward(doc, &edit);
        self.undo_stack.push((id, edit));
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// The id of whatever edit currently sits on top of the undo stack, or `None` for an empty
    /// stack (its own sentinel — never conflated with "id 0"). This is edit *identity*, not stack
    /// depth or content: undoing back to a point the stack was at before and redoing back restores
    /// the exact same id, while a *new* edit applied at the same depth (e.g. after an undo) always
    /// gets a fresh id via `next_id`. That's what makes it sound as a "has anything changed since
    /// X" marker — see `gascii/src/app.rs`'s `saved_marker`/`is_dirty`, the caller this exists for.
    pub fn top_edit_id(&self) -> Option<u64> {
        self.undo_stack.last().map(|(id, _)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Layer, Rgba};

    fn cell(ch: char) -> Cell {
        Cell {
            ch,
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        }
    }

    /// Watched characteristic, not a behavior: a full-canvas edit stores one uncompressed
    /// `CellEdit` per touched cell — the deliberate unbounded-history tradeoff documented on
    /// `History`. If this assertion ever breaks (an entry stops being ~cell-count-sized, or
    /// `CellEdit` grows), the memory math in that tradeoff needs re-deriving.
    #[test]
    fn a_full_canvas_edit_costs_one_cell_edit_per_cell() {
        let doc = Document::new(64, 64);
        let edit = crate::clear::clear_document(&{
            let mut d = doc.clone();
            for y in 0..64u16 {
                for x in 0..64u16 {
                    d.set_cell(0, x, y, cell('#'));
                }
            }
            d
        })
        .expect("a fully painted canvas must clear to an edit");
        let Edit::Cells(cells) = &edit else { panic!("expected an Edit::Cells") };
        assert_eq!(cells.len(), 64 * 64, "one CellEdit per touched cell, no dedup/compression");
        let entry_bytes = cells.len() * std::mem::size_of::<CellEdit>();
        assert!(
            entry_bytes < 1 << 20,
            "a 64x64 full-canvas entry should stay well under 1 MB; at the 1024x1024 cap the same \
             edit is 256x this — the documented tens-of-MB ceiling ({entry_bytes} bytes here)"
        );
    }

    #[test]
    fn apply_single_cell_edit_mutates_doc_to_after() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 3,
            y: 4,
            before: Cell::BLANK,
            after: cell('x'),
        }]);
        history.apply(&mut doc, edit);
        assert_eq!(doc.cell(0, 3, 4), Some(&cell('x')));
    }

    #[test]
    fn apply_multi_cell_edit_mutates_all_cells() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit = Edit::Cells(vec![
            CellEdit { frame: 0, layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('a') },
            CellEdit { frame: 0, layer: 0, x: 1, y: 0, before: Cell::BLANK, after: cell('b') },
            CellEdit { frame: 0, layer: 0, x: 2, y: 0, before: Cell::BLANK, after: cell('c') },
        ]);
        history.apply(&mut doc, edit);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));
        assert_eq!(doc.cell(0, 1, 0), Some(&cell('b')));
        assert_eq!(doc.cell(0, 2, 0), Some(&cell('c')));
    }

    #[test]
    fn undo_restores_exact_before() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let before = *doc.cell(0, 3, 4).unwrap();
        let edit = Edit::Cells(vec![CellEdit { frame: 0, layer: 0, x: 3, y: 4, before, after: cell('x') }]);
        history.apply(&mut doc, edit);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.cell(0, 3, 4), Some(&before));
    }

    #[test]
    fn redo_reapplies_after() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 3,
            y: 4,
            before: Cell::BLANK,
            after: cell('x'),
        }]);
        history.apply(&mut doc, edit);
        history.undo(&mut doc);
        assert!(history.redo(&mut doc));
        assert_eq!(doc.cell(0, 3, 4), Some(&cell('x')));
    }

    #[test]
    fn new_apply_after_undo_clears_redo() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit1 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit1);
        history.undo(&mut doc);
        assert!(history.can_redo());

        let edit2 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 1,
            y: 0,
            before: Cell::BLANK,
            after: cell('b'),
        }]);
        history.apply(&mut doc, edit2);
        assert!(!history.can_redo());
    }

    #[test]
    fn can_undo_can_redo_transitions() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        assert!(!history.can_undo());
        assert!(!history.can_redo());

        let edit = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit);
        assert!(history.can_undo());
        assert!(!history.can_redo());

        history.undo(&mut doc);
        assert!(!history.can_undo());
        assert!(history.can_redo());

        history.redo(&mut doc);
        assert!(history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn apply_undo_redo_undo_round_trips_multi_cell_edit() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit = Edit::Cells(vec![
            CellEdit { frame: 0, layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('a') },
            CellEdit { frame: 0, layer: 0, x: 1, y: 0, before: Cell::BLANK, after: cell('b') },
        ]);
        history.apply(&mut doc, edit);
        history.undo(&mut doc);
        history.redo(&mut doc);
        history.undo(&mut doc);
        assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 1, 0), Some(&Cell::BLANK));
    }

    #[test]
    fn undo_on_empty_stack_returns_false_and_is_noop() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        assert!(!history.undo(&mut doc));
    }

    /// Documents a flush-before-redo hazard: callers that flush an in-progress edit via `apply()`
    /// right before calling `redo()` will always find the redo stack empty, because `apply()`
    /// unconditionally clears it. Any caller that wants a pending-edit flush *and* a possible redo
    /// to coexist must check `can_redo()` first and skip the flush (leaving the pending edit
    /// uncommitted) when a redo is actually available — this is exactly what
    /// `gascii/src/app.rs`'s `request_redo` does.
    #[test]
    fn apply_after_undo_clears_the_very_redo_stack_a_flush_before_redo_would_need() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit1 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit1);
        history.undo(&mut doc);
        assert!(history.can_redo(), "undo must populate the redo stack");

        // Simulate "flush a pending edit right before redo": a second, unrelated apply() call
        // (standing in for flush_active_tool's own History::apply) fires here.
        let edit2 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 1,
            y: 0,
            before: Cell::BLANK,
            after: cell('b'),
        }]);
        history.apply(&mut doc, edit2);

        // The redo that was available a moment ago is now gone — a caller that unconditionally
        // flushes before redoing would see this exact silent no-op.
        assert!(!history.can_redo());
        assert!(!history.redo(&mut doc));
    }

    /// Documents the mechanism behind a "stale pending tool state survives Open" corruption class
    /// (`gascii/src/app.rs`'s `open_file`): `History::apply`/`undo` never validate that a
    /// `CellEdit`'s `before` matches the target `Document`'s actual current cell value — they
    /// simply write `after` forward and `before` backward, unconditionally. If an `Edit` were ever
    /// constructed with a `before` pinned against a *different*, already-discarded document (e.g.
    /// a `TextTool` burst that survived a document swap), applying it would silently overwrite the
    /// new document's cell with `after`, and a later undo would overwrite it again with the old,
    /// unrelated `before` — neither step notices the mismatch, because that check is deliberately
    /// not `History`'s job (see the module doc). This is exactly why `open_file` resets any
    /// pending `TextTool` state (rather than relying on `History` to catch drift that never gets
    /// caught) when a load succeeds.
    #[test]
    fn apply_and_undo_do_not_validate_before_against_the_documents_actual_current_state() {
        let mut doc = Document::new(5, 5);
        doc.set_cell(0, 0, 0, cell('N')); // the "new" document's real current content at (0,0)

        let mut history = History::new();
        let stale_edit = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 0,
            y: 0,
            before: cell('O'), // an OLD, unrelated document's pre-edit value — not doc's 'N'
            after: cell('X'),
        }]);

        history.apply(&mut doc, stale_edit);
        assert_eq!(
            doc.cell(0, 0, 0),
            Some(&cell('X')),
            "apply blindly writes `after`, never checking the doc's actual prior value"
        );

        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.cell(0, 0, 0),
            Some(&cell('O')),
            "undo blindly restores the stored `before` ('O', the OLD document's value), \
             clobbering 'N' — which was never seen, checked, or recorded anywhere"
        );
    }

    #[test]
    fn redo_on_empty_stack_returns_false_and_is_noop() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        assert!(!history.redo(&mut doc));
    }

    #[test]
    fn edit_referencing_missing_layer_degrades_without_panic_or_mutation() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 5, // doc only has 1 layer
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('x'),
        }]);
        history.apply(&mut doc, edit);
        // No panic; layer 0 untouched.
        assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK));
        assert!(history.can_undo());
        assert!(history.undo(&mut doc));
        assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK));
    }

    #[test]
    fn top_edit_id_is_none_for_a_fresh_history() {
        let history = History::new();
        assert_eq!(history.top_edit_id(), None);
    }

    #[test]
    fn top_edit_id_changes_after_apply() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        assert_eq!(history.top_edit_id(), None);
        let edit = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit);
        assert!(history.top_edit_id().is_some());
    }

    #[test]
    fn top_edit_id_returns_to_the_prior_value_after_undo() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit1 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit1);
        let id_a = history.top_edit_id();

        let edit2 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 1,
            y: 0,
            before: Cell::BLANK,
            after: cell('b'),
        }]);
        history.apply(&mut doc, edit2);
        assert_ne!(history.top_edit_id(), id_a);

        history.undo(&mut doc);
        assert_eq!(history.top_edit_id(), id_a, "undo must restore the exact prior edit's id");
    }

    #[test]
    fn top_edit_id_after_redo_matches_the_original_apply() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit);
        let id_original = history.top_edit_id();

        history.undo(&mut doc);
        history.redo(&mut doc);
        assert_eq!(history.top_edit_id(), id_original, "redo must restore the original apply's id, not a fresh one");
    }

    #[test]
    fn new_apply_after_undo_gives_a_fresh_id_even_at_the_same_stack_depth() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit1 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit1);

        let edit2 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 1,
            y: 0,
            before: Cell::BLANK,
            after: cell('b'),
        }]);
        history.apply(&mut doc, edit2);
        let id_edit2 = history.top_edit_id();

        history.undo(&mut doc);
        let edit3 = Edit::Cells(vec![CellEdit {
            frame: 0,
            layer: 0,
            x: 2,
            y: 0,
            before: Cell::BLANK,
            after: cell('c'),
        }]);
        history.apply(&mut doc, edit3);

        // Same stack depth (one entry) as edit2 was at, but a genuinely different edit — the id
        // must differ, proving stack depth alone is not a sound identifier.
        assert_ne!(
            history.top_edit_id(),
            id_edit2,
            "a new edit applied after undo must get a fresh id, even at the same stack depth"
        );
    }

    #[test]
    fn resize_edit_apply_and_undo_swap_extent_and_layers_wholesale() {
        let mut doc = Document::new(5, 5);
        doc.set_cell(0, 0, 0, cell('a'));
        let before = DocSnapshot {
            extent: doc.extent(),
            frames: vec![Frame { layers: doc.layers().to_vec(), duration_override: None }],
        };

        // Simulate a grow that preserves top-left content and pads the rest.
        let after_cells: Vec<Cell> = {
            let mut cells = vec![Cell::BLANK; 8 * 8];
            cells[0] = cell('a');
            cells
        };
        let after_layer = Layer::from_cells(after_cells, 8, 8);
        let after = DocSnapshot {
            extent: DocExtent { width: 8, height: 8 },
            frames: vec![Frame { layers: vec![after_layer], duration_override: None }],
        };

        let mut history = History::new();
        history.apply(&mut doc, Edit::Resize { before: before.clone(), after: after.clone() });
        assert_eq!(doc.width, 8);
        assert_eq!(doc.height, 8);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));
        assert_eq!(doc.cell(0, 7, 7), Some(&Cell::BLANK));

        assert!(history.undo(&mut doc));
        assert_eq!(doc.width, 5);
        assert_eq!(doc.height, 5);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));

        assert!(history.redo(&mut doc));
        assert_eq!(doc.width, 8);
        assert_eq!(doc.height, 8);
    }

    // --- frame ops: LIFO safety and undo correctness ---

    /// Pins the mechanical property the positional-frame-addressing safety argument (module doc)
    /// depends on: `History` is a single stack, and undo always reverses the *most recently
    /// applied* edit first, regardless of its variant kind. A future change that breaks this
    /// (selective undo, history compression, per-frame history) must fail this test, not silently
    /// corrupt an unrelated `CellEdit`.
    #[test]
    fn history_is_a_single_strictly_lifo_stack_across_mixed_edit_kinds() {
        let mut doc = Document::new(5, 5);
        let mut history = History::new();

        let cells_edit = Edit::Cells(vec![CellEdit { frame: 0, layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('a') }]);
        history.apply(&mut doc, cells_edit);
        let id_cells = history.top_edit_id();

        let duration_edit = Edit::SetFrameDuration { index: 0, before: None, after: Some(50) };
        history.apply(&mut doc, duration_edit);
        let id_duration = history.top_edit_id();

        assert_ne!(id_cells, id_duration, "each applied edit gets its own id regardless of kind");
        assert_eq!(doc.frame(0).unwrap().duration_override, Some(50));

        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.frame(0).unwrap().duration_override,
            None,
            "the first undo must reverse the more recently applied edit (SetFrameDuration), not Cells"
        );
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')), "the Cells edit must still be applied");
        assert_eq!(history.top_edit_id(), id_cells);

        assert!(history.undo(&mut doc));
        assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK), "the second undo reverses the Cells edit");
        assert_eq!(history.top_edit_id(), None);
    }

    /// The worked correctness argument from the module doc, proven concretely: a `CellEdit` on
    /// frame 1, followed by a `ReorderFrame` swapping 0<->1, then undoing both in sequence must
    /// land the cell edit's content back at frame 1 — never frame 0 — because the reorder's own
    /// undo runs first and restores addressing before the older edit's undo executes.
    #[test]
    fn undoing_a_reorder_before_an_older_cell_edit_targets_the_correct_frame() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();

        // Add a second frame, landing after the active one (active_frame stays 0).
        let add_edit = Edit::AddFrame { index: 1, frame: Frame::blank(3, 3), active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, add_edit);
        assert_eq!(doc.frame_count(), 2);

        // Draw a distinguishing glyph on frame 1.
        let cell_edit = Edit::Cells(vec![CellEdit { frame: 1, layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('Q') }]);
        history.apply(&mut doc, cell_edit);
        assert_eq!(doc.cell_at(1, 0, 0, 0), Some(&cell('Q')));

        // Reorder: swap frames 0 and 1. The active frame (0) follows the swap to index 1.
        let reorder_edit = Edit::ReorderFrame { from: 0, to: 1, active_frame_before: 0, active_frame_after: 1 };
        history.apply(&mut doc, reorder_edit);
        assert_eq!(doc.cell_at(0, 0, 0, 0), Some(&cell('Q')), "the reorder moved frame 1's content to index 0");

        // Undo the reorder first: frame 1's content (with 'Q') must land back at index 1.
        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.cell_at(1, 0, 0, 0),
            Some(&cell('Q')),
            "the reorder's own undo must restore addressing before the older CellEdit's undo runs"
        );

        // Undo the CellEdit: it targets frame 1 positionally, which is now correctly restored.
        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.cell_at(1, 0, 0, 0),
            Some(&Cell::BLANK),
            "the CellEdit must undo against frame 1, not frame 0, after both undos complete"
        );
    }

    #[test]
    fn add_frame_undo_restores_active_frame_to_its_pre_insert_value() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let add_edit = Edit::AddFrame { index: 0, frame: Frame::blank(3, 3), active_frame_before: 0, active_frame_after: 1 };
        history.apply(&mut doc, add_edit);
        assert_eq!(doc.frame_count(), 2);
        assert_eq!(doc.active_frame(), 1);

        assert!(history.undo(&mut doc));
        assert_eq!(doc.frame_count(), 1);
        assert_eq!(doc.active_frame(), 0, "undo must restore active_frame to its pre-insert value");
    }

    #[test]
    fn remove_frame_undo_reinserts_the_exact_removed_frame_content() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();

        let mut cells = vec![Cell::BLANK; 9];
        cells[0] = cell('Z');
        let frame1 = Frame { layers: vec![Layer::from_cells(cells, 3, 3)], duration_override: None };

        let add_edit = Edit::AddFrame { index: 1, frame: frame1.clone(), active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, add_edit);
        assert_eq!(doc.cell_at(1, 0, 0, 0), Some(&cell('Z')));

        let remove_edit = Edit::RemoveFrame { index: 1, frame: frame1, active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, remove_edit);
        assert_eq!(doc.frame_count(), 1);

        assert!(history.undo(&mut doc));
        assert_eq!(doc.frame_count(), 2);
        assert_eq!(
            doc.cell_at(1, 0, 0, 0),
            Some(&cell('Z')),
            "undo must reinsert the exact removed frame's content"
        );
    }

    #[test]
    fn set_frame_duration_undo_restores_the_prior_override_including_none() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();

        history.apply(&mut doc, Edit::SetFrameDuration { index: 0, before: None, after: Some(200) });
        assert_eq!(doc.frame(0).unwrap().duration_override, Some(200));

        history.apply(&mut doc, Edit::SetFrameDuration { index: 0, before: Some(200), after: Some(400) });
        assert_eq!(doc.frame(0).unwrap().duration_override, Some(400));

        assert!(history.undo(&mut doc));
        assert_eq!(doc.frame(0).unwrap().duration_override, Some(200));

        assert!(history.undo(&mut doc));
        assert_eq!(
            doc.frame(0).unwrap().duration_override,
            None,
            "undo must restore the prior override exactly, including a None baseline"
        );
    }

    // `structural_edit_is_valid`/`History::apply`'s OOB-no-op contract: a structural frame edit
    // addressing an index the document no longer has must never panic, never mutate, and never
    // land on the undo stack — the direct regression coverage for a buggy/adversarial plugin's
    // `PanelOutcome.edits`.

    #[test]
    fn add_frame_at_len_plus_one_is_a_silent_no_op() {
        let mut doc = Document::new(3, 3); // frame_count() == 1
        let mut history = History::new();
        let edit = Edit::AddFrame { index: 2, frame: Frame::blank(3, 3), active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, edit);
        assert_eq!(doc.frame_count(), 1, "an out-of-range AddFrame index must not touch the document");
        assert_eq!(history.top_edit_id(), None, "a rejected edit must never reach the undo stack");
    }

    #[test]
    fn add_frame_at_exactly_len_still_appends_normally() {
        // The boundary just inside the guard: `index == frame_count()` is a valid append (mirrors
        // `Vec::insert`'s own contract), not an off-by-one rejection.
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = Edit::AddFrame { index: 1, frame: Frame::blank(3, 3), active_frame_before: 0, active_frame_after: 1 };
        history.apply(&mut doc, edit);
        assert_eq!(doc.frame_count(), 2);
        assert!(history.top_edit_id().is_some());
    }

    #[test]
    fn remove_frame_out_of_range_is_a_silent_no_op() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = Edit::RemoveFrame { index: 5, frame: Frame::blank(3, 3), active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, edit);
        assert_eq!(doc.frame_count(), 1);
        assert_eq!(history.top_edit_id(), None);
    }

    /// The last-frame case even when the index itself is in range: a `RemoveFrame` targeting a
    /// document's only remaining frame must also no-op, mirroring `frame_ops::remove_frame`'s own
    /// `LastFrame` guard rather than ever leaving `doc.frames` empty.
    #[test]
    fn remove_frame_of_the_only_remaining_frame_is_a_silent_no_op() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = Edit::RemoveFrame { index: 0, frame: Frame::blank(3, 3), active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, edit);
        assert_eq!(doc.frame_count(), 1, "a document must never be left with zero frames");
        assert_eq!(history.top_edit_id(), None);
    }

    #[test]
    fn reorder_frame_out_of_range_from_or_to_is_a_silent_no_op() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = Edit::AddFrame { index: 1, frame: Frame::blank(3, 3), active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, edit); // 2 frames now
        let marker = history.top_edit_id();

        let bad_from = Edit::ReorderFrame { from: 9, to: 0, active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, bad_from);
        assert_eq!(history.top_edit_id(), marker, "an out-of-range `from` must not push a no-op edit");

        let bad_to = Edit::ReorderFrame { from: 0, to: 9, active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, bad_to);
        assert_eq!(history.top_edit_id(), marker, "an out-of-range `to` must not push a no-op edit");
    }

    #[test]
    fn set_frame_duration_out_of_range_is_a_silent_no_op() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let edit = Edit::SetFrameDuration { index: 7, before: None, after: Some(50) };
        history.apply(&mut doc, edit);
        assert_eq!(history.top_edit_id(), None);
        assert_eq!(doc.frame(0).unwrap().duration_override, None);
    }

    /// A no-op rejection must leave the undo stack exactly as coherent as it was before the
    /// attempt — a later, legitimate undo must still reverse the correct (last real) edit, not
    /// anything the rejected call might have half-applied.
    #[test]
    fn undo_after_a_rejected_structural_edit_still_reverses_the_correct_prior_edit() {
        let mut doc = Document::new(3, 3);
        let mut history = History::new();
        let real_edit = Edit::Cells(vec![CellEdit { frame: 0, layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('a') }]);
        history.apply(&mut doc, real_edit);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')));

        let bad_edit = Edit::RemoveFrame { index: 9, frame: Frame::blank(3, 3), active_frame_before: 0, active_frame_after: 0 };
        history.apply(&mut doc, bad_edit);
        assert_eq!(doc.cell(0, 0, 0), Some(&cell('a')), "the rejected edit must not disturb the document");

        assert!(history.undo(&mut doc));
        assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK), "undo must still reverse the one real edit that was applied");
        assert!(!history.can_undo());
    }
}
