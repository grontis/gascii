//! The playback canvas-renderer decorator — `AnimPlugin::wrap_renderer`'s return value, folded
//! once into `app.renderer` at startup. Reads the same live `SharedState` the plugin's own
//! `panel`/`tick` hold a clone of (see `shared.rs`), so the playback frame changing every frame is
//! visible here despite `wrap_renderer` only ever running once.
//!
//! Known trade-off: `CanvasRenderer`'s contract only crosses `font_px` (via `CellGrid`), never the
//! host's own named canvas font family (`gascii::fonts::canvas_font_id`, private to the host crate)
//! — this decorator's own glyph painting uses `egui::FontFamily::Monospace` instead, so playback
//! glyphs render in a different (but still monospace) typeface than the host's Iosevka Fixed
//! canvas font. A deliberate trade-off: closing the gap would need a new `PluginHost`/`CellGrid`
//! surface that nothing else requires yet.

use egui::{Color32, Painter, Pos2, Rect, Vec2};
use gascii_core::{Document, PendingCell, SelectionView};
use gascii_plugin_api::{CanvasRenderer, CellBatch, CellGrid};

use crate::shared::SharedState;

fn font_id(px: f32) -> egui::FontId {
    egui::FontId::new(px, egui::FontFamily::Monospace)
}

fn color32(c: gascii_core::Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

pub(crate) struct PlaybackRenderer {
    inner: Box<dyn CanvasRenderer>,
    state: SharedState,
}

impl PlaybackRenderer {
    pub fn new(inner: Box<dyn CanvasRenderer>, state: SharedState) -> Self {
        Self { inner, state }
    }
}

/// The viewport facts every cell/frame paint helper below needs, bundled so those helpers stay
/// under clippy's argument-count threshold without dropping any of them — `vp`/`origin`/`cell`/
/// `visible` are exactly what every one of `PlaybackRenderer::paint`'s own calls into this module
/// already had to thread through uniformly, and `font` is computed once (`vp.font_px()`) rather
/// than recomputed per frame or per cell. The trait method itself (`PlaybackRenderer::paint`)
/// still carries its own `#[allow(clippy::too_many_arguments)]`: its signature is
/// `CanvasRenderer::paint`'s, defined in `gascii-plugin-api` and shared by every renderer/
/// decorator in the workspace, not something this module can shrink unilaterally.
struct PaintCtx<'a> {
    vp: &'a dyn CellGrid,
    origin: Pos2,
    cell: Vec2,
    visible: (u16, u16, u16, u16),
    font: egui::FontId,
}

impl<'a> PaintCtx<'a> {
    fn new(vp: &'a dyn CellGrid, origin: Pos2, cell: Vec2, visible: (u16, u16, u16, u16)) -> Self {
        Self {
            vp,
            origin,
            cell,
            visible,
            font: font_id(vp.font_px()),
        }
    }
}

impl CanvasRenderer for PlaybackRenderer {
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
        let ctx = PaintCtx::new(vp, origin, cell, visible);
        let s = self.state.borrow();
        if s.playing {
            // Render-only override: paint the playback frame's committed cells only — never the
            // active/editing frame, and never `Document.active_frame`/the undo cursor, both of
            // which stay completely untouched by playback. pending/hover/caret/selection are
            // deliberately NOT forwarded while playing: an in-progress stroke's overlay must not
            // appear against a frame that isn't the one being edited.
            let frame = s.playback_frame;
            drop(s);
            paint_frame_cells(painter, doc, frame, &ctx);
            return;
        }
        // The instant `s.playing` goes false — including the moment playback stops naturally, at
        // the end of a non-looping run — this falls through to `doc.active_frame()` via
        // `self.inner.paint`. This is a deliberate consequence of playback never touching
        // `Document.active_frame`/the editing cursor, not an unconsidered gap: there is no "last
        // played frame" to linger on once playback stops being what drives the render, only the
        // editing cursor. (Pause parks the cursor on the frozen frame — `Inner::pause_playback` —
        // so the common stop path lands seamlessly anyway.)
        drop(s);
        self.inner.paint(
            painter, doc, vp, origin, cell, visible, pending, hover, caret, selection,
        );
    }
}

/// Paints frame `frame`'s committed cells only — no pending/hover/caret/selection overlay. Mirrors
/// `NaiveRenderer::paint`'s own stacked ("acetate") layer walk, against an explicit frame instead
/// of the active one, so playback shows exactly what editing that frame shows — including the
/// same `CellBatch` submission, flushed per layer to keep the stacking exact.
fn paint_frame_cells(painter: &Painter, doc: &Document, frame: usize, ctx: &PaintCtx) {
    let (x0, y0, x1, y1) = ctx.visible;
    let mut batch = CellBatch::new(ctx.font.clone());
    for layer in gascii_core::visible_layers(doc, frame) {
        for y in y0..y1 {
            for x in x0..x1 {
                let Some(&c) = doc.cell_at(frame, layer, x, y) else {
                    continue;
                };
                let rect_min = ctx.vp.cell_to_screen(x, y, ctx.cell, ctx.origin);
                if c.bg.3 > 0 {
                    batch.bg(Rect::from_min_size(rect_min, ctx.cell), color32(c.bg));
                }
                if c.ch != ' ' {
                    batch.glyph(painter, rect_min, c.ch, color32(c.fg));
                }
            }
        }
        batch.flush_layer(painter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::{Cell, Document};

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

    fn run_paint(
        renderer: &mut PlaybackRenderer,
        doc: &Document,
        pending: &[PendingCell],
        hover: &[(u16, u16)],
    ) {
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
    fn playback_renderer_suppresses_pending_hover_caret_selection_while_playing_and_forwards_them_while_idle(
    ) {
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
        let mut renderer = PlaybackRenderer::new(Box::new(inner), state.clone());
        let doc = Document::default_document();
        let pending = [PendingCell {
            x: 0,
            y: 0,
            cell: Cell::BLANK,
        }];
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
        assert_eq!(
            *calls.borrow(),
            1,
            "inner renderer must not be invoked while playing"
        );
    }

    /// The render-side complement of `plugin.rs`'s own `tick`-clamp test: between an external frame
    /// removal (e.g. a delete arriving via `PanelOutcome` mid-playback) and the *next* `tick` call
    /// re-clamping `playback_frame`, a paint can still be requested against a now out-of-range index
    /// — `doc.cell_at` already returns `None` gracefully for it, so this must render nothing for
    /// that frame rather than panicking or indexing out of bounds.
    #[test]
    fn playback_renderer_override_paints_nothing_and_does_not_panic_when_playback_frame_is_stale_and_out_of_range(
    ) {
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
        let mut renderer = PlaybackRenderer::new(Box::new(inner), state);
        let doc = Document::default_document(); // 1 frame only
        run_paint(&mut renderer, &doc, &[], &[]);
        // No panic (the test would abort if `cell_at`/indexing panicked) and the inner renderer
        // stays skipped, exactly like the in-range playback-override path.
        assert_eq!(*calls.borrow(), 0);
    }

    /// The playback path (`paint_frame_cells`) must composite every visible layer of the played
    /// frame, not just layer 0 — the bug this fix closes (`doc.cell_at(frame, 0, x, y)` silently
    /// dropped content on any layer above 0).
    #[test]
    fn playback_renderer_paints_content_from_a_non_zero_layer() {
        let mut doc = Document::default_document();
        let mut history = gascii_core::History::new();
        let add = gascii_core::add_layer(&doc, doc.layer_count()).unwrap();
        history.apply(&mut doc, add);
        let (cx, cy) = (doc.width / 2, doc.height / 2);
        let top_bg = gascii_core::Rgba(40, 60, 80, 255);
        doc.set_cell(
            1,
            cx,
            cy,
            Cell {
                ch: 'Y',
                fg: gascii_core::Rgba::WHITE,
                bg: top_bg,
            },
        );

        let inner = RecordingRenderer {
            calls: Default::default(),
            last_pending_len: Default::default(),
            last_hover_len: Default::default(),
            last_caret_some: Default::default(),
            last_selection_some: Default::default(),
        };
        let state = SharedState::new();
        state.borrow_mut().playing = true;
        let mut renderer = PlaybackRenderer::new(Box::new(inner), state);

        let ctx = egui::Context::default();
        let seeded_color = color32(top_bg);
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            renderer.paint(
                &painter,
                &doc,
                &FakeGrid,
                Pos2::ZERO,
                Vec2::new(10.0, 16.0),
                (0, 0, doc.width, doc.height),
                &[],
                &[],
                None,
                None,
            );
        });
        let count = out
            .shapes
            .iter()
            .filter(|cs| matches!(&cs.shape, egui::Shape::Rect(r) if r.fill == seeded_color))
            .count();
        assert_eq!(
            count, 1,
            "the playback path must composite layer 1's content, not drop it"
        );
    }

    /// A hidden layer's content must be excluded from the playback path's composited paint, same
    /// contract `composite_cell` gives every other consumer.
    #[test]
    fn playback_renderer_excludes_a_hidden_layers_content() {
        let mut doc = Document::default_document();
        let mut history = gascii_core::History::new();
        let add = gascii_core::add_layer(&doc, doc.layer_count()).unwrap();
        history.apply(&mut doc, add);
        let (cx, cy) = (doc.width / 2, doc.height / 2);
        let hidden_bg = gascii_core::Rgba(40, 60, 80, 255);
        doc.set_cell(
            1,
            cx,
            cy,
            Cell {
                ch: 'Y',
                fg: gascii_core::Rgba::WHITE,
                bg: hidden_bg,
            },
        );
        let hide = gascii_core::set_layer_visibility(&doc, 1, false)
            .unwrap()
            .unwrap();
        history.apply(&mut doc, hide);

        let inner = RecordingRenderer {
            calls: Default::default(),
            last_pending_len: Default::default(),
            last_hover_len: Default::default(),
            last_caret_some: Default::default(),
            last_selection_some: Default::default(),
        };
        let state = SharedState::new();
        state.borrow_mut().playing = true;
        let mut renderer = PlaybackRenderer::new(Box::new(inner), state);

        let ctx = egui::Context::default();
        let seeded_color = color32(hidden_bg);
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            renderer.paint(
                &painter,
                &doc,
                &FakeGrid,
                Pos2::ZERO,
                Vec2::new(10.0, 16.0),
                (0, 0, doc.width, doc.height),
                &[],
                &[],
                None,
                None,
            );
        });
        let count = out
            .shapes
            .iter()
            .filter(|cs| matches!(&cs.shape, egui::Shape::Rect(r) if r.fill == seeded_color))
            .count();
        assert_eq!(
            count, 0,
            "a hidden layer's content must never reach the playback path's composited paint"
        );
    }
}
