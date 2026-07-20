//! Cell-diff undo/redo. `History` is the sole choke point for committed document mutation: it is
//! the only thing that ever writes `Edit::after`/`before` cells back into a `Document`, so the doc
//! and the undo/redo stacks can never drift out of sync.

use crate::model::{Cell, DocExtent, Document, Layer};

/// A single cell's before/after value, addressed by layer + coordinate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellEdit {
    pub layer: usize,
    pub x: u16,
    pub y: u16,
    pub before: Cell,
    pub after: Cell,
}

/// A full-document snapshot: extent plus every layer's contents. Backs `Edit::Resize`'s
/// before/after — deliberately a whole-snapshot swap rather than a diff (resize is a rare,
/// deliberate action, not a per-frame hot path; see `resize_document`'s own docs).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocSnapshot {
    pub extent: DocExtent,
    pub layers: Vec<Layer>,
}

/// A single undoable Document mutation. `#[non_exhaustive]` because further mutation kinds (e.g.
/// layer ops) join as new variants without touching the `Cells`/`Resize` paths or `History`'s
/// apply/undo/redo mechanics, which are already variant-agnostic.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Edit {
    Cells(Vec<CellEdit>),
    /// A document-extent change (grow or shrink), top-left anchored. `before`/`after` are full
    /// snapshots so undo/redo restore cropped-away cells exactly, not just the extent.
    Resize { before: DocSnapshot, after: DocSnapshot },
}

fn apply_forward(doc: &mut Document, edit: &Edit) {
    match edit {
        Edit::Cells(cells) => {
            for c in cells {
                doc.set_cell(c.layer, c.x, c.y, c.after);
            }
        }
        Edit::Resize { after, .. } => {
            doc.width = after.extent.width;
            doc.height = after.extent.height;
            doc.layers = after.layers.clone();
        }
    }
}

fn apply_backward(doc: &mut Document, edit: &Edit) {
    match edit {
        Edit::Cells(cells) => {
            for c in cells {
                doc.set_cell(c.layer, c.x, c.y, c.before);
            }
        }
        Edit::Resize { before, .. } => {
            doc.width = before.extent.width;
            doc.height = before.extent.height;
            doc.layers = before.layers.clone();
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
/// `MAX_WIDTH`/`MAX_HEIGHT` or shipping long-session multi-layer workflows.
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
    pub fn apply(&mut self, doc: &mut Document, edit: Edit) {
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
    use crate::model::Rgba;

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
            CellEdit { layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('a') },
            CellEdit { layer: 0, x: 1, y: 0, before: Cell::BLANK, after: cell('b') },
            CellEdit { layer: 0, x: 2, y: 0, before: Cell::BLANK, after: cell('c') },
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
        let edit = Edit::Cells(vec![CellEdit { layer: 0, x: 3, y: 4, before, after: cell('x') }]);
        history.apply(&mut doc, edit);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.cell(0, 3, 4), Some(&before));
    }

    #[test]
    fn redo_reapplies_after() {
        let mut doc = Document::new(10, 10);
        let mut history = History::new();
        let edit = Edit::Cells(vec![CellEdit {
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
            CellEdit { layer: 0, x: 0, y: 0, before: Cell::BLANK, after: cell('a') },
            CellEdit { layer: 0, x: 1, y: 0, before: Cell::BLANK, after: cell('b') },
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
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit1);
        let id_a = history.top_edit_id();

        let edit2 = Edit::Cells(vec![CellEdit {
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
            layer: 0,
            x: 0,
            y: 0,
            before: Cell::BLANK,
            after: cell('a'),
        }]);
        history.apply(&mut doc, edit1);

        let edit2 = Edit::Cells(vec![CellEdit {
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
        let before = DocSnapshot { extent: doc.extent(), layers: doc.layers.clone() };

        // Simulate a grow that preserves top-left content and pads the rest.
        let after_cells: Vec<Cell> = {
            let mut cells = vec![Cell::BLANK; 8 * 8];
            cells[0] = cell('a');
            cells
        };
        let after_layer = Layer::from_cells(after_cells, 8, 8);
        let after = DocSnapshot { extent: DocExtent { width: 8, height: 8 }, layers: vec![after_layer] };

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
}
