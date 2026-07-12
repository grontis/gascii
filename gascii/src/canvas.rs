use eframe::egui::{self, Align2, Color32, Painter, Pos2, Rect, Vec2};
use gascii_core::{Direction, Document, PendingCell, Rgba, ToolEvent, ToolResponse};

use crate::app::{GasciiApp, ToolKind};
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
        pending: &[PendingCell],
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
        pending: &[PendingCell],
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

        for p in pending {
            if p.x < x0 || p.x >= x1 || p.y < y0 || p.y >= y1 {
                continue;
            }
            let rect_min = vp.cell_to_screen(p.x, p.y, cell, origin);
            if p.cell.bg.3 > 0 {
                let rect = Rect::from_min_size(rect_min, cell);
                painter.rect_filled(rect, 0.0, color32(p.cell.bg));
            }
            if p.cell.ch != ' ' {
                painter.text(
                    rect_min,
                    Align2::LEFT_TOP,
                    p.cell.ch,
                    font_id.clone(),
                    color32(p.cell.fg),
                );
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

pub(crate) fn tool_ctx(app: &GasciiApp) -> gascii_core::ToolCtx {
    gascii_core::ToolCtx {
        layer: 0,
        glyph: app.active_glyph,
        fg: app.active_fg,
        bg: app.active_bg,
        mask: app.mask,
    }
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

    // Precedence 1: zoom. Allowed any time, including mid-stroke — pending cells are
    // cell-addressed and stay valid; the cursor-anchored zoom keeps the pointer's cell fixed.
    let (scroll_y, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
    if ctrl && scroll_y != 0.0 {
        if let Some(cursor) = response.hover_pos() {
            app.viewport
                .zoom_at(cursor, scroll_y.signum() as i32, cell, origin);
        }
    }

    // Precedence 2: pan. Middle-drag is always available (never conflicts with a primary
    // stroke). Space+primary-drag pans only while the space-pan gesture owns the primary button
    // (decided at press time below), so it never steals an in-progress stroke.
    if response.dragged_by(egui::PointerButton::Middle) {
        app.viewport.pan += response.drag_delta();
    }
    let space = ui.input(|i| i.key_down(egui::Key::Space));

    let cell = app.viewport.cell_size(&ctx);
    let doc_extent = app.doc.extent();
    app.hovered_cell = response
        .hover_pos()
        .and_then(|p| app.viewport.screen_to_cell(p, cell, origin, doc_extent));

    // Precedence 3: stroke vs space-pan, resolved from raw pointer edges (not
    // clicked()/dragged()) so a single click that doesn't move still yields a one-cell stroke.
    // Gesture ownership is decided once at press time and holds until release, so a mid-gesture
    // Space toggle can't steal an in-progress stroke and a mid-gesture tool switch can't corrupt
    // an in-progress pan.
    //
    // Known gap: release is detected from pointer state, so an OS-level focus loss mid-drag with
    // no synthetic mouse-up (e.g. alt-tab while dragging) can leave
    // `stroke_active`/`space_pan_active` stuck until the next primary press.
    let (primary_pressed, primary_down, primary_released) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released()));
    let gesture_ends = primary_released || !primary_down;

    // Tracks whether this frame's Press branch just started a stroke, so the Drag branch below
    // (which re-checks `app.stroke_active`, now true) doesn't also send a same-frame, same-cell
    // Drag for the press that just happened — one pointer event in, one Tool event out per frame.
    let mut stroke_just_started = false;

    if !app.stroke_active && !app.space_pan_active && primary_pressed {
        if space {
            app.space_pan_active = true;
        } else if app.tool_kind == ToolKind::Eyedropper {
            // One-shot pick, not a multi-frame gesture: no ownership to track.
            if response.contains_pointer() {
                if let Some(pos) = response.interact_pointer_pos() {
                    if let Some((x, y)) = app.viewport.screen_to_cell(pos, cell, origin, doc_extent) {
                        if let Some(picked) = app.doc.cell(0, x, y).copied() {
                            let (fg, bg) = gascii_core::eyedrop(&picked);
                            app.active_fg = fg;
                            app.active_bg = bg;
                        }
                    }
                }
            }
        } else if app.tool_kind == ToolKind::Text {
            // No pointer-drag/release lifecycle for text entry: a single Press call is the whole
            // pointer-side interaction, so stroke_active is never set here.
            if response.contains_pointer() {
                if let Some(pos) = response.interact_pointer_pos() {
                    if let Some((x, y)) = app.viewport.screen_to_cell(pos, cell, origin, doc_extent) {
                        let tctx = tool_ctx(app);
                        let resp = app.tool.update(ToolEvent::Press { x, y }, &tctx, &app.doc);
                        if let ToolResponse::Commit(Some(edit)) = resp {
                            app.history.apply(&mut app.doc, edit);
                        }
                        app.text_editing = true;
                    }
                }
            }
        } else if response.contains_pointer() {
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some((x, y)) = app.viewport.screen_to_cell(pos, cell, origin, doc_extent) {
                    app.stroke_active = true;
                    stroke_just_started = true;
                    let tctx = tool_ctx(app);
                    app.tool.update(ToolEvent::Press { x, y }, &tctx, &app.doc);
                }
            }
        }
    }

    if app.space_pan_active {
        if primary_down {
            app.viewport.pan += response.drag_delta();
        }
        if gesture_ends {
            app.space_pan_active = false;
        }
    } else if app.stroke_active {
        if primary_down && !stroke_just_started {
            if let Some(pos) = response.interact_pointer_pos() {
                let (x, y) = app.viewport.screen_to_cell_clamped(pos, cell, origin, doc_extent);
                let tctx = tool_ctx(app);
                app.tool.update(ToolEvent::Drag { x, y }, &tctx, &app.doc);
            }
        }
        if gesture_ends {
            let tctx = tool_ctx(app);
            let resp = app.tool.update(ToolEvent::Release, &tctx, &app.doc);
            if let ToolResponse::Commit(Some(edit)) = resp {
                app.history.apply(&mut app.doc, edit);
            }
            app.stroke_active = false;
        }
    }

    // Text-mode keyboard routing: translates raw input events to ToolEvents, fed to app.tool the
    // same way pointer events already are (same Commit -> history.apply handling).
    if app.tool_kind == ToolKind::Text && app.text_editing {
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::Text(s) => {
                    for ch in s.chars() {
                        let tctx = tool_ctx(app);
                        let resp = app.tool.update(ToolEvent::Char(ch), &tctx, &app.doc);
                        if let ToolResponse::Commit(Some(edit)) = resp {
                            app.history.apply(&mut app.doc, edit);
                        }
                    }
                }
                egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                    let tctx = tool_ctx(app);
                    app.tool.update(ToolEvent::Enter, &tctx, &app.doc);
                }
                egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                    let tctx = tool_ctx(app);
                    app.tool.update(ToolEvent::Backspace, &tctx, &app.doc);
                }
                egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                    // Escape exits text-edit mode; not just a flush, so it's not routed through
                    // the generic dispatch above.
                    app.flush_text_tool();
                }
                egui::Event::Key { key: egui::Key::ArrowUp, pressed: true, .. } => {
                    let tctx = tool_ctx(app);
                    app.tool.update(ToolEvent::Arrow(Direction::Up), &tctx, &app.doc);
                }
                egui::Event::Key { key: egui::Key::ArrowDown, pressed: true, .. } => {
                    let tctx = tool_ctx(app);
                    app.tool.update(ToolEvent::Arrow(Direction::Down), &tctx, &app.doc);
                }
                egui::Event::Key { key: egui::Key::ArrowLeft, pressed: true, .. } => {
                    let tctx = tool_ctx(app);
                    app.tool.update(ToolEvent::Arrow(Direction::Left), &tctx, &app.doc);
                }
                egui::Event::Key { key: egui::Key::ArrowRight, pressed: true, .. } => {
                    let tctx = tool_ctx(app);
                    app.tool.update(ToolEvent::Arrow(Direction::Right), &tctx, &app.doc);
                }
                _ => {}
            }
        }
    }

    // Focus-loss detection: a burst mid-typing must commit, not vanish, when the OS window loses
    // focus. A no-op via flush_text_tool's own guard when not in text mode.
    let focused = ui.input(|i| i.viewport().focused).unwrap_or(true);
    if app.was_focused && !focused {
        app.flush_text_tool();
    }
    app.was_focused = focused;

    let visible = app.viewport.visible_cell_rect(painter.clip_rect(), cell, origin, doc_extent);

    ctx.request_repaint_after(std::time::Duration::from_millis(500));
    let on = cursor_blink_on(ui);
    let cursor_cell = Some(app.cursor).filter(|_| on);

    app.renderer.paint(
        &painter,
        &app.doc,
        &app.viewport,
        origin,
        cell,
        visible,
        app.tool.pending(),
        cursor_cell,
    );
}
