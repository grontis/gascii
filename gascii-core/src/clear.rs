//! Whole-document Clear: blanks every cell of every layer, skipping cells already blank —
//! mirrors `resize_document`'s pure, `Edit`-producing shape so it goes through the same
//! `History`/undo path as every other document mutation.

use crate::{Cell, CellEdit, Document, Edit};

/// Builds the `Edit::Cells` that blanks `doc` in place. `None` if the document is already
/// entirely blank — no phantom undo step for a no-op Clear, matching every other tool's
/// "nothing to commit" contract.
pub fn clear_document(doc: &Document) -> Option<Edit> {
    let mut cell_edits = Vec::new();
    for (layer_ix, _layer) in doc.layers.iter().enumerate() {
        for y in 0..doc.height {
            for x in 0..doc.width {
                let before = doc.cell(layer_ix, x, y).copied().unwrap_or(Cell::BLANK);
                if before == Cell::BLANK {
                    continue;
                }
                cell_edits.push(CellEdit { layer: layer_ix, x, y, before, after: Cell::BLANK });
            }
        }
    }
    (!cell_edits.is_empty()).then_some(Edit::Cells(cell_edits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::History;
    use crate::model::{Layer, Rgba};

    fn cell(ch: char) -> Cell {
        Cell { ch, fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }
    }

    #[test]
    fn clear_document_skips_already_blank_cells() {
        let mut doc = Document::new(3, 3);
        doc.set_cell(0, 1, 1, cell('a'));
        let edit = clear_document(&doc).unwrap();
        let Edit::Cells(cells) = &edit else { panic!("expected Edit::Cells") };
        // Only the one non-blank cell should appear — every already-blank cell is skipped.
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].x, 1);
        assert_eq!(cells[0].y, 1);
        assert_eq!(cells[0].before, cell('a'));
        assert_eq!(cells[0].after, Cell::BLANK);
    }

    #[test]
    fn clear_document_blanks_every_nonblank_cell_across_all_layers() {
        let mut doc = Document::new(2, 2);
        doc.layers.push(Layer::blank(2, 2));
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(1, 1, 1, cell('b'));
        let edit = clear_document(&doc).unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        for layer in 0..2 {
            for y in 0..2u16 {
                for x in 0..2u16 {
                    assert_eq!(doc.cell(layer, x, y), Some(&Cell::BLANK));
                }
            }
        }
    }

    #[test]
    fn clear_document_on_an_already_blank_document_returns_none() {
        let doc = Document::new(5, 5);
        assert_eq!(clear_document(&doc), None);
    }

    #[test]
    fn clear_document_edit_undoes_cleanly_through_history() {
        let mut doc = Document::new(3, 3);
        doc.set_cell(0, 0, 0, cell('x'));
        doc.set_cell(0, 2, 2, cell('y'));
        let before = doc.clone();
        let edit = clear_document(&doc).unwrap();
        let mut history = History::new();
        history.apply(&mut doc, edit);
        assert_ne!(doc, before);
        assert!(doc.layers[0].cells().iter().all(Cell::is_blank));
        assert!(history.undo(&mut doc));
        assert_eq!(doc, before);
    }
}
