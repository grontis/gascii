//! Cell-diff undo/redo. `History` is the sole choke point for committed document mutation: it is
//! the only thing that ever writes `Edit::after`/`before` cells back into a `Document`, so the doc
//! and the undo/redo stacks can never drift out of sync.

use crate::model::{Cell, Document};

/// A single cell's before/after value, addressed by layer + coordinate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellEdit {
    pub layer: usize,
    pub x: u16,
    pub y: u16,
    pub before: Cell,
    pub after: Cell,
}

/// A single undoable Document mutation. `#[non_exhaustive]` because further mutation kinds (e.g.
/// resize, layer ops) join as new variants without touching the `Cells` path or `History`'s
/// apply/undo/redo mechanics, which are already variant-agnostic.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Edit {
    Cells(Vec<CellEdit>),
}

fn apply_forward(doc: &mut Document, edit: &Edit) {
    match edit {
        Edit::Cells(cells) => {
            for c in cells {
                doc.set_cell(c.layer, c.x, c.y, c.after);
            }
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
    }
}

/// Single undo/redo history over a `Document`. App-level state (active tool, color, zoom, plane
/// mask) is never represented here — only committed `Edit`s.
#[derive(Default)]
pub struct History {
    undo_stack: Vec<Edit>,
    redo_stack: Vec<Edit>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes `edit`'s `after` cells into `doc`, pushes it onto the undo stack, and clears redo.
    pub fn apply(&mut self, doc: &mut Document, edit: Edit) {
        apply_forward(doc, &edit);
        self.undo_stack.push(edit);
        self.redo_stack.clear();
    }

    /// Restores the most recently applied edit's `before` cells. Returns `false` (no-op) if the
    /// undo stack is empty.
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(edit) = self.undo_stack.pop() else {
            return false;
        };
        apply_backward(doc, &edit);
        self.redo_stack.push(edit);
        true
    }

    /// Re-applies the most recently undone edit's `after` cells. Returns `false` (no-op) if the
    /// redo stack is empty.
    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(edit) = self.redo_stack.pop() else {
            return false;
        };
        apply_forward(doc, &edit);
        self.undo_stack.push(edit);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
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
}
