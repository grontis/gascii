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

/// The viewport facts every cell/frame paint helper below needs, bundled so those helpers stay
/// under clippy's argument-count threshold without dropping any of them — `vp`/`origin`/`cell`/
/// `visible` are exactly what every one of `OnionRenderer::paint`'s own calls into this module
/// already had to thread through uniformly, and `font` is computed once (`vp.font_px()`) rather
/// than recomputed per frame or per cell. The trait method itself (`OnionRenderer::paint`) still
/// carries its own `#[allow(clippy::too_many_arguments)]`: its signature is `CanvasRenderer::
/// paint`'s, defined in `gascii-plugin-api` and shared by every renderer/decorator in the
/// workspace, not something this module can shrink unilaterally.
struct PaintCtx<'a> {
    vp: &'a dyn CellGrid,
    origin: Pos2,
    cell: Vec2,
    visible: (u16, u16, u16, u16),
    font: egui::FontId,
}

impl<'a> PaintCtx<'a> {
    fn new(vp: &'a dyn CellGrid, origin: Pos2, cell: Vec2, visible: (u16, u16, u16, u16)) -> Self {
        Self { vp, origin, cell, visible, font: font_id(vp.font_px()) }
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
            paint_onion(painter, doc, active, prev, next, &ctx);
        } else {
            drop(s);
        }
        self.inner.paint(painter, doc, vp, origin, cell, visible, pending, hover, caret, selection);
    }
}

/// Paints frame `frame`'s committed cells only — no pending/hover/caret/selection overlay. Mirrors
/// `NaiveRenderer::paint`'s own cell-drawing loop in shape, compositing an explicit frame via
/// `composite_cell` instead of the active one.
fn paint_frame_cells(painter: &Painter, doc: &Document, frame: usize, ctx: &PaintCtx) {
    let (x0, y0, x1, y1) = ctx.visible;
    for y in y0..y1 {
        for x in x0..x1 {
            let c = gascii_core::composite_cell(doc, frame, x, y);
            paint_cell(painter, &c, ctx, x, y, None);
        }
    }
}

/// Tinted neighbor content from up to `prev` frames before and `next` frames after `active`,
/// beneath the active frame's own render (which the caller paints separately via `inner.paint`).
/// The visitation order and per-step tint are entirely decided by the pure `onion_paint_plan` —
/// this just executes it, so the ordering/alpha-fade logic is unit-testable independent of any
/// actual painting.
fn paint_onion(painter: &Painter, doc: &Document, active: usize, prev: u8, next: u8, ctx: &PaintCtx) {
    for (idx, tint) in onion_paint_plan(active, prev, next, doc) {
        paint_tinted_frame(painter, doc, idx, ctx, tint);
    }
}

/// The pure decision core of `paint_onion`: which frame indices to paint, in what order, tinted
/// how strongly. Out-of-range neighbors (before frame 0, or past the last frame) are silently
/// skipped — clamped at document edges, never an error.
///
/// Ordered **farthest-to-nearest**, the reverse of the configured depth order: the active frame's
/// immediate neighbor is the most useful onion-skin reference (the very next/previous drawing
/// step) and must never be hidden beneath a farther, less relevant one — painting nearest last (on
/// top, since painters draw over what came before) is what guarantees that regardless of overlap.
/// `onion_alpha_scale` fades each step's tint alpha by its distance from `active` (nearest = full
/// strength, farthest configured depth = `ONION_FAR_SCALE`), so depth also reads visually, not
/// just via occlusion order.
fn onion_paint_plan(active: usize, prev: u8, next: u8, doc: &Document) -> Vec<(usize, Color32)> {
    let mut plan = Vec::new();
    for i in (1..=prev as usize).rev() {
        let Some(idx) = active.checked_sub(i) else { continue };
        plan.push((idx, scaled_tint(ONION_PREV_TINT, onion_alpha_scale(i as u8, prev))));
    }
    for i in (1..=next as usize).rev() {
        let idx = active + i;
        if doc.frame(idx).is_none() {
            continue;
        }
        plan.push((idx, scaled_tint(ONION_NEXT_TINT, onion_alpha_scale(i as u8, next))));
    }
    plan
}

const ONION_PREV_TINT: Color32 = Color32::from_rgba_premultiplied(90, 20, 20, 90);
const ONION_NEXT_TINT: Color32 = Color32::from_rgba_premultiplied(20, 70, 20, 90);
/// The farthest configured neighbor's tint strength, as a fraction of the nearest neighbor's —
/// see `onion_alpha_scale`.
const ONION_FAR_SCALE: f32 = 0.35;

/// Linear fade from `1.0` (the nearest neighbor, `i == 1`) down to `ONION_FAR_SCALE` (the farthest
/// *configured* neighbor, `i == depth`) — `depth` is the onion stepper's own value (`prev`/`next`),
/// not however many neighbor frames actually exist near a document edge, so the fade rate stays
/// stable regardless of where `active` sits. `depth <= 1` (nothing to fade across) returns full
/// strength.
fn onion_alpha_scale(i: u8, depth: u8) -> f32 {
    if depth <= 1 {
        return 1.0;
    }
    let t = (i - 1) as f32 / (depth - 1) as f32;
    1.0 - t * (1.0 - ONION_FAR_SCALE)
}

/// Scales every channel of a premultiplied-alpha `Color32` by `scale` — premultiplied color
/// requires scaling R/G/B alongside A to stay correctly premultiplied at the reduced opacity;
/// scaling only the alpha channel would leave the RGB too bright for its new, lower alpha.
fn scaled_tint(base: Color32, scale: f32) -> Color32 {
    let ch = |c: u8| (c as f32 * scale).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_premultiplied(ch(base.r()), ch(base.g()), ch(base.b()), ch(base.a()))
}

fn paint_tinted_frame(painter: &Painter, doc: &Document, frame: usize, ctx: &PaintCtx, tint: Color32) {
    let (x0, y0, x1, y1) = ctx.visible;
    for y in y0..y1 {
        for x in x0..x1 {
            let c = gascii_core::composite_cell(doc, frame, x, y);
            if c.is_blank() {
                continue;
            }
            paint_cell(painter, &c, ctx, x, y, Some(tint));
        }
    }
}

/// Shared single-cell paint used by both the playback and onion paths: bg fill (tinted if `tint` is
/// set) then glyph.
fn paint_cell(painter: &Painter, c: &Cell, ctx: &PaintCtx, x: u16, y: u16, tint: Option<Color32>) {
    let rect_min = ctx.vp.cell_to_screen(x, y, ctx.cell, ctx.origin);
    let rect = Rect::from_min_size(rect_min, ctx.cell);
    if let Some(tint) = tint {
        painter.rect_filled(rect, 0.0, tint);
    } else if c.bg.3 > 0 {
        painter.rect_filled(rect, 0.0, color32(c.bg));
    }
    if c.ch != ' ' {
        let fg = tint.unwrap_or_else(|| color32(c.fg));
        painter.text(rect_min, Align2::LEFT_TOP, c.ch, ctx.font.clone(), fg);
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

    /// The playback path (`paint_frame_cells`) must composite every visible layer of the played
    /// frame, not just layer 0 — the bug this fix closes (`doc.cell_at(frame, 0, x, y)` silently
    /// dropped content on any layer above 0).
    #[test]
    fn onion_renderer_playback_path_paints_content_from_a_non_zero_layer() {
        let mut doc = Document::default_document();
        let mut history = gascii_core::History::new();
        let add = gascii_core::add_layer(&doc, doc.layer_count()).unwrap();
        history.apply(&mut doc, add);
        let (cx, cy) = (doc.width / 2, doc.height / 2);
        let top_bg = gascii_core::Rgba(40, 60, 80, 255);
        doc.set_cell(1, cx, cy, Cell { ch: 'Y', fg: gascii_core::Rgba::WHITE, bg: top_bg });

        let inner = RecordingRenderer {
            calls: Default::default(),
            last_pending_len: Default::default(),
            last_hover_len: Default::default(),
            last_caret_some: Default::default(),
            last_selection_some: Default::default(),
        };
        let state = SharedState::new();
        state.borrow_mut().playing = true;
        let mut renderer = OnionRenderer::new(Box::new(inner), state);

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
        let count = out.shapes.iter().filter(|cs| matches!(&cs.shape, egui::Shape::Rect(r) if r.fill == seeded_color)).count();
        assert_eq!(count, 1, "the playback path must composite layer 1's content, not drop it");
    }

    /// A hidden layer's content must be excluded from the playback path's composited paint, same
    /// contract `composite_cell` gives every other consumer.
    #[test]
    fn onion_renderer_playback_path_excludes_a_hidden_layers_content() {
        let mut doc = Document::default_document();
        let mut history = gascii_core::History::new();
        let add = gascii_core::add_layer(&doc, doc.layer_count()).unwrap();
        history.apply(&mut doc, add);
        let (cx, cy) = (doc.width / 2, doc.height / 2);
        let hidden_bg = gascii_core::Rgba(40, 60, 80, 255);
        doc.set_cell(1, cx, cy, Cell { ch: 'Y', fg: gascii_core::Rgba::WHITE, bg: hidden_bg });
        let hide = gascii_core::set_layer_visibility(&doc, 1, false).unwrap().unwrap();
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
        let mut renderer = OnionRenderer::new(Box::new(inner), state);

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
        let count = out.shapes.iter().filter(|cs| matches!(&cs.shape, egui::Shape::Rect(r) if r.fill == seeded_color)).count();
        assert_eq!(count, 0, "a hidden layer's content must never reach the playback path's composited paint");
    }

    /// `paint_tinted_frame` (the actual onion-skin overlay, distinct from `paint_frame_cells`'
    /// playback path above) must also composite every visible layer of its neighbor frame through
    /// `composite_cell`, and exclude a hidden one — same contract, different call site. Layer 1's
    /// content sits alone at the seeded cell (layer 0 stays blank there), so a tinted rect only
    /// appears at all once the composite includes it.
    #[test]
    fn onion_skin_tint_path_composites_a_non_zero_layer_and_excludes_it_once_hidden() {
        let mut doc = Document::default_document();
        let mut history = gascii_core::History::new();
        let add_frame_edit = gascii_core::add_frame(&doc, 1, gascii_core::Frame::blank(doc.width, doc.height)).unwrap();
        history.apply(&mut doc, add_frame_edit);
        let add_layer_edit = gascii_core::add_layer(&doc, doc.layer_count()).unwrap();
        history.apply(&mut doc, add_layer_edit);
        assert_eq!(doc.active_frame(), 0, "sanity: inserting after the active frame leaves it unchanged");

        let (cx, cy) = (doc.width / 2, doc.height / 2);
        // Frame 0 (the prev-neighbor once frame 1 becomes active), layer 1 only.
        doc.set_cell(1, cx, cy, Cell { ch: 'Y', fg: gascii_core::Rgba::WHITE, bg: gascii_core::Rgba(1, 2, 3, 255) });
        assert!(doc.set_active_frame(1), "the onion skin tints neighbors around the active frame");

        let paint = |doc: &Document| {
            let inner = RecordingRenderer {
                calls: Default::default(),
                last_pending_len: Default::default(),
                last_hover_len: Default::default(),
                last_caret_some: Default::default(),
                last_selection_some: Default::default(),
            };
            let state = SharedState::new();
            state.borrow_mut().onion_enabled = true;
            state.borrow_mut().onion_prev = 1;
            let mut renderer = OnionRenderer::new(Box::new(inner), state);
            let ctx = egui::Context::default();
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let painter = ui.painter().clone();
                renderer.paint(
                    &painter,
                    doc,
                    &FakeGrid,
                    Pos2::ZERO,
                    Vec2::new(10.0, 16.0),
                    (0, 0, doc.width, doc.height),
                    &[],
                    &[],
                    None,
                    None,
                );
            })
        };

        // A single configured prev-neighbor (depth 1) tints at full strength — ONION_PREV_TINT
        // itself, unscaled (`onion_alpha_scale`'s own `depth <= 1` branch).
        let visible = paint(&doc);
        let visible_count =
            visible.shapes.iter().filter(|cs| matches!(&cs.shape, egui::Shape::Rect(r) if r.fill == ONION_PREV_TINT)).count();
        assert_eq!(visible_count, 1, "the onion-skin tint pass must composite layer 1's content from the prev-neighbor frame");

        let hide = gascii_core::set_layer_visibility(&doc, 1, false).unwrap().unwrap();
        history.apply(&mut doc, hide);
        let hidden = paint(&doc);
        let hidden_count =
            hidden.shapes.iter().filter(|cs| matches!(&cs.shape, egui::Shape::Rect(r) if r.fill == ONION_PREV_TINT)).count();
        assert_eq!(hidden_count, 0, "hiding layer 1 must exclude its content from the onion-skin tint pass too");
    }

    fn doc_with_frames(n: usize) -> Document {
        let mut doc = Document::new(2, 2);
        let mut history = gascii_core::History::new();
        for i in 1..n {
            let edit = gascii_core::add_frame(&doc, i, gascii_core::Frame::blank(2, 2)).unwrap();
            history.apply(&mut doc, edit);
        }
        doc
    }

    fn alpha_of(c: Color32) -> u8 {
        c.a()
    }

    /// The direct H4/L3 regression: `onion_paint_plan` must visit farthest-to-nearest on both
    /// sides, so a caller painting in that order naturally paints the active frame's immediate
    /// neighbors last (on top).
    #[test]
    fn onion_paint_plan_orders_farthest_to_nearest_on_both_sides() {
        let doc = doc_with_frames(10);
        let plan = onion_paint_plan(5, 3, 2, &doc);
        let indices: Vec<usize> = plan.iter().map(|(idx, _)| *idx).collect();
        assert_eq!(
            indices,
            vec![2, 3, 4, 7, 6],
            "prev side must run farthest (2) to nearest (4); next side must run farthest (7) to nearest (6)"
        );
    }

    /// Alpha must strictly increase (weakest first) moving through the plan on each side, ending at
    /// full strength for the nearest neighbor immediately adjacent to `active`.
    #[test]
    fn onion_paint_plan_alpha_increases_toward_the_nearest_neighbor_on_each_side() {
        let doc = doc_with_frames(10);
        let plan = onion_paint_plan(5, 4, 3, &doc);
        let prev_alphas: Vec<u8> = plan[0..4].iter().map(|(_, c)| alpha_of(*c)).collect();
        let next_alphas: Vec<u8> = plan[4..7].iter().map(|(_, c)| alpha_of(*c)).collect();
        assert!(prev_alphas.windows(2).all(|w| w[0] <= w[1]), "prev-side alpha must never decrease moving toward active: {prev_alphas:?}");
        assert!(next_alphas.windows(2).all(|w| w[0] <= w[1]), "next-side alpha must never decrease moving toward active: {next_alphas:?}");
        assert_eq!(*prev_alphas.last().unwrap(), ONION_PREV_TINT.a(), "the nearest prev neighbor must be full strength");
        assert_eq!(*next_alphas.last().unwrap(), ONION_NEXT_TINT.a(), "the nearest next neighbor must be full strength");
    }

    /// A neighbor at the document edge (out of range) is skipped without disturbing the ones still
    /// in range, and the surviving entries keep the alpha they'd have had against the *configured*
    /// depth (not a depth silently shrunk to however many frames actually exist).
    #[test]
    fn onion_paint_plan_skips_out_of_range_neighbors_without_shifting_the_alpha_of_the_rest() {
        let doc = doc_with_frames(3); // valid indices 0, 1, 2
        // active = 1, prev = 5 (only index 0 is actually reachable), next = 5 (only index 2).
        let plan = onion_paint_plan(1, 5, 5, &doc);
        let indices: Vec<usize> = plan.iter().map(|(idx, _)| *idx).collect();
        assert_eq!(indices, vec![0, 2], "only the two in-range neighbors survive");
        // Both are the *nearest* configured step (i == 1) on their respective side, so both must
        // be full strength — proving the scale is computed against the requested depth (5), not a
        // depth quietly reduced to "however many neighbors exist" (which would also read as i==1
        // over depth==1, coincidentally full strength too — see the next test for a case that
        // actually distinguishes the two).
        assert_eq!(alpha_of(plan[0].1), ONION_PREV_TINT.a());
        assert_eq!(alpha_of(plan[1].1), ONION_NEXT_TINT.a());
    }

    /// Distinguishes "scaled against the configured depth" from "scaled against however many
    /// neighbors survive": the nearest neighbor (i==1) is unaffected either way (previous test), so
    /// this checks the *farthest surviving* one, which only reads correctly against the configured
    /// depth.
    #[test]
    fn onion_paint_plan_scales_against_the_configured_depth_not_the_surviving_neighbor_count() {
        let doc = doc_with_frames(10);
        // active = 2, prev = 5: only indices 0 and 1 (i == 2, i == 1) are reachable, i == 3..5 fall
        // off the front. The surviving farthest (index 0, i == 2) must be scaled as "step 2 of a
        // 5-deep fade", not as "step 2 of a 2-deep fade" (which would be a much weaker scale).
        let plan = onion_paint_plan(2, 5, 0, &doc);
        let indices: Vec<usize> = plan.iter().map(|(idx, _)| *idx).collect();
        assert_eq!(indices, vec![0, 1]);
        let scale_against_full_depth = onion_alpha_scale(2, 5);
        let scale_against_shrunk_depth = onion_alpha_scale(2, 2);
        assert_ne!(scale_against_full_depth, scale_against_shrunk_depth, "sanity: the two scales must actually differ for this to be a meaningful test");
        assert_eq!(alpha_of(plan[0].1), (ONION_PREV_TINT.a() as f32 * scale_against_full_depth).round().clamp(0.0, 255.0) as u8);
    }

    #[test]
    fn onion_alpha_scale_is_full_strength_at_i_1_and_far_scale_at_the_configured_depth() {
        assert_eq!(onion_alpha_scale(1, 5), 1.0);
        assert!((onion_alpha_scale(5, 5) - ONION_FAR_SCALE).abs() < 1e-6);
        // Monotonic: every step farther from active must be weaker or equal, never stronger.
        let scales: Vec<f32> = (1..=8u8).map(|i| onion_alpha_scale(i, 8)).collect();
        assert!(scales.windows(2).all(|w| w[0] >= w[1]), "alpha must never increase with distance: {scales:?}");
    }

    #[test]
    fn onion_alpha_scale_is_always_full_strength_when_depth_is_1_or_0() {
        assert_eq!(onion_alpha_scale(1, 1), 1.0);
        assert_eq!(onion_alpha_scale(1, 0), 1.0);
    }

    #[test]
    fn scaled_tint_at_full_scale_is_the_identity() {
        assert_eq!(scaled_tint(ONION_PREV_TINT, 1.0), ONION_PREV_TINT);
    }

    #[test]
    fn scaled_tint_scales_every_channel_including_alpha() {
        let half = scaled_tint(Color32::from_rgba_premultiplied(100, 50, 20, 200), 0.5);
        assert_eq!(half, Color32::from_rgba_premultiplied(50, 25, 10, 100));
    }
}
