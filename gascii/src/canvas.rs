use eframe::egui::{self, Align2, Color32, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};
use gascii_core::{CellRect, Direction, Document, PendingCell, Rgba, SelectionView, ToolEvent, ToolResponse};

use crate::app::{GasciiApp, ToolKind};
use crate::fonts::canvas_font_id;
use crate::viewport::Viewport;

fn color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

/// Background the doc canvas paints onto before compositing transparent Blank cells.
const DOC_BG: Color32 = Color32::from_rgb(10, 10, 10);

/// Converts an inclusive cell-space rect to the screen-space rect covering all of its cells.
fn cell_rect_to_screen(r: CellRect, vp: &Viewport, cell: Vec2, origin: Pos2) -> Rect {
    let min = vp.cell_to_screen(r.x0, r.y0, cell, origin);
    let max = vp.cell_to_screen(r.x1 + 1, r.y1 + 1, cell, origin);
    Rect::from_min_max(min, max)
}

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
        selection: Option<SelectionView>,
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
        selection: Option<SelectionView>,
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

        // A floating stamp's vacated source region: painted as plain background, after doc cells
        // and before the pending overlay, so it reads as erased under the float even though the
        // document itself was never mutated.
        if let Some(src) = selection.and_then(|s| s.lifted_source) {
            painter.rect_filled(cell_rect_to_screen(src, vp, cell, origin), 0.0, DOC_BG);
        }

        for p in pending {
            if p.x < x0 || p.x >= x1 || p.y < y0 || p.y >= y1 {
                continue;
            }
            let rect_min = vp.cell_to_screen(p.x, p.y, cell, origin);
            let rect = Rect::from_min_size(rect_min, cell);
            // A pending cell is the exact result the commit will write, so the preview must
            // fully replace this destination cell down to the canvas background first — a Blank
            // pending cell (ch ' ', transparent bg) would otherwise leave whatever's already
            // painted there (the underlying doc glyph, or the vacated-source fill above) showing
            // through, contradicting what the drop actually produces.
            painter.rect_filled(rect, 0.0, DOC_BG);
            if p.cell.bg.3 > 0 {
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

        if let Some(marquee) = selection.and_then(|s| s.marquee) {
            let rect = cell_rect_to_screen(marquee, vp, cell, origin);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);
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
        density: app.density_mode,
        ramp: app.ramps[app.active_ramp].chars.clone(),
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
        } else if app.tool_kind == ToolKind::Selection {
            // Unlike the generic branch below, a Selection Press can itself return a committed
            // edit (clicking away from a floating stamp drops it) — that response must be applied,
            // not discarded, or a click-away drop would silently vanish.
            if response.contains_pointer() {
                if let Some(pos) = response.interact_pointer_pos() {
                    if let Some((x, y)) = app.viewport.screen_to_cell(pos, cell, origin, doc_extent) {
                        app.stroke_active = true;
                        stroke_just_started = true;
                        let tctx = tool_ctx(app);
                        let resp = app.tool.update(ToolEvent::Press { x, y }, &tctx, &app.doc);
                        if let ToolResponse::Commit(Some(edit)) = resp {
                            app.history.apply(&mut app.doc, edit);
                        }
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
                    app.flush_active_tool();
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

    // Selection keyboard routing: Delete clears the float/selection to Blank, Enter drops a
    // floating stamp, Escape cancels the marquee/float outright without touching the document.
    // Gated on no widget having focus, mirroring `handle_keys`'s `!focused` guard on the
    // single-letter tool keys: `TextEdit`'s own key handling (e.g. the hex color popup) reads
    // events via `filtered_events`, which clones rather than consumes, so an unguarded block here
    // would also fire on every Delete/Enter/Escape typed into an unrelated focused text field.
    let selection_keys_focused = ui.memory(|m| m.focused().is_some());
    if app.tool_kind == ToolKind::Selection && !selection_keys_focused {
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::Key { key: egui::Key::Delete, pressed: true, .. } => {
                    let tctx = tool_ctx(app);
                    let resp = app.tool.update(ToolEvent::Delete, &tctx, &app.doc);
                    if let ToolResponse::Commit(Some(edit)) = resp {
                        app.history.apply(&mut app.doc, edit);
                    }
                }
                egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                    app.flush_active_tool();
                }
                egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                    let tctx = tool_ctx(app);
                    app.tool.update(ToolEvent::Cancel, &tctx, &app.doc);
                }
                _ => {}
            }
        }
    }

    // Clipboard paste: lands as a floating Selection stamp regardless of the active tool. Read
    // (not consumed) alongside the text/selection keyboard blocks above — Event::Paste is never
    // matched by either of those, so there's no double-handling.
    let paste_texts: Vec<String> = ui.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Paste(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    });
    for text in paste_texts {
        app.paste_text(&text);
    }

    // Focus-loss detection: a burst mid-typing or a floating stamp must commit, not vanish, when
    // the OS window loses focus (flush_active_tool is a no-op for every other tool). Additionally,
    // an in-progress pointer gesture (stroke or space-pan) has no synthetic mouse-up on an OS-level
    // focus loss (e.g. alt-tabbing mid-drag) — left alone, `stroke_active`/`space_pan_active` would
    // stay stuck until the next primary press. Cancel the gesture outright so the tool and the app
    // both return to a clean idle state; this guards different flags than flush_active_tool (burst/
    // float vs. pointer-gesture ownership) so both run on the same focus-loss edge.
    let focused = ui.input(|i| i.viewport().focused).unwrap_or(true);
    if app.was_focused && !focused {
        app.flush_active_tool();
        if app.stroke_active || app.space_pan_active {
            let tctx = tool_ctx(app);
            app.tool.update(ToolEvent::Cancel, &tctx, &app.doc);
            app.stroke_active = false;
            app.space_pan_active = false;
        }
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
        app.tool.selection_overlay(),
    );
}
