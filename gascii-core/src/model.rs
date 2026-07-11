use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);
impl Rgba {
    pub const WHITE: Rgba = Rgba(255, 255, 255, 255);
    pub const TRANSPARENT: Rgba = Rgba(0, 0, 0, 0);
    pub const fn is_transparent(&self) -> bool {
        self.3 == 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgba,
    pub bg: Rgba,
}
impl Cell {
    /// Canonical empty cell: space glyph + fully transparent bg (ADR-0007). The ONLY empty state.
    pub const BLANK: Cell = Cell {
        ch: ' ',
        fg: Rgba::WHITE,
        bg: Rgba::TRANSPARENT,
    };
    pub fn is_blank(&self) -> bool {
        self.ch == ' ' && self.bg.3 == 0
    }
}
impl Default for Cell {
    fn default() -> Self {
        Cell::BLANK
    }
}

/// One full-canvas sheet of Cells, row-major, length == width*height. `cells` stays private so
/// all indexing goes through Document (which owns the dimensions).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Layer {
    cells: Vec<Cell>,
}
impl Layer {
    pub fn blank(width: u16, height: u16) -> Self {
        Layer {
            cells: vec![Cell::BLANK; width as usize * height as usize],
        }
    }
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DocSettings {
    pub strict_ascii: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DocExtent {
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Document {
    pub width: u16,
    pub height: u16,
    pub layers: Vec<Layer>, // v1: exactly one, but never collapse to a single Layer (ADR-0006)
    pub settings: DocSettings,
}
impl Document {
    pub const DEFAULT_WIDTH: u16 = 80;
    pub const DEFAULT_HEIGHT: u16 = 25;

    pub fn new(width: u16, height: u16) -> Self {
        assert!(width > 0 && height > 0, "canvas must be non-empty");
        Document {
            width,
            height,
            layers: vec![Layer::blank(width, height)],
            settings: DocSettings::default(),
        }
    }
    /// Default new document: 80×25 (FR-1).
    pub fn default_document() -> Self {
        Self::new(Self::DEFAULT_WIDTH, Self::DEFAULT_HEIGHT)
    }

    pub fn extent(&self) -> DocExtent {
        DocExtent {
            width: self.width,
            height: self.height,
        }
    }
    pub fn in_bounds(&self, x: u16, y: u16) -> bool {
        x < self.width && y < self.height
    }
    #[inline]
    fn index(&self, x: u16, y: u16) -> usize {
        y as usize * self.width as usize + x as usize
    }

    pub fn cell(&self, layer: usize, x: u16, y: u16) -> Option<&Cell> {
        if !self.in_bounds(x, y) {
            return None;
        }
        let i = self.index(x, y);
        self.layers.get(layer).and_then(|l| l.cells.get(i))
    }
    /// Returns false (no-op) if out of bounds or layer missing.
    pub fn set_cell(&mut self, layer: usize, x: u16, y: u16, value: Cell) -> bool {
        if !self.in_bounds(x, y) {
            return false;
        }
        let i = self.index(x, y);
        match self.layers.get_mut(layer).and_then(|l| l.cells.get_mut(i)) {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_cell_is_blank() {
        assert!(Cell::BLANK.is_blank());
    }

    #[test]
    fn opaque_bg_space_is_not_blank() {
        let cell = Cell {
            ch: ' ',
            fg: Rgba::WHITE,
            bg: Rgba::WHITE,
        };
        assert!(!cell.is_blank());
    }

    #[test]
    fn non_space_transparent_bg_is_not_blank() {
        let cell = Cell {
            ch: 'x',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        };
        assert!(!cell.is_blank());
    }

    #[test]
    fn rgba_transparency() {
        assert!(Rgba::TRANSPARENT.is_transparent());
        assert!(!Rgba::WHITE.is_transparent());
    }

    #[test]
    fn default_document_is_80x25_blank() {
        let doc = Document::default_document();
        assert_eq!(doc.width, 80);
        assert_eq!(doc.height, 25);
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].cells().len(), 2000);
        assert!(doc.layers[0].cells().iter().all(Cell::is_blank));
    }

    #[test]
    fn in_bounds_edges() {
        let doc = Document::default_document();
        assert!(doc.in_bounds(79, 24));
        assert!(!doc.in_bounds(80, 24));
        assert!(!doc.in_bounds(79, 25));
    }

    #[test]
    fn set_cell_and_cell_round_trip() {
        let mut doc = Document::new(10, 5);
        let value = Cell {
            ch: 'x',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        };
        assert!(doc.set_cell(0, 3, 2, value));
        assert_eq!(doc.cell(0, 3, 2), Some(&value));
        assert_eq!(doc.layers[0].cells()[2 * 10 + 3], value);

        // neighbours untouched
        assert_eq!(doc.cell(0, 2, 2), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 4, 2), Some(&Cell::BLANK));
    }

    #[test]
    fn set_cell_out_of_bounds_is_noop() {
        let mut doc = Document::new(10, 5);
        let value = Cell {
            ch: 'x',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        };
        assert!(!doc.set_cell(0, 10, 0, value));
        assert!(!doc.set_cell(0, 0, 5, value));
        assert!(doc.layers[0].cells().iter().all(Cell::is_blank));
        assert_eq!(doc.cell(0, 10, 0), None);
    }

    #[test]
    fn serde_round_trip() {
        let doc = Document::default_document();
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    #[should_panic(expected = "canvas must be non-empty")]
    fn new_with_zero_width_panics() {
        Document::new(0, 10);
    }

    #[test]
    #[should_panic(expected = "canvas must be non-empty")]
    fn new_with_zero_height_panics() {
        Document::new(10, 0);
    }

    #[test]
    fn far_corner_set_cell_and_cell_at_1024x1024() {
        // Locks in the u16->usize widen-before-multiply index math: at ~1M cells the index
        // would overflow a multiply done in u16 space before widening.
        let mut doc = Document::new(1024, 1024);
        let value = Cell {
            ch: 'x',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        };
        assert!(doc.set_cell(0, 1023, 1023, value));
        assert_eq!(doc.cell(0, 1023, 1023), Some(&value));
        assert_eq!(doc.layers[0].cells()[1024 * 1024 - 1], value);

        // one-before-far-corner untouched
        assert_eq!(doc.cell(0, 1022, 1023), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 1023, 1022), Some(&Cell::BLANK));
    }
}
