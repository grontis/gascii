use eframe::egui::{self, Align2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
use gascii_core::{
    CellRect, Direction, DocExtent, Document, History, PendingCell, Rgba, SelectionView, Tool,
    ToolCtx, ToolEvent, ToolResponse,
};

use crate::app::{Binding, GasciiApp, ToolKind};
use crate::fonts::canvas_font_id;
use crate::viewport::Viewport;

fn color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

/// Background the doc canvas paints onto before compositing transparent Blank cells. A document
/// property, not a chrome colour — it does not follow the theme.
const DOC_BG: Color32 = crate::ui::theme::CANVAS_SURFACE;

/// The accent, used only on canvas overlays.
const ACCENT: Color32 = crate::ui::theme::CANVAS_ACCENT;

/// Minimum desk showing around the document card, per spec §4.
pub const DESK_MARGIN: f32 = 28.0;

/// The marquee's dash pattern, in points.
const MARQUEE_DASH: (f32, f32) = (4.0, 3.0);

/// Converts an inclusive cell-space rect to the screen-space rect covering all of its cells.
fn cell_rect_to_screen(r: CellRect, vp: &Viewport, cell: Vec2, origin: Pos2) -> Rect {
    let min = vp.cell_to_screen(r.x0, r.y0, cell, origin);
    let max = vp.cell_to_screen(r.x1 + 1, r.y1 + 1, cell, origin);
    Rect::from_min_max(min, max)
}

pub trait CanvasRenderer {
    /// `hover` is the cells the active tool's next application would land on — the hovered cell,
    /// expanded to the tool's footprint for sized tools; empty when no marker should show.
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
        hover: &[(u16, u16)],
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
        hover: &[(u16, u16)],
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

        // Cell cursor: a 1px accent outline on every cell the next application would land on. The
        // spec's outline-only treatment (a wash would obscure the very glyph you are aiming at); for
        // a sized tool the same outline traces each cell of the footprint.
        for &(hx, hy) in hover {
            if hx < x0 || hx >= x1 || hy < y0 || hy >= y1 {
                continue;
            }
            let rect = Rect::from_min_size(vp.cell_to_screen(hx, hy, cell, origin), cell);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
        }

        // The text caret stays a solid block — it marks an insertion point rather than a target, and
        // it blinks, so it must read differently from the cell cursor.
        if let Some((cx, cy)) = cursor_on {
            let rect = Rect::from_min_size(vp.cell_to_screen(cx, cy, cell, origin), cell);
            painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 120));
        }

        if let Some(marquee) = selection.and_then(|s| s.marquee) {
            let rect = cell_rect_to_screen(marquee, vp, cell, origin);
            painter.rect_filled(rect, 0.0, ACCENT.gamma_multiply(0.08));
            let c = rect;
            let corners = [c.left_top(), c.right_top(), c.right_bottom(), c.left_bottom(), c.left_top()];
            painter.extend(Shape::dashed_line(
                &corners,
                Stroke::new(1.0, ACCENT),
                MARQUEE_DASH.0,
                MARQUEE_DASH.1,
            ));
            size_tag(painter, c, marquee);
        }
    }
}

/// The marquee's live size readout: accent fill, canvas-coloured text, mono 10px, sitting just
/// outside the top-right corner so it never covers the cells being selected.
fn size_tag(painter: &Painter, rect: Rect, marquee: CellRect) {
    let text = format!("{}×{}", marquee.width(), marquee.height());
    let font = crate::fonts::mono_id(10.0);
    let galley = painter.layout_no_wrap(text, font, DOC_BG);
    let pad = Vec2::new(5.0, 1.0);
    let size = galley.size() + pad * 2.0;
    let tag = Rect::from_min_size(Pos2::new(rect.max.x - size.x, rect.min.y - size.y), size);
    painter.rect_filled(tag, 0.0, ACCENT);
    painter.galley(tag.min + pad, galley, DOC_BG);
}

fn arrow_direction(key: egui::Key) -> Option<Direction> {
    match key {
        egui::Key::ArrowUp => Some(Direction::Up),
        egui::Key::ArrowDown => Some(Direction::Down),
        egui::Key::ArrowLeft => Some(Direction::Left),
        egui::Key::ArrowRight => Some(Direction::Right),
        _ => None,
    }
}

/// Cursor blink: on for half of each 1s period. Caller drives repaint scheduling.
pub fn cursor_blink_on(ui: &egui::Ui) -> bool {
    let t = ui.input(|i| i.time);
    (t * 2.0) as i64 % 2 == 0
}

/// Outcome of `drive_stroke_tail` for the caller's ownership bookkeeping.
struct StrokeTail {
    ended: bool,
    committed: bool,
}

/// The drag/release tail of a pointer-stroke lifecycle, shared by the primary and right-click
/// gestures so there is exactly one copy of this state machine. Press-time ownership stays with
/// each caller — that half genuinely differs per button (tool special cases, space-pan
/// arbitration).
#[allow(clippy::too_many_arguments)]
fn drive_stroke_tail(
    tool: &mut dyn Tool,
    doc: &mut Document,
    history: &mut History,
    viewport: &Viewport,
    tctx: &ToolCtx,
    response: &egui::Response,
    cell: Vec2,
    origin: Pos2,
    doc_extent: DocExtent,
    down: bool,
    just_started: bool,
    ends: bool,
) -> StrokeTail {
    if down && !just_started {
        if let Some(pos) = response.interact_pointer_pos() {
            let (x, y) = viewport.screen_to_cell_clamped(pos, cell, origin, doc_extent);
            tool.update(ToolEvent::Drag { x, y }, tctx, doc);
        }
    }
    let mut committed = false;
    if ends {
        if let ToolResponse::Commit(Some(edit)) = tool.update(ToolEvent::Release, tctx, doc) {
            history.apply(doc, edit);
            committed = true;
        }
    }
    StrokeTail { ended: ends, committed }
}

/// The `ToolCtx` for one binding. Everything but the footprint is app-global shared state; the
/// size/shape come from that binding's own slot, so each button draws with its own stamp.
pub(crate) fn tool_ctx(app: &GasciiApp, b: Binding) -> gascii_core::ToolCtx {
    let stamp = app.slot(b).stamp();
    gascii_core::ToolCtx {
        layer: 0,
        glyph: app.active_glyph,
        fg: app.active_fg,
        bg: app.active_bg,
        mask: app.mask,
        density: app.density_mode,
        ramp: app.ramps[app.active_ramp].chars.clone(),
        size: stamp.size,
        shape: stamp.shape,
    }
}

/// One button's press against its own slot, at cell `(x, y)`. Returns whether a multi-frame gesture
/// now owns the canvas.
///
/// Nothing here is button-specific — that is the whole point of two symmetric slots. The Eyedropper
/// is the single remaining special case, because it is the one kind that isn't a `Tool`.
pub(crate) fn begin_gesture(app: &mut GasciiApp, b: Binding, x: u16, y: u16) -> bool {
    // Drawing with a button selects that button's segment in the options bar.
    app.options_focus = b;

    if app.slot(b).kind == ToolKind::Eyedropper {
        // A one-shot pick, not a gesture: there is no ownership to track and no `Edit` to apply.
        if let Some(picked) = app.doc.cell(0, x, y).copied() {
            let (fg, bg) = gascii_core::eyedrop(&picked);
            app.active_fg = fg;
            app.active_bg = bg;
        }
        return false;
    }

    // At most one cross-frame session exists at a time, across both bindings. Starting one finishes
    // the other slot's, which is what keeps two Selection bindings coherent (never two floats), lets
    // `keyboard_owner` be the unique session holder, and keeps "the selection" singular for
    // copy/paste. Only Text and Selection hold sessions, so a quick right-click erase under a live
    // burst still never disturbs it.
    if matches!(app.slot(b).kind, ToolKind::Text | ToolKind::Selection) {
        app.flush_slot(b.other());
        app.keyboard_owner = Some(b);
    }

    let tctx = tool_ctx(app, b);
    let resp = app.slots[b.ix()].tool.update(ToolEvent::Press { x, y }, &tctx, &app.doc);
    // Always apply the press response. This is what makes the bindings symmetric: a stroke tool's
    // `Press` returns `Active` and never matches, while Selection's press CAN commit (clicking away
    // from a float drops it) and discarding that would silently lose the drop. The old right-click
    // path was allowed to drop this response only because it was restricted to stroke tools — that
    // restriction, not the response, was the thing standing in the way.
    if let ToolResponse::Commit(Some(edit)) = resp {
        app.apply_edit(edit, Some(b));
    }
    app.gesture = Some(b);
    true
}

/// `pointer_on_resize_grip`: the pointer sits in the window-edge resize ring this frame. The press
/// branch must yield to it — this function reads raw pointer edges, not egui interactions, so the
/// grip cannot win any other way.
pub fn show(ui: &mut egui::Ui, app: &mut GasciiApp, pointer_on_resize_grip: bool) {
    let ctx = ui.ctx().clone();
    if app.pending_fit {
        app.viewport
            .fit_to_window(ui.available_size(), DESK_MARGIN, app.doc.extent(), &ctx);
        app.pending_fit = false;
    }

    let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
    let origin = response.rect.min;
    let mut cell = app.viewport.cell_size(&ctx);
    let doc_extent = app.doc.extent();

    // The close-confirm dialog is the only modal surface in this app, but this function polls raw
    // pointer/keyboard state (`ui.input(|i| i.pointer...)`) rather than using egui's occlusion
    // system, so `egui::Modal`'s backdrop does not itself block canvas interaction. Every
    // pointer/keyboard-consuming branch below (through focus-loss detection) is therefore gated
    // explicitly on the dialog flag. Rendering below this block stays unconditional — the canvas
    // keeps showing its last frame, frozen, underneath the dialog.
    if !app.close_dialog_open {
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
        // stroke). Space+primary-drag pans only while the space-pan gesture owns the primary
        // button (decided at press time below), so it never steals an in-progress stroke.
        if response.dragged_by(egui::PointerButton::Middle) {
            app.viewport.pan += response.drag_delta();
        }
        let space = ui.input(|i| i.key_down(egui::Key::Space));

        cell = app.viewport.cell_size(&ctx);
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
        let (primary_pressed, primary_down, primary_released, secondary_pressed, secondary_down, secondary_released) =
            ui.input(|i| {
                (
                    i.pointer.primary_pressed(),
                    i.pointer.primary_down(),
                    i.pointer.primary_released(),
                    i.pointer.secondary_pressed(),
                    i.pointer.secondary_down(),
                    i.pointer.secondary_released(),
                )
            });
        let gesture_ends = primary_released || !primary_down;

        // Tracks whether this frame's press just started the gesture, so the tail below doesn't also
        // send a same-frame, same-cell Drag for it — one pointer event in, one Tool event out.
        let mut gesture_just_started = false;

        // Press. One routine for both buttons: the slots are symmetric, so nothing here branches on
        // which one it is. Two things stay genuinely per-button — Space puts the canvas in navigate
        // mode (primary pans, secondary is inert, neither draws), and only one gesture may own the
        // canvas at a time. Two simultaneous strokes would interleave two `apply_edit` calls and pin
        // each slot's `before` values against the other's uncommitted writes.
        if app.gesture.is_none() && !app.space_pan_active && !pointer_on_resize_grip {
            if primary_pressed && space {
                app.space_pan_active = true;
            } else if !space {
                let pressed = if primary_pressed {
                    Some(Binding::L)
                } else if secondary_pressed {
                    Some(Binding::R)
                } else {
                    None
                };
                if let Some(b) = pressed.filter(|_| response.contains_pointer()) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        if let Some((x, y)) = app.viewport.screen_to_cell(pos, cell, origin, doc_extent) {
                            gesture_just_started = begin_gesture(app, b, x, y);
                        }
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
        } else if let Some(b) = app.gesture {
            let (down, ends) = match b {
                Binding::L => (primary_down, gesture_ends),
                Binding::R => (secondary_down, secondary_released || !secondary_down),
            };
            let tctx = tool_ctx(app, b);
            let tail = drive_stroke_tail(
                app.slots[b.ix()].tool.as_mut(),
                &mut app.doc,
                &mut app.history,
                &app.viewport,
                &tctx,
                &response,
                cell,
                origin,
                doc_extent,
                down,
                gesture_just_started,
                ends,
            );
            if tail.committed {
                // These cells were committed inside `drive_stroke_tail`, bypassing `apply_edit` —
                // so discharge the same obligation here: the other slot's pending session may now
                // hold `before` values pinned against the pre-commit document.
                app.resync_slots(Some(b));
                // A committed stroke that stamped the active glyph counts as "using" it.
                let kind = app.slots[b.ix()].kind;
                app.note_glyph_drawn(kind);
            }
            if tail.ended {
                app.gesture = None;
            }
        }

        // Keyboard routing: the owning slot's tool receives keys, dispatched by that slot's kind.
        // At most one slot owns the keyboard, so the Text and Selection routings are mutually
        // exclusive by construction — the pre-slot code relied on `tool_kind` being global for that.
        //
        // Both are gated on no widget having focus. `TextEdit`'s own key handling (e.g. the hex
        // color popup) reads events via `filtered_events`, which clones rather than consumes, so an
        // unguarded block fires on keys typed into an unrelated focused field. The selection block
        // always had this guard; the text block did not, and so fed `Event::Text` to `TextTool`
        // while you typed into the color picker. Unifying them fixes that.
        let widget_focused = ui.memory(|m| m.focused().is_some());
        if let Some(b) = app.keyboard_owner.filter(|_| !widget_focused) {
            let bi = b.ix();
            let events = ui.input(|i| i.events.clone());
            match app.slots[bi].kind {
                ToolKind::Text => {
                    for ev in events {
                        match ev {
                            egui::Event::Text(s) => {
                                for ch in s.chars() {
                                    // The tool's own entry validation drops a rejected character
                                    // either way; this pre-check only makes the drop visible.
                                    if let Err(reject) = gascii_core::validate_width(ch) {
                                        app.warn_rejected_char(ch, reject);
                                        continue;
                                    }
                                    let tctx = tool_ctx(app, b);
                                    let resp =
                                        app.slots[bi].tool.update(ToolEvent::Char(ch), &tctx, &app.doc);
                                    if let ToolResponse::Commit(Some(edit)) = resp {
                                        app.apply_edit(edit, Some(b));
                                    }
                                }
                            }
                            egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                                let tctx = tool_ctx(app, b);
                                app.slots[bi].tool.update(ToolEvent::Enter, &tctx, &app.doc);
                            }
                            egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                                let tctx = tool_ctx(app, b);
                                app.slots[bi].tool.update(ToolEvent::Backspace, &tctx, &app.doc);
                            }
                            egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                                // Escape ends the session; only the owner's, never the other slot's.
                                app.flush_slot(b);
                            }
                            egui::Event::Key { key, pressed: true, .. } => {
                                if let Some(dir) = arrow_direction(key) {
                                    let tctx = tool_ctx(app, b);
                                    app.slots[bi].tool.update(ToolEvent::Arrow(dir), &tctx, &app.doc);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                ToolKind::Selection => {
                    for ev in events {
                        match ev {
                            egui::Event::Key { key: egui::Key::Delete, pressed: true, .. } => {
                                let tctx = tool_ctx(app, b);
                                let resp = app.slots[bi].tool.update(ToolEvent::Delete, &tctx, &app.doc);
                                if let ToolResponse::Commit(Some(edit)) = resp {
                                    app.apply_edit(edit, Some(b));
                                }
                            }
                            egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                                app.flush_slot(b);
                            }
                            egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                                let tctx = tool_ctx(app, b);
                                app.slots[bi].tool.update(ToolEvent::Cancel, &tctx, &app.doc);
                                app.keyboard_owner = None;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
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
        // the OS window loses focus (a no-op for every other tool). Additionally, an in-progress
        // gesture has no synthetic mouse-up on an OS-level focus loss (e.g. alt-tabbing mid-drag) —
        // left alone, `gesture`/`space_pan_active` would stay stuck until the next press. Cancel it
        // outright so the tool and the app both return to a clean idle state; this guards different
        // state than the flush (session vs. pointer-gesture ownership), so both run on the same edge.
        let focused = ui.input(|i| i.viewport().focused).unwrap_or(true);
        if app.was_focused && !focused {
            app.flush_all();
            if let Some(b) = app.gesture.take() {
                let tctx = tool_ctx(app, b);
                app.slots[b.ix()].tool.update(ToolEvent::Cancel, &tctx, &app.doc);
            }
            app.space_pan_active = false;
        }
        app.was_focused = focused;
    }

    let visible = app.viewport.visible_cell_rect(painter.clip_rect(), cell, origin, doc_extent);

    // The text caret follows keyboard ownership, which is what keeps it honest: no caret means "not
    // accepting keys". It reads the same state the routing above does, so the caret can never
    // advertise a session whose typing would be dropped or consumed as tool-switch keys. Clamped for
    // display — the tool's cursor may sit one column past the right edge after typing a full row.
    // The blink is the only animation needing unprompted repaints, so the wakeup is gated on it.
    let caret = app
        .keyboard_owner
        .filter(|&b| app.slot(b).kind == ToolKind::Text)
        .and_then(|b| app.slot(b).tool.caret())
        .map(|(x, y)| (x.min(app.doc.width.saturating_sub(1)), y.min(app.doc.height.saturating_sub(1))));
    if caret.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
    let caret_cell = caret.filter(|_| cursor_blink_on(ui));

    // Hover marker: the cells the active tool's next click/stroke would land on, footprint-
    // expanded for sized tools using the ACTIVE tool's own stamp (a right-click stroke's
    // footprint differs, but hover can't know which button is coming — the left-click tool is
    // the honest default). Hidden while any gesture owns the canvas: mid-stroke, the pending
    // overlay is already the real preview.
    let mut hover_cells: Vec<(u16, u16)> = Vec::new();
    if let Some((hx, hy)) = app.hovered_cell {
        if !app.stroke_in_progress()
            && !app.space_pan_active
            && crate::app::tool_shows_hover(app.slot(Binding::L).kind)
        {
            if crate::app::tool_is_sized(app.slot(Binding::L).kind) {
                let stamp = app.slot(Binding::L).stamp();
                gascii_core::footprint((hx, hy), stamp.size, stamp.shape, &mut hover_cells);
                hover_cells.retain(|&(x, y)| app.doc.in_bounds(x, y));
            } else {
                hover_cells.push((hx, hy));
            }
        }
    }

    // Overlay ordering = commit ordering: whichever slot commits last wins any overlapped cell, so
    // it must paint on top, or the preview promises an outcome the commits then reverse. The
    // gesturing slot commits at its imminent release and therefore goes underneath. `commit_order`
    // is the single definition of that, shared with `flush_all`. The concat clone is skipped in the
    // common case where only one slot has anything pending.
    // Indexed directly rather than through `app.slot()`: that accessor borrows all of `app`, which
    // would collide with the `&mut app.renderer` below.
    let [first, second] = app.commit_order();
    let (under, over) = (
        app.slots[first.ix()].tool.pending(),
        app.slots[second.ix()].tool.pending(),
    );
    let mut combined;
    let pending: &[PendingCell] = if under.is_empty() {
        over
    } else if over.is_empty() {
        under
    } else {
        combined = under.to_vec();
        combined.extend_from_slice(over);
        &combined
    };

    // At most one session exists at a time, so at most one slot has a selection overlay; the
    // commit-order scan just finds it without caring which binding it belongs to.
    let selection = [first, second]
        .iter()
        .find_map(|&b| app.slots[b.ix()].tool.selection_overlay());

    // The document as a card on the desk: a hard 3px offset shadow under it, a 1px window-edge
    // border over it. Painted here rather than in the renderer because the border is a chrome
    // colour and follows the theme, while everything the renderer draws is document content and
    // deliberately does not.
    let t = crate::ui::theme::current(&ctx);
    let doc_rect = Rect::from_min_size(
        origin + app.viewport.pan,
        Vec2::new(app.doc.width as f32 * cell.x, app.doc.height as f32 * cell.y),
    );
    painter.rect_filled(doc_rect.translate(Vec2::splat(3.0)), 0.0, t.shadow);

    app.renderer.paint(
        &painter,
        &app.doc,
        &app.viewport,
        origin,
        cell,
        visible,
        pending,
        &hover_cells,
        caret_cell,
        selection,
    );

    painter.rect_stroke(doc_rect, 0.0, Stroke::new(1.0, t.window_edge), StrokeKind::Outside);
}
