use eframe::egui::{self, Align2, Color32, Painter, Pos2, Rect, Vec2};
use gascii_core::{Document, Rgba};

use crate::app::GasciiApp;
use crate::fonts::canvas_font_id;
use crate::viewport::Viewport;

fn color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

/// Background the doc canvas paints onto before compositing transparent Blank cells.
const DOC_BG: Color32 = Color32::from_rgb(10, 10, 10);

pub trait CanvasRenderer {
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        painter: &Painter,
        doc: &Document,
        vp: &Viewport,
        origin: Pos2,
        cell: Vec2,
        visible: (u16, u16, u16, u16),
        cursor_on: Option<(u16, u16)>,
    );
}

/// Default renderer: per-cell `Painter::text`/`rect_filled`, no caching.
pub struct NaiveRenderer;

impl CanvasRenderer for NaiveRenderer {
    fn paint(
        &mut self,
        painter: &Painter,
        doc: &Document,
        vp: &Viewport,
        origin: Pos2,
        cell: Vec2,
        visible: (u16, u16, u16, u16),
        cursor_on: Option<(u16, u16)>,
    ) {
        let (x0, y0, x1, y1) = visible;

        let doc_rect = Rect::from_min_size(
            origin + vp.pan,
            Vec2::new(doc.width as f32 * cell.x, doc.height as f32 * cell.y),
        );
        painter.rect_filled(doc_rect, 0.0, DOC_BG);

        let font_id = canvas_font_id(vp.font_px());
        for y in y0..y1 {
            for x in x0..x1 {
                let Some(c) = doc.cell(0, x, y) else {
                    continue;
                };
                let rect_min = vp.cell_to_screen(x, y, cell, origin);
                if c.bg.3 > 0 {
                    let rect = Rect::from_min_size(rect_min, cell);
                    painter.rect_filled(rect, 0.0, color32(c.bg));
                }
                if c.ch != ' ' {
                    painter.text(
                        rect_min,
                        Align2::LEFT_TOP,
                        c.ch,
                        font_id.clone(),
                        color32(c.fg),
                    );
                }
            }
        }

        if let Some((cx, cy)) = cursor_on {
            let rect_min = vp.cell_to_screen(cx, cy, cell, origin);
            let rect = Rect::from_min_size(rect_min, cell);
            painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 120));
        }
    }
}

/// Cursor blink: on for half of each 1s period. Caller drives repaint scheduling.
pub fn cursor_blink_on(ui: &egui::Ui) -> bool {
    let t = ui.input(|i| i.time);
    (t * 2.0) as i64 % 2 == 0
}

pub fn show(ui: &mut egui::Ui, app: &mut GasciiApp) {
    let ctx = ui.ctx().clone();
    if app.pending_fit {
        app.viewport
            .fit_to_window(ui.available_size(), app.doc.extent(), &ctx);
        app.pending_fit = false;
    }

    let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
    let origin = response.rect.min;
    let cell = app.viewport.cell_size(&ctx);

    let (scroll_y, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
    if ctrl && scroll_y != 0.0 {
        if let Some(cursor) = response.hover_pos() {
            app.viewport
                .zoom_at(cursor, scroll_y.signum() as i32, cell, origin);
        }
    }

    if response.dragged_by(egui::PointerButton::Middle) {
        app.viewport.pan += response.drag_delta();
    }
    let space = ui.input(|i| i.key_down(egui::Key::Space));
    if space && response.dragged_by(egui::PointerButton::Primary) {
        app.viewport.pan += response.drag_delta();
    }

    let cell = app.viewport.cell_size(&ctx);
    app.hovered_cell = response
        .hover_pos()
        .and_then(|p| app.viewport.screen_to_cell(p, cell, origin, app.doc.extent()));

    let visible = app
        .viewport
        .visible_cell_rect(painter.clip_rect(), cell, origin, app.doc.extent());

    let on = if app.spike.active {
        ctx.request_repaint();
        true
    } else {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        cursor_blink_on(ui)
    };
    let cursor_cell = Some(app.cursor).filter(|_| on);

    let t0 = std::time::Instant::now();
    app.renderer
        .paint(&painter, &app.doc, &app.viewport, origin, cell, visible, cursor_cell);
    app.spike.record(t0.elapsed());
}
