use egui::{Painter, Pos2, Rect, Vec2};
use gascii_core::{CellRect, Document, PendingCell, SelectionView};

/// The two host-viewport facts a `CanvasRenderer` needs — cell-to-screen placement and the
/// current glyph size — without naming the host's own `Viewport` type (an app-owned struct with
/// its own gascii-crate-only font dependency).
pub trait CellGrid {
    fn cell_to_screen(&self, x: u16, y: u16, cell: Vec2, origin: Pos2) -> Pos2;
    fn font_px(&self) -> f32;
}

/// Converts an inclusive cell-space rect to the screen-space rect covering all of its cells.
pub fn cell_rect_to_screen(r: CellRect, vp: &dyn CellGrid, cell: Vec2, origin: Pos2) -> Rect {
    let min = vp.cell_to_screen(r.x0, r.y0, cell, origin);
    let max = vp.cell_to_screen(r.x1 + 1, r.y1 + 1, cell, origin);
    Rect::from_min_max(min, max)
}

/// The canvas paint seam: swappable so a plugin (`Plugin::wrap_renderer`) can layer its own
/// drawing above or below the host's own cell/overlay painting.
pub trait CanvasRenderer {
    /// `hover` is the cells the active tool's next application would land on — the hovered cell,
    /// expanded to the tool's footprint for sized tools; empty when no marker should show.
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        painter: &Painter,
        doc: &Document,
        vp: &dyn CellGrid,
        origin: Pos2,
        cell: Vec2,
        visible: (u16, u16, u16, u16),
        pending: &[PendingCell],
        hover: &[(u16, u16)],
        caret: Option<(u16, u16, bool)>,
        selection: Option<SelectionView>,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeGrid;
    impl CellGrid for FakeGrid {
        fn cell_to_screen(&self, x: u16, y: u16, cell: Vec2, origin: Pos2) -> Pos2 {
            origin + Vec2::new(x as f32 * cell.x, y as f32 * cell.y)
        }
        fn font_px(&self) -> f32 {
            16.0
        }
    }

    /// `cell_rect_to_screen` must be exactly the two-corner `cell_to_screen` calls its doc comment
    /// describes, not a hand-rolled reimplementation that could drift from a `CellGrid` impl's own
    /// placement math.
    #[test]
    fn cell_rect_to_screen_matches_two_corner_calls_to_cell_to_screen() {
        let grid = FakeGrid;
        let cell = Vec2::new(10.0, 20.0);
        let origin = Pos2::new(5.0, 5.0);
        let r = CellRect {
            x0: 2,
            y0: 3,
            x1: 6,
            y1: 8,
        };

        let expect_min = grid.cell_to_screen(r.x0, r.y0, cell, origin);
        let expect_max = grid.cell_to_screen(r.x1 + 1, r.y1 + 1, cell, origin);
        let rect = cell_rect_to_screen(r, &grid, cell, origin);

        assert_eq!(rect.min, expect_min);
        assert_eq!(rect.max, expect_max);
    }
}
