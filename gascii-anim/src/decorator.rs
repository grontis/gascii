//! The onion-skin/playback canvas-renderer decorator — `AnimPlugin::wrap_renderer`'s return value,
//! folded once into `app.renderer` at startup. Reads the same live `SharedState` the plugin's own
//! `panel`/`tick` hold a clone of (see `shared.rs`), so playback frame/onion config changing every
//! frame is visible here despite `wrap_renderer` only ever running once.
//!
//! Known trade-off: `CanvasRenderer`'s contract only crosses `font_px` (via `CellGrid`), never the
//! host's own named canvas font family (`gascii::fonts::canvas_font_id`, private to the host crate)
//! — this decorator's own glyph painting uses `egui::FontFamily::Monospace` instead, so onion/
//! playback glyphs render in a different (but still monospace) typeface than the host's Iosevka
//! Fixed canvas font. A deliberate trade-off: closing the gap would need a new
//! `PluginHost`/`CellGrid` surface that nothing else requires yet.

use egui::{Align2, Color32, Painter, Pos2, Rect, Vec2};
use gascii_core::{Cell, Document, PendingCell, SelectionView};
use gascii_plugin_api::{CanvasRenderer, CellGrid};

use crate::shared::SharedState;

fn font_id(px: f32) -> egui::FontId {
    egui::FontId::new(px, egui::FontFamily::Monospace)
}

fn color32(c: gascii_core::Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

pub(crate) struct OnionRenderer {
    inner: Box<dyn CanvasRenderer>,
    state: SharedState,
}

impl OnionRenderer {
    pub fn new(inner: Box<dyn CanvasRenderer>, state: SharedState) -> Self {
        Self { inner, state }
    }
}

impl CanvasRenderer for OnionRenderer {
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
    ) {
        let s = self.state.borrow();
        if s.playing {
            // Render-only override: paint the playback frame's committed cells only — never the
            // active/editing frame, and never `Document.active_frame`/the undo cursor, both of
            // which stay completely untouched by playback. pending/hover/caret/selection are
            // deliberately NOT forwarded while playing: an in-progress stroke's overlay must not
            // appear against a frame that isn't the one being edited.
            let frame = s.playback_frame;
            drop(s);
            paint_frame_cells(painter, doc, frame, vp, origin, cell, visible);
            return;
        }
        // The instant `s.playing` goes false — including the moment playback stops naturally, at
        // the end of a non-looping run — everything below falls through to `doc.active_frame()` via
        // `self.inner.paint`. This is a deliberate consequence of playback never touching
        // `Document.active_frame`/the editing cursor, not an unconsidered gap: there is no "last
        // played frame" to linger on once playback stops being what drives the render, only the
        // editing cursor. If the cursor sits on a different frame than the one playback just
        // finished on, the canvas visibly jumps to it here — accepted rather than adding a second,
        // separate "recently stopped" render state with its own expiry rules to track.
        if s.onion_enabled {
            let active = doc.active_frame();
            let (prev, next) = (s.onion_prev, s.onion_next);
            drop(s);
            paint_onion(painter, doc, active, prev, next, vp, origin, cell, visible);
        } else {
            drop(s);
        }
        self.inner.paint(painter, doc, vp, origin, cell, visible, pending, hover, caret, selection);
    }
}

/// Paints frame `frame`'s committed cells only — no pending/hover/caret/selection overlay. Mirrors
/// `NaiveRenderer::paint`'s own cell-drawing loop in shape, reading an explicit frame via
/// `doc.cell_at` instead of the active one via `doc.cell`.
fn paint_frame_cells(painter: &Painter, doc: &Document, frame: usize, vp: &dyn CellGrid, origin: Pos2, cell: Vec2, visible: (u16, u16, u16, u16)) {
    let (x0, y0, x1, y1) = visible;
    let font = font_id(vp.font_px());
    for y in y0..y1 {
        for x in x0..x1 {
            let Some(c) = doc.cell_at(frame, 0, x, y) else { continue };
            paint_cell(painter, c, vp, origin, cell, x, y, &font, None);
        }
    }
}

/// Tinted neighbor content from up to `prev` frames before and `next` frames after `active`,
/// beneath the active frame's own render (which the caller paints separately via `inner.paint`).
/// Out-of-range neighbors (`doc.frame_layers` returning `None`) are silently skipped — clamped at
/// document edges, never an error.
#[allow(clippy::too_many_arguments)]
fn paint_onion(painter: &Painter, doc: &Document, active: usize, prev: u8, next: u8, vp: &dyn CellGrid, origin: Pos2, cell: Vec2, visible: (u16, u16, u16, u16)) {
    let font = font_id(vp.font_px());
    for i in 1..=prev as usize {
        let Some(idx) = active.checked_sub(i) else { break };
        paint_tinted_frame(painter, doc, idx, vp, origin, cell, visible, &font, ONION_PREV_TINT);
    }
    for i in 1..=next as usize {
        let idx = active + i;
        if doc.frame(idx).is_none() {
            break;
        }
        paint_tinted_frame(painter, doc, idx, vp, origin, cell, visible, &font, ONION_NEXT_TINT);
    }
}

const ONION_PREV_TINT: Color32 = Color32::from_rgba_premultiplied(90, 20, 20, 90);
const ONION_NEXT_TINT: Color32 = Color32::from_rgba_premultiplied(20, 70, 20, 90);

#[allow(clippy::too_many_arguments)]
fn paint_tinted_frame(painter: &Painter, doc: &Document, frame: usize, vp: &dyn CellGrid, origin: Pos2, cell: Vec2, visible: (u16, u16, u16, u16), font: &egui::FontId, tint: Color32) {
    let (x0, y0, x1, y1) = visible;
    for y in y0..y1 {
        for x in x0..x1 {
            let Some(c) = doc.cell_at(frame, 0, x, y) else { continue };
            if c.is_blank() {
                continue;
            }
            paint_cell(painter, c, vp, origin, cell, x, y, font, Some(tint));
        }
    }
}

/// Shared single-cell paint used by both the playback and onion paths: bg fill (tinted if `tint` is
/// set) then glyph.
#[allow(clippy::too_many_arguments)]
fn paint_cell(painter: &Painter, c: &Cell, vp: &dyn CellGrid, origin: Pos2, cell: Vec2, x: u16, y: u16, font: &egui::FontId, tint: Option<Color32>) {
    let rect_min = vp.cell_to_screen(x, y, cell, origin);
    let rect = Rect::from_min_size(rect_min, cell);
    if let Some(tint) = tint {
        painter.rect_filled(rect, 0.0, tint);
    } else if c.bg.3 > 0 {
        painter.rect_filled(rect, 0.0, color32(c.bg));
    }
    if c.ch != ' ' {
        let fg = tint.unwrap_or_else(|| color32(c.fg));
        painter.text(rect_min, Align2::LEFT_TOP, c.ch, font.clone(), fg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::Document;

    struct FakeGrid;
    impl CellGrid for FakeGrid {
        fn cell_to_screen(&self, x: u16, y: u16, cell: Vec2, origin: Pos2) -> Pos2 {
            origin + Vec2::new(x as f32 * cell.x, y as f32 * cell.y)
        }
        fn font_px(&self) -> f32 {
            16.0
        }
    }

    /// Records exactly what it was called with, so tests can assert the overlay params were
    /// suppressed while playing — mirrors `gascii-plugin-api`'s own `MarkerRenderer` pattern.
    struct RecordingRenderer {
        calls: std::rc::Rc<std::cell::RefCell<usize>>,
        last_pending_len: std::rc::Rc<std::cell::RefCell<usize>>,
        last_hover_len: std::rc::Rc<std::cell::RefCell<usize>>,
        last_caret_some: std::rc::Rc<std::cell::RefCell<bool>>,
        last_selection_some: std::rc::Rc<std::cell::RefCell<bool>>,
    }
    impl CanvasRenderer for RecordingRenderer {
        fn paint(
            &mut self,
            _painter: &Painter,
            _doc: &Document,
            _vp: &dyn CellGrid,
            _origin: Pos2,
            _cell: Vec2,
            _visible: (u16, u16, u16, u16),
            pending: &[PendingCell],
            hover: &[(u16, u16)],
            caret: Option<(u16, u16, bool)>,
            selection: Option<SelectionView>,
        ) {
            *self.calls.borrow_mut() += 1;
            *self.last_pending_len.borrow_mut() = pending.len();
            *self.last_hover_len.borrow_mut() = hover.len();
            *self.last_caret_some.borrow_mut() = caret.is_some();
            *self.last_selection_some.borrow_mut() = selection.is_some();
        }
    }

    fn run_paint(renderer: &mut OnionRenderer, doc: &Document, pending: &[PendingCell], hover: &[(u16, u16)]) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            renderer.paint(
                &painter,
                doc,
                &FakeGrid,
                Pos2::ZERO,
                Vec2::new(10.0, 16.0),
                (0, 0, doc.width, doc.height),
                pending,
                hover,
                Some((0, 0, true)),
                None,
            );
        });
    }

    #[test]
    fn onion_renderer_suppresses_pending_hover_caret_selection_while_playing_and_forwards_them_while_idle() {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(0));
        let last_pending_len = std::rc::Rc::new(std::cell::RefCell::new(0));
        let last_hover_len = std::rc::Rc::new(std::cell::RefCell::new(0));
        let last_caret_some = std::rc::Rc::new(std::cell::RefCell::new(false));
        let last_selection_some = std::rc::Rc::new(std::cell::RefCell::new(false));
        let inner = RecordingRenderer {
            calls: calls.clone(),
            last_pending_len: last_pending_len.clone(),
            last_hover_len: last_hover_len.clone(),
            last_caret_some: last_caret_some.clone(),
            last_selection_some: last_selection_some.clone(),
        };
        let state = SharedState::new();
        let mut renderer = OnionRenderer::new(Box::new(inner), state.clone());
        let doc = Document::default_document();
        let pending = [PendingCell { x: 0, y: 0, cell: Cell::BLANK }];
        let hover = [(0u16, 0u16)];

        // Idle: forwards straight through to the inner renderer with the real overlay values.
        run_paint(&mut renderer, &doc, &pending, &hover);
        assert_eq!(*calls.borrow(), 1);
        assert_eq!(*last_pending_len.borrow(), 1);
        assert_eq!(*last_hover_len.borrow(), 1);
        assert!(*last_caret_some.borrow());

        // Playing: the inner renderer must not be called at all (the decorator paints the playback
        // frame directly and returns).
        state.borrow_mut().playing = true;
        run_paint(&mut renderer, &doc, &pending, &hover);
        assert_eq!(*calls.borrow(), 1, "inner renderer must not be invoked while playing");
    }

    #[test]
    fn onion_respects_configured_depth_and_document_edges_without_panicking() {
        let state = SharedState::new();
        state.borrow_mut().onion_enabled = true;
        state.borrow_mut().onion_prev = 5;
        state.borrow_mut().onion_next = 5;
        let inner = RecordingRenderer {
            calls: Default::default(),
            last_pending_len: Default::default(),
            last_hover_len: Default::default(),
            last_caret_some: Default::default(),
            last_selection_some: Default::default(),
        };
        let mut renderer = OnionRenderer::new(Box::new(inner), state);
        // Only 1 frame exists: onion_prev/next of 5 must clamp silently, never panic or index oob.
        let doc = Document::default_document();
        run_paint(&mut renderer, &doc, &[], &[]);
    }

    /// The render-side complement of `plugin.rs`'s own `tick`-clamp test: between an external frame
    /// removal (e.g. a delete arriving via `PanelOutcome` mid-playback) and the *next* `tick` call
    /// re-clamping `playback_frame`, a paint can still be requested against a now out-of-range index
    /// — `doc.cell_at` already returns `None` gracefully for it, so this must render nothing for
    /// that frame rather than panicking or indexing out of bounds.
    #[test]
    fn onion_renderer_playback_override_paints_nothing_and_does_not_panic_when_playback_frame_is_stale_and_out_of_range() {
        let inner = RecordingRenderer {
            calls: Default::default(),
            last_pending_len: Default::default(),
            last_hover_len: Default::default(),
            last_caret_some: Default::default(),
            last_selection_some: Default::default(),
        };
        let calls = inner.calls.clone();
        let state = SharedState::new();
        state.borrow_mut().playing = true;
        state.borrow_mut().playback_frame = 99; // stale: e.g. frame_count shrunk to 1 since the last tick
        let mut renderer = OnionRenderer::new(Box::new(inner), state);
        let doc = Document::default_document(); // 1 frame only
        run_paint(&mut renderer, &doc, &[], &[]);
        // No panic (the test would abort if `cell_at`/indexing panicked) and the inner renderer
        // stays skipped, exactly like the in-range playback-override path.
        assert_eq!(*calls.borrow(), 0);
    }

    #[test]
    fn onion_is_suppressed_while_playing_even_if_enabled() {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(0));
        let inner = RecordingRenderer {
            calls: calls.clone(),
            last_pending_len: Default::default(),
            last_hover_len: Default::default(),
            last_caret_some: Default::default(),
            last_selection_some: Default::default(),
        };
        let state = SharedState::new();
        state.borrow_mut().onion_enabled = true;
        state.borrow_mut().playing = true;
        let mut renderer = OnionRenderer::new(Box::new(inner), state);
        let doc = Document::default_document();
        run_paint(&mut renderer, &doc, &[], &[]);
        // While playing, the inner renderer (which onion would otherwise delegate to) is never
        // called — proving onion's tint pass didn't run either, since it shares the same early
        // return as the playback override.
        assert_eq!(*calls.borrow(), 0);
    }
}
