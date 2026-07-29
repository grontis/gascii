use eframe::egui::{self, Align2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};
use gascii_core::{
    CellRect, DensityMode, Direction, DocExtent, Document, Edit, Fixed, PendingCell, Rgba,
    SelectionView, Tool, ToolCtx, ToolEvent, ToolResponse,
};
use gascii_plugin_api::{cell_rect_to_screen, CanvasRenderer, CellGrid};

use crate::app::{tool_def, Binding, GasciiApp, ToolKind};
use crate::fonts::canvas_font_id;
use crate::viewport::Viewport;

/// The `ToolCtx.density` a tool that never reads it gets — a pure "doesn't care" placeholder,
/// matching the literal `GasciiApp::with_state` used to initialize its own (now-deleted)
/// `density_mode` field before this migration, so this stays a pure refactor rather than a
/// behavior change.
const DEFAULT_DENSITY: DensityMode = DensityMode::Fixed(Fixed(1.0));

fn color32(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

/// The size tag's own text color, chosen for legibility against the accent fill it sits on — not
/// the live document's background (see `doc.background` in `NaiveRenderer::paint` for that).
const TAG_FG: Color32 = crate::ui::theme::CANVAS_SURFACE;

/// The accent, used only on canvas overlays.
const ACCENT: Color32 = crate::ui::theme::CANVAS_ACCENT;

/// Minimum desk showing around the document card.
pub const DESK_MARGIN: f32 = 28.0;

/// The marquee's dash pattern, in points.
const MARQUEE_DASH: (f32, f32) = (4.0, 3.0);

/// Default renderer: per-cell `Painter::text`/`rect_filled`, no caching.
pub struct NaiveRenderer;

impl CanvasRenderer for NaiveRenderer {
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
        let (x0, y0, x1, y1) = visible;
        let doc_bg = color32(doc.background);

        // The full-`doc_rect` background fill lives in `show()`, ahead of the trace-image block —
        // not here — so the trace paints above it instead of being immediately painted over. This
        // renderer only ever fills `doc_bg` per-cell (vacated float regions, pending-cell previews)
        // from here down.
        let font_id = canvas_font_id(vp.font_px());
        for y in y0..y1 {
            for x in x0..x1 {
                // Literal, not `app.active_layer`: this renderer has no `GasciiApp` handle, only
                // `doc`. v1 documents have exactly one layer, so this is behavior-identical.
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
            painter.rect_filled(cell_rect_to_screen(src, vp, cell, origin), 0.0, doc_bg);
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
            painter.rect_filled(rect, 0.0, doc_bg);
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

        // Cell cursor: a 1px accent outline on every cell the next application would land on.
        // Outline only — a wash would obscure the very glyph you are aiming at; for a sized tool
        // the same outline traces each cell of the footprint.
        for &(hx, hy) in hover {
            if hx < x0 || hx >= x1 || hy < y0 || hy >= y1 {
                continue;
            }
            let rect = Rect::from_min_size(vp.cell_to_screen(hx, hy, cell, origin), cell);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
        }

        // The text caret: a solid block during the blink's on-phase — it marks an insertion point
        // rather than a target, and it blinks, so it must read differently from the cell cursor —
        // plus a persistent underscore so the insertion point never fully vanishes between blinks.
        if let Some((cx, cy, block_on)) = caret {
            let rect = Rect::from_min_size(vp.cell_to_screen(cx, cy, cell, origin), cell);
            if block_on {
                painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 120));
            }
            let h = (cell.y * 0.12).max(1.0);
            let underscore = Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - h), rect.max);
            painter.rect_filled(underscore, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 200));
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
    let font = crate::fonts::mono_id(crate::fonts::size::TAG);
    let galley = painter.layout_no_wrap(text, font, TAG_FG);
    let pad = Vec2::new(5.0, 1.0);
    let size = galley.size() + pad * 2.0;
    let tag = Rect::from_min_size(Pos2::new(rect.max.x - size.x, rect.min.y - size.y), size);
    painter.rect_filled(tag, 0.0, ACCENT);
    painter.galley(tag.min + pad, galley, TAG_FG);
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
    edit: Option<Edit>,
    /// The (possibly Shift-constrained) cell this call's `Drag` landed on, if any moved this frame
    /// — the caller's source of truth for a committed `Line`'s terminal point
    /// (`ToolSlot::last_line_point`), so the shift-click-continue feature always continues from
    /// exactly where the stroke actually ended, not a separately re-derived value.
    last_drag_cell: Option<(u16, u16)>,
}

/// Snaps `cur` toward a Shift-constrained target relative to `anchor`, for the two tool kinds that
/// support it — `Line` (nearest of the 8 compass rays, 0/45/90/.../315°) and `Rectangle` (a square,
/// whose side is whichever axis the pointer moved further along — the dominant axis "wins" and the
/// other one is stretched to match, preserving direction on both). Returns `cur` unchanged for
/// every other kind, whenever `shift` is false, or when `cur == anchor` (no direction to snap to)
/// — the unconstrained path is a byte-for-byte passthrough of whatever `drive_stroke_tail` already
/// computed, by construction, satisfying the "existing Line/Rectangle drag behavior is unchanged
/// when Shift is not held" requirement without a separate code path.
///
/// Integer-only (no floating point): the 45°-ray boundaries at 22.5°/67.5° are approximated via
/// `adx * 1000` vs. `ady * TAN_67_5_X1000` cross-multiplication rather than `atan2`, so the result
/// is exactly reproducible and trivially unit-testable against round-number vectors.
pub(crate) fn shift_constrain(kind: ToolKind, anchor: (u16, u16), cur: (u16, u16), shift: bool) -> (u16, u16) {
    if !shift || !matches!(kind, ToolKind::Line | ToolKind::Rectangle) {
        return cur;
    }
    let dx = cur.0 as i32 - anchor.0 as i32;
    let dy = cur.1 as i32 - anchor.1 as i32;
    let (adx, ady) = (dx.abs(), dy.abs());
    if adx == 0 && ady == 0 {
        return cur;
    }
    const TAN_67_5_X1000: i32 = 2414; // tan(67.5°) * 1000, the 22.5°/67.5° ray-boundary ratio
    let (ndx, ndy) = match kind {
        ToolKind::Rectangle => {
            let side = adx.max(ady);
            (side * dx.signum(), side * dy.signum())
        }
        ToolKind::Line => {
            if adx * 1000 > ady * TAN_67_5_X1000 {
                (dx, 0) // within 22.5° of horizontal
            } else if ady * 1000 > adx * TAN_67_5_X1000 {
                (0, dy) // within 22.5° of vertical
            } else {
                let diag = adx.max(ady); // the 45° diagonal ray
                (diag * dx.signum(), diag * dy.signum())
            }
        }
        _ => unreachable!("gated by the matches! check above"),
    };
    (
        (anchor.0 as i32 + ndx).clamp(0, u16::MAX as i32) as u16,
        (anchor.1 as i32 + ndy).clamp(0, u16::MAX as i32) as u16,
    )
}

/// The drag/release tail of a pointer-stroke lifecycle, shared by the primary and right-click
/// gestures so there is exactly one copy of this state machine. Press-time ownership stays with
/// each caller — that half genuinely differs per button (tool special cases, space-pan
/// arbitration).
///
/// Never mutates the document itself — `Tool::update` only ever takes `&Document` — and returns
/// any committed `Edit` rather than applying it, so the caller can route it through `apply_edit`,
/// keeping that the crate's one and only `History::apply` call site.
#[allow(clippy::too_many_arguments)]
fn drive_stroke_tail(
    tool: &mut dyn Tool,
    doc: &Document,
    viewport: &Viewport,
    tctx: &ToolCtx,
    response: &egui::Response,
    cell: Vec2,
    origin: Pos2,
    doc_extent: DocExtent,
    down: bool,
    just_started: bool,
    ends: bool,
    kind: ToolKind,
    anchor: Option<(u16, u16)>,
) -> StrokeTail {
    let mut last_drag_cell = None;
    if down && !just_started {
        if let Some(pos) = response.interact_pointer_pos() {
            let (x, y) = viewport.screen_to_cell_clamped(pos, cell, origin, doc_extent);
            // Shift held live, read off `response`'s own `Context` — `drive_stroke_tail` takes no
            // `ui`/ambient input handle otherwise, and `egui::Response::ctx` is exactly this app's
            // one live `Context`, so this needs no extra parameter.
            let shift = response.ctx.input(|i| i.modifiers.shift);
            let (x, y) = match anchor {
                Some(a) => shift_constrain(kind, a, (x, y), shift),
                None => (x, y),
            };
            last_drag_cell = Some((x, y));
            tool.update(ToolEvent::Drag { x, y }, tctx, doc);
        }
    }
    let mut edit = None;
    if ends {
        if let ToolResponse::Commit(Some(e)) = tool.update(ToolEvent::Release, tctx, doc) {
            edit = Some(e);
        }
    }
    StrokeTail { ended: ends, edit, last_drag_cell }
}

/// The `ToolCtx` for one binding. Everything but the footprint is app-global shared state; the
/// size/shape come from that binding's own slot, so each button draws with its own stamp.
///
/// Size has one exception: while `b` is the live stroke owner, a pending stylus-pressure override
/// (`pressure_stamp_size`) takes precedence over the slot's remembered `StampSettings.size`. This
/// is a read-only substitution — the slot's stored size is never written by pressure, so the
/// binding's configured/persisted size survives the stroke unchanged.
pub(crate) fn tool_ctx(app: &GasciiApp, b: Binding) -> gascii_core::ToolCtx {
    let stamp = app.slot(b).stamp();
    let size = if app.stroke_owner == Some(b) {
        app.pressure_stamp_size.unwrap_or(stamp.size)
    } else {
        stamp.size
    };
    // Only a tool whose row asks for it (Brush's, via `wants_extra_ctx`) reads `density`/`ramp`;
    // for every other tool the ramp clone would be a per-drag-frame allocation on the stroke hot
    // path for data it ignores.
    let kind = app.slot(b).kind;
    let (density, ramp) = if tool_def(kind).wants_extra_ctx {
        let i = tool_def(kind).plugin_slot.expect("wants_extra_ctx implies a plugin_slot");
        app.plugins[i].extra_tool_ctx(tool_def(kind).name).unwrap_or((DEFAULT_DENSITY, Vec::new()))
    } else {
        (DEFAULT_DENSITY, Vec::new())
    };
    gascii_core::ToolCtx {
        frame: app.active_frame,
        layer: app.active_layer,
        glyph: app.active_glyph,
        fg: app.active_fg,
        bg: app.active_bg,
        mask: app.mask,
        density,
        ramp,
        size,
        shape: stamp.shape,
    }
}

/// One button's press against its own slot, at cell `(x, y)`. Returns whether a multi-frame gesture
/// now owns the canvas.
///
/// Nothing here is button-specific — that is the whole point of two symmetric slots. The Eyedropper
/// is the single remaining special case, because it is the one kind that isn't a `Tool`.
///
/// `alt_sample`: the Alt-hold temporary eyedropper — Alt held at press time samples color exactly
/// like a real Eyedropper press, without ever touching `self.slots[b.ix()].kind`. Ephemeral and
/// press-time-only: no transient field, no restore bookkeeping, nothing for any other path to know
/// about.
///
/// `shift_held`: Shift-click-continue on a Line — a Shift-held press on a `Line` binding with a
/// remembered `last_line_point` replays as a `Press` at that remembered point immediately followed
/// by a `Drag` to the actual click, instead of starting a fresh line at the click. The pointer's
/// subsequent mouse-up still fires an ordinary `Release` through the unchanged `stroke_owner`
/// machinery below.
pub(crate) fn begin_gesture(app: &mut GasciiApp, b: Binding, x: u16, y: u16, alt_sample: bool, shift_held: bool) -> bool {
    // Drawing with a button focuses that binding for the [/] size keys.
    app.options_focus = b;

    if app.slot(b).kind == ToolKind::Eyedropper || alt_sample {
        // A one-shot pick, not a gesture: there is no ownership to track and no `Edit` to apply.
        if let Some(picked) = app.doc.cell(app.active_layer, x, y).copied() {
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
    if crate::app::holds_session(app.slot(b).kind) {
        app.end_session(b.other());
        app.acquire_keyboard(b);
    }

    let continue_from = (app.slot(b).kind == ToolKind::Line && shift_held)
        .then(|| app.slots[b.ix()].last_line_point)
        .flatten();

    let tctx = tool_ctx(app, b);
    let resp = if let Some(lp) = continue_from {
        app.slots[b.ix()].tool.update(ToolEvent::Press { x: lp.0, y: lp.1 }, &tctx, &app.doc);
        app.slots[b.ix()].tool.update(ToolEvent::Drag { x, y }, &tctx, &app.doc)
    } else {
        app.slots[b.ix()].tool.update(ToolEvent::Press { x, y }, &tctx, &app.doc)
    };
    // Always apply the press response. This is what makes the bindings symmetric: a stroke tool's
    // `Press` returns `Active` and never matches, while Selection's press CAN commit (clicking away
    // from a float drops it) and discarding that would silently lose the drop.
    if let ToolResponse::Commit(Some(edit)) = resp {
        app.apply_edit(edit, Some(b));
    }
    app.stroke_owner = Some(b);
    // The anchor `shift_constrain` measures a Shift-held drag against — the remembered
    // continuation point when replaying one, otherwise the actual press cell, matching every other
    // gesture.
    app.stroke_press_cell = Some(continue_from.unwrap_or((x, y)));
    // A fresh stroke starts with no pressure override — this stroke hasn't reported any `force`
    // yet, so it must draw at the slot's configured size until it does, not a leftover value from
    // whatever stroke (or binding) last set one.
    app.pressure_stamp_size = None;
    true
}

/// `pointer_on_resize_grip`: the pointer sits in the window-edge resize ring this frame. The press
/// branch must yield to it — this function reads raw pointer edges, not egui interactions, so the
/// grip cannot win any other way.
pub fn show(ui: &mut egui::Ui, app: &mut GasciiApp, pointer_on_resize_grip: bool) {
    let ctx = ui.ctx().clone();
    let is_fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    if app.pending_fit {
        app.pending_fit = false;
        // Dropped mid-stroke, same policy as `pending_step_zoom`: an unconditional recenter
        // would remap the still pointer to a different cell under a live gesture.
        if !app.stroke_in_progress() {
            app.viewport
                .fit_to_window(ui.available_size(), DESK_MARGIN, app.doc.extent(), &ctx);
            app.kiosk_last_fit_size = Some(ui.available_size());
        }
    } else if is_fullscreen {
        // Kiosk's zoom stays "auto": re-fit whenever the canvas area's own size changes (window
        // resize, monitor change, sidebar geometry change), but not unconditionally every frame.
        // Held off (not skipped) mid-stroke — same reason as `pending_fit`/`pending_step_zoom`
        // above: a refit remaps the still pointer to a different cell under a live gesture. The
        // size mismatch persists, so the refit lands on the first frame after release.
        let avail = ui.available_size();
        if app.kiosk_last_fit_size != Some(avail) && !app.stroke_in_progress() {
            app.viewport.fit_to_window(avail, DESK_MARGIN, app.doc.extent(), &ctx);
            app.kiosk_last_fit_size = Some(avail);
        }
    } else {
        // Outside fullscreen this is stale by construction; clearing it means re-entering kiosk
        // later always re-fits at least once rather than trusting a leftover size match.
        app.kiosk_last_fit_size = None;
    }

    let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
    let origin = response.rect.min;
    let mut cell = app.viewport.cell_size(&ctx);
    let doc_extent = app.doc.extent();

    // This function polls raw pointer/keyboard state (`ui.input(|i| i.pointer...)`) rather than
    // using egui's occlusion system, so no modal's backdrop blocks canvas interaction on its own —
    // any modal flag must gate this section explicitly, which is exactly what `modal_open()` is
    // for. Rendering below this block stays unconditional — the canvas keeps showing its last
    // frame, frozen, underneath whichever dialog is open.
    if !app.modal_open() {
        // Precedence 1: zoom. Allowed any time, including mid-stroke — pending cells are
        // cell-addressed and stay valid; the cursor-anchored zoom keeps the pointer's cell fixed.
        let (scroll_y, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
        if ctrl && scroll_y != 0.0 {
            if let Some(cursor) = response.hover_pos() {
                app.viewport
                    .zoom_at(cursor, scroll_y.signum() as i32, cell, origin);
            }
        }

        // Precedence 1b: two-finger pinch, independent of Ctrl+scroll. `zoom_delta` is a per-frame
        // ratio (1.0 = no change), not a cumulative gesture magnitude, so it is multiplied into an
        // accumulator that persists across frames; once the accumulator has drifted 15% from
        // neutral, one discrete zoom step fires against the cell size's own six-step scale and the
        // accumulator resets. Also pans by the gesture's own translation, so the fingers can
        // recentre the view while pinching.
        if let Some(multi) = ui.input(|i| i.multi_touch()) {
            app.viewport.pan += multi.translation_delta;
            app.pinch_zoom_accum *= multi.zoom_delta;
            const PINCH_THRESHOLD: f32 = 0.15;
            if app.pinch_zoom_accum > 1.0 + PINCH_THRESHOLD {
                app.viewport.zoom_at(multi.center_pos, 1, cell, origin);
                app.pinch_zoom_accum = 1.0;
            } else if app.pinch_zoom_accum < 1.0 - PINCH_THRESHOLD {
                app.viewport.zoom_at(multi.center_pos, -1, cell, origin);
                app.pinch_zoom_accum = 1.0;
            }
        } else {
            // No active gesture: reset rather than let a stale accumulator from a prior pinch
            // trigger an unexpected zoom on the very first frame of the next one.
            app.pinch_zoom_accum = 1.0;
        }

        // Precedence 1c: deferred step zoom (`+`/`-` chords, View menu, status bar). Anchored on
        // the pointer when it's over the canvas — the same contract that makes the wheel path
        // safe — else on the viewport's visible center. Dropped (not held) mid-stroke: an anchored
        // zoom keeps the pointer's *cell* fixed but a keyboard zoom isn't under the pointer's
        // control, so firing it into a live stroke still surprises; the user can re-press after
        // release.
        if app.pending_step_zoom != 0 {
            let dir = app.pending_step_zoom;
            app.pending_step_zoom = 0;
            if !app.stroke_in_progress() {
                let anchor = response.hover_pos().unwrap_or_else(|| response.rect.center());
                app.viewport.zoom_at(anchor, dir, cell, origin);
            }
        }

        // Precedence 2: pan. Middle-drag is always available (never conflicts with a primary
        // stroke). Space+primary-drag pans only while the space-pan gesture owns the primary
        // button (decided at press time below), so it never steals an in-progress stroke.
        if response.dragged_by(egui::PointerButton::Middle) {
            app.viewport.pan += response.drag_delta();
        }
        let space = ui.input(|i| i.key_down(egui::Key::Space));
        // Alt held at press time: a temporary eyedropper, regardless of the bound kind — see
        // `begin_gesture`'s own doc comment.
        let alt_sample = ui.input(|i| i.modifiers.alt);
        // Shift held at press time: shift-click-continue on a Line binding — see `begin_gesture`'s
        // own doc comment.
        let shift_held = ui.input(|i| i.modifiers.shift);

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
        if app.stroke_owner.is_none() && !app.space_pan_active && !pointer_on_resize_grip {
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
                            gesture_just_started = begin_gesture(app, b, x, y, alt_sample, shift_held);
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
        } else if let Some(b) = app.stroke_owner {
            let (down, ends) = match b {
                Binding::L => (primary_down, gesture_ends),
                Binding::R => (secondary_down, secondary_released || !secondary_down),
            };
            let tctx = tool_ctx(app, b);
            let kind = app.slots[b.ix()].kind;
            let tail = drive_stroke_tail(
                app.slots[b.ix()].tool.as_mut(),
                &app.doc,
                &app.viewport,
                &tctx,
                &response,
                cell,
                origin,
                doc_extent,
                down,
                gesture_just_started,
                ends,
                kind,
                app.stroke_press_cell,
            );
            if let Some(edit) = tail.edit {
                // `apply_edit` performs its own `resync_slots(Some(b))` — the other slot's pending
                // session may now hold `before` values pinned against the pre-commit document.
                app.apply_edit(edit, Some(b));
                // A committed stroke that stamped the active glyph counts as "using" it.
                app.note_glyph_drawn(kind);
                // A committed Line stroke's terminal cell becomes the next Shift-click's
                // continuation point (`begin_gesture`'s own shift-click-continue branch). Falls
                // back to the press cell for a plain click-with-no-drag commit (a zero-length
                // line), whose terminal point IS the press cell — `drive_stroke_tail` never emits a
                // `Drag` for that case.
                if kind == ToolKind::Line {
                    if let Some(p) = tail.last_drag_cell.or(app.stroke_press_cell) {
                        app.slots[b.ix()].last_line_point = Some(p);
                    }
                }
            }
            if tail.ended {
                app.stroke_owner = None;
                app.pressure_stamp_size = None;
                app.stroke_press_cell = None;
            }
        }

        // Keyboard routing: the owning slot's tool receives keys, dispatched by that slot's kind.
        // At most one slot owns the keyboard, so the Text and Selection routings are mutually
        // exclusive by construction.
        //
        // Both are gated on no widget having focus. `TextEdit`'s own key handling (e.g. the hex
        // color popup) reads events via `filtered_events`, which clones rather than consumes, so an
        // unguarded block would fire on keys typed into an unrelated focused field — feeding
        // `Event::Text` to `TextTool` while you type into the color picker.
        let widget_focused = ui.memory(|m| m.focused().is_some());
        if let Some(b) = app.keyboard_owner().filter(|_| !widget_focused) {
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
                                app.end_session(b);
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
                                // Bespoke, deliberately non-flushing: Escape-as-abort must be able to
                                // discard an in-progress move rather than commit it, so this does NOT
                                // route through `end_session` (which always commits first).
                                let tctx = tool_ctx(app, b);
                                app.slots[bi].tool.update(ToolEvent::Cancel, &tctx, &app.doc);
                                app.release_keyboard(b);
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

        // Stylus pressure. `force` is `Some` only for an actual contact — never hover — so this
        // naturally only fires mid-stroke, exactly when it should affect what's being stamped. The
        // quantized size lands in `pressure_stamp_size`, a transient override `tool_ctx` consults
        // for the gesturing binding only — it never writes the slot's own `StampSettings.size`, so
        // the Size stepper's/`[`/`]`-configured value (what `prefs.rs` persists) survives the
        // stroke untouched.
        let latest_force: Option<f32> = ui.input(|i| {
            i.events.iter().rev().find_map(|e| match e {
                egui::Event::Touch { force: Some(f), .. } => Some(*f),
                _ => None,
            })
        });
        if let Some(force) = latest_force {
            app.stylus_detected = true;
            if let Some(b) = app.stroke_owner {
                let td = tool_def(app.slot(b).kind);
                let overridden = td.pressure_sizeable
                    && td.plugin_slot.is_some_and(|i| app.plugins[i].pressure_override_enabled(td.name));
                if overridden {
                    let quantized = 1 + (force.clamp(0.0, 1.0) * 3.0).round() as u16; // 1..=4
                    app.pressure_stamp_size = Some(quantized);
                }
            }
        }

        // Focus-loss detection: a burst mid-typing or a floating stamp must commit, not vanish, when
        // the OS window loses focus (a no-op for every other tool). Additionally, an in-progress
        // stroke has no synthetic mouse-up on an OS-level focus loss (e.g. alt-tabbing mid-drag) —
        // left alone, `stroke_owner`/`space_pan_active` would stay stuck until the next press. Cancel
        // it outright so the tool and the app both return to a clean idle state; this guards
        // different state than the flush (session vs. pointer-stroke ownership), so both run on the
        // same edge.
        let focused = ui.input(|i| i.viewport().focused).unwrap_or(true);
        if app.was_focused && !focused {
            // Flush first — it commits even a mid-stroke session, so the Cancel below only ever
            // clears pointer-gesture state, never uncommitted work.
            app.flush_all();
            if let Some(b) = app.stroke_owner.take() {
                app.pressure_stamp_size = None;
                let tctx = tool_ctx(app, b);
                app.slots[b.ix()].tool.update(ToolEvent::Cancel, &tctx, &app.doc);
                // The Cancel just cleared this binding's residue (caret, marquee); a keyboard
                // claim pointing at residue-free Text would silently swallow every keystroke on
                // return, with no caret to explain why.
                app.release_keyboard(b);
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
        .keyboard_owner()
        .filter(|&b| app.slot(b).kind == ToolKind::Text)
        .and_then(|b| app.slot(b).tool.caret())
        .map(|(x, y)| (x.min(app.doc.width.saturating_sub(1)), y.min(app.doc.height.saturating_sub(1))));
    if caret.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
    let caret_cell = caret.map(|(x, y)| (x, y, cursor_blink_on(ui)));

    // Preview target: the binding whose next stamp the outline should show. Mid-stroke that's the
    // gesturing binding itself — the outline then shows where the *next* stamp lands, which is
    // complementary to the pending overlay (what's already stamped), not redundant with it. Idle,
    // it falls back to L, the honest default when hover can't know which button is coming next.
    let preview_b = app.stroke_owner.unwrap_or(Binding::L);
    let preview_kind = app.slot(preview_b).kind;
    // Unclamped mapping, unlike the drag path's own clamped `screen_to_cell_clamped`: the preview
    // should vanish once the pointer leaves the document, not stick to its edge.
    let preview_center = if app.stroke_in_progress() {
        response
            .interact_pointer_pos()
            .and_then(|p| app.viewport.screen_to_cell(p, cell, origin, doc_extent))
    } else {
        app.hovered_cell
    };

    let mut hover_cells: Vec<(u16, u16)> = Vec::new();
    if let Some((hx, hy)) = preview_center {
        if !app.space_pan_active && crate::app::tool_shows_hover(preview_kind) {
            if crate::app::tool_is_sized(preview_kind) {
                let stamp = app.slot(preview_b).stamp();
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

    // The document's own solid background, filled here — ahead of the trace image and
    // `renderer.paint` — rather than as the renderer's first operation: the renderer paints only
    // cells, so this is the one full-`doc_rect` fill in the stack and everything above it (trace,
    // then cells) is guaranteed to land on top rather than risk being painted over.
    painter.rect_filled(doc_rect, 0.0, color32(app.doc.background));

    // The trace image: a tracing aid shown above that solid fill and under the document's cells,
    // letterboxed (`fit_contain`) into `doc_rect` so it tracks pan/zoom for free. `texture: None`
    // (not yet uploaded, or a headless test) is a pure no-op — nothing paints.
    if let Some(bg) = &app.image_bg {
        if bg.show_as_trace {
            if let Some(tex) = &bg.texture {
                if let Some((ox, oy, w, h)) =
                    crate::image_bg::fit_contain(bg.pixels.width(), bg.pixels.height(), doc_rect.width(), doc_rect.height())
                {
                    let target = Rect::from_min_size(doc_rect.min + Vec2::new(ox, oy), Vec2::new(w, h));
                    painter.image(
                        tex.id(),
                        target,
                        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                        Color32::from_white_alpha((bg.trace_opacity * 255.0).round() as u8),
                    );
                }
            }
        }
    }

    app.renderer.paint(
        &painter,
        &app.doc,
        &app.viewport as &dyn CellGrid,
        origin,
        cell,
        visible,
        pending,
        &hover_cells,
        caret_cell,
        selection,
    );

    if app.show_grid {
        paint_grid(&painter, &app.viewport, cell, origin, doc_rect, visible, doc_extent);
    }

    painter.rect_stroke(doc_rect, 0.0, Stroke::new(1.0, t.window_edge), StrokeKind::Outside);

    // The tool-icon cursor: replaces the OS cursor over the canvas for every stamp-shaped tool.
    // Text/Selection keep stock cursors (their gestures aren't stamp-shaped); space-pan gets the
    // grab hand. Must not paint while a modal is open — a painted cursor would advertise
    // interactivity the modal gate has already shut off.
    if !app.modal_open() && !pointer_on_resize_grip && response.contains_pointer() {
        let space_held = ui.input(|i| i.key_down(egui::Key::Space));
        if space_held || app.space_pan_active {
            ctx.set_cursor_icon(if app.space_pan_active {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        } else {
            match preview_kind {
                ToolKind::Text => ctx.set_cursor_icon(egui::CursorIcon::Text),
                ToolKind::Selection => ctx.set_cursor_icon(egui::CursorIcon::Crosshair),
                _ => {
                    ctx.set_cursor_icon(egui::CursorIcon::None);
                    if let Some(pos) = ctx.pointer_latest_pos() {
                        paint_tool_cursor(&painter, preview_kind, pos);
                    }
                }
            }
        }
    }
}

/// A cell-grid overlay: 1px lines on interior cell boundaries (the outer edge is already the doc
/// border), clipped to the document's own screen rect. 4% white over the document surface — faint
/// enough to read as structure, not as ink.
fn paint_grid(
    painter: &Painter,
    vp: &Viewport,
    cell: Vec2,
    origin: Pos2,
    doc_rect: Rect,
    visible: (u16, u16, u16, u16),
    extent: gascii_core::DocExtent,
) {
    let color = Color32::WHITE.gamma_multiply(0.04);
    let (x0, y0, x1, y1) = visible;
    for x in x0.max(1)..x1.min(extent.width) {
        let sx = vp.cell_to_screen(x, 0, cell, origin).x;
        painter.vline(sx, doc_rect.y_range(), Stroke::new(1.0, color));
    }
    for y in y0.max(1)..y1.min(extent.height) {
        let sy = vp.cell_to_screen(0, y, cell, origin).y;
        painter.hline(doc_rect.x_range(), sy, Stroke::new(1.0, color));
    }
}

/// Paints `kind`'s tool icon centered on `pos`: white over a 1px black hard-offset copy, legible
/// against both the black document surface and any light-themed desk around it.
fn paint_tool_cursor(painter: &Painter, kind: ToolKind, pos: Pos2) {
    const ICON_SIZE: f32 = 17.0;
    let rect = Rect::from_center_size(pos, Vec2::splat(ICON_SIZE));
    crate::ui::icons::paint(painter, kind, rect.translate(Vec2::splat(1.0)), Color32::BLACK);
    crate::ui::icons::paint(painter, kind, rect, Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::GasciiApp;

    fn headless_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        crate::fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |_ui| {});
        ctx
    }

    fn raw_input_with_screen(w: f32, h: f32, fullscreen: bool) -> egui::RawInput {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(w, h))),
            ..Default::default()
        };
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(fullscreen);
        raw
    }

    /// K2's write-back gate, driven through the real `show`: a re-fit must happen when the canvas
    /// area's own size actually changes, and must NOT happen on a steady-state frame at the same
    /// size — proven by forcing `zoom_step` away from the fit value between two same-size frames
    /// and confirming it survives untouched, then confirming a genuine resize DOES move it again.
    #[test]
    fn kiosk_auto_refit_only_recomputes_when_the_canvas_area_actually_changes_size() {
        let mut app = GasciiApp::headless();
        app.pending_fit = false; // isolate the auto-refit gate from the entry-transition snap

        let ctx = headless_ctx();
        let _ = ctx.run_ui(raw_input_with_screen(900.0, 700.0, true), |ui| show(ui, &mut app, false));
        let fit_size_1 = app.kiosk_last_fit_size.expect("fit must have run on the first fullscreen frame");

        // Nudge the zoom step away from whatever the fit picked — if auto-refit fired
        // unconditionally every frame, the next `show` call at the SAME size would silently
        // overwrite this back to the fit value.
        app.viewport.zoom_step = 0;
        let _ = ctx.run_ui(raw_input_with_screen(900.0, 700.0, true), |ui| show(ui, &mut app, false));
        assert_eq!(
            app.kiosk_last_fit_size,
            Some(fit_size_1),
            "an unchanged canvas area must not move the tracked fit size"
        );
        assert_eq!(
            app.viewport.zoom_step, 0,
            "no size change this frame: auto-refit must not have fired, so the forced override survives"
        );

        // A genuine resize DOES trigger a re-fit.
        let _ = ctx.run_ui(raw_input_with_screen(400.0, 300.0, true), |ui| show(ui, &mut app, false));
        let fit_size_2 = app.kiosk_last_fit_size.expect("fit must run again after a real resize");
        assert_ne!(fit_size_2, fit_size_1, "a genuine size change must update the tracked fit size");
    }

    /// Drives the actual pressure scan (`canvas.rs`'s own event loop, not a hand-rolled shortcut)
    /// through a synthetic `Event::Touch` for every quantization boundary the coder's formula
    /// (`1 + (force.clamp(0.0, 1.0) * 3.0).round()`) implies, including an out-of-range force to
    /// confirm the clamp. Neither the coder nor the code review had a test for this math at all.
    #[test]
    fn stylus_pressure_quantizes_force_into_a_1_to_4_stamp_size_and_marks_stylus_detected() {
        let cases: [(f32, u16); 6] = [
            (0.0, 1),
            (0.16, 1),
            (0.34, 2),
            (0.6, 3),
            (1.0, 4),
            (1.5, 4), // out-of-range: clamped to 1.0 before quantizing
        ];
        for (force, expected) in cases {
            let mut app = GasciiApp::headless();
            app.bind(Binding::L, ToolKind::Brush);
            app.brush_plugin_mut().set_pressure_enabled(true);
            begin_gesture(&mut app, Binding::L, 2, 2, false, false);

            let ctx = headless_ctx();
            let pos = Pos2::new(50.0, 50.0);
            let mut raw = raw_input_with_screen(900.0, 700.0, false);
            raw.events.push(egui::Event::PointerMoved(pos));
            raw.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            raw.events.push(egui::Event::Touch {
                device_id: egui::TouchDeviceId(0),
                id: egui::TouchId(0),
                phase: egui::TouchPhase::Move,
                pos,
                force: Some(force),
            });
            let _ = ctx.run_ui(raw, |ui| show(ui, &mut app, false));

            assert!(app.stylus_detected, "force={force}: any touch force must mark stylus_detected");
            assert_eq!(
                app.pressure_stamp_size,
                Some(expected),
                "force={force}: unexpected quantized stamp size"
            );
        }
    }

    /// A tool whose row is not `pressure_sizeable` (Pencil — no plugin owns it at all, let alone
    /// one that opts into a pressure override) must never get a pressure-driven size override, even
    /// while stylus force events fire mid-stroke — the gate must be a real capability check, not
    /// "any tool happens to be Brush by coincidence".
    #[test]
    fn a_non_pressure_sizeable_tool_never_gets_a_pressure_driven_size_override() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        begin_gesture(&mut app, Binding::L, 2, 2, false, false);

        let ctx = headless_ctx();
        let pos = Pos2::new(50.0, 50.0);
        let mut raw = raw_input_with_screen(900.0, 700.0, false);
        raw.events.push(egui::Event::PointerMoved(pos));
        raw.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        raw.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(0),
            phase: egui::TouchPhase::Move,
            pos,
            force: Some(1.0),
        });
        let _ = ctx.run_ui(raw, |ui| show(ui, &mut app, false));

        assert!(app.stylus_detected, "force events still mark stylus_detected regardless of tool");
        assert_eq!(app.pressure_stamp_size, None, "Pencil is not pressure_sizeable: no override may apply");
    }

    /// `REVIEW_plugin-api_2026-07-20.md` Suggestion 2, the one untested combination the review
    /// hand-traced but didn't lock with a test: a tool whose row IS `pressure_sizeable` (Brush)
    /// but whose live "Pressure" opt-in is still off (`BrushPlugin`'s own default,
    /// `brush_pressure: false` — never toggled on here) must get no size override, even while
    /// stylus force events fire mid-stroke. Distinct from
    /// `a_non_pressure_sizeable_tool_never_gets_a_pressure_driven_size_override`: that test covers
    /// the wrong-capability case (Pencil); this one covers the right-capability, opt-in-still-off
    /// case, which is the other half of `canvas.rs`'s two-part gate
    /// (`td.pressure_sizeable && td.plugin_slot.is_some_and(|i| app.plugins[i].
    /// pressure_override_enabled(td.name))`).
    #[test]
    fn a_pressure_sizeable_tool_with_the_pressure_toggle_off_gets_no_size_override() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Brush);
        assert!(!app.brush_plugin_mut().pressure_enabled(), "sanity: the Pressure opt-in starts off");
        begin_gesture(&mut app, Binding::L, 2, 2, false, false);

        let ctx = headless_ctx();
        let pos = Pos2::new(50.0, 50.0);
        let mut raw = raw_input_with_screen(900.0, 700.0, false);
        raw.events.push(egui::Event::PointerMoved(pos));
        raw.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        raw.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(0),
            phase: egui::TouchPhase::Move,
            pos,
            force: Some(1.0),
        });
        let _ = ctx.run_ui(raw, |ui| show(ui, &mut app, false));

        assert!(app.stylus_detected, "force events still mark stylus_detected regardless of the opt-in");
        assert_eq!(
            app.pressure_stamp_size, None,
            "Brush IS pressure_sizeable, but the live Pressure toggle is off: no override may apply"
        );
    }

    /// `build_renderer`'s fold over the real, per-app plugin list must be a true no-op for this
    /// phase: `BrushPlugin` never overrides `wrap_renderer` (it uses the trait's default identity),
    /// so painting through `app.renderer` (built from `app.plugins` in `with_state`/`headless`)
    /// must produce the exact same shapes as painting through a bare, freshly constructed
    /// `NaiveRenderer` on the same document. Proven on a seeded, non-trivial cell rather than an
    /// empty document, so the comparison actually exercises glyph/background painting, not just an
    /// empty-canvas coincidence.
    #[test]
    fn the_real_plugin_composed_renderer_paints_the_same_shapes_as_a_bare_naive_renderer() {
        let mut app = GasciiApp::headless();
        let seeded_bg = Rgba(10, 20, 30, 255);
        // Well inside the fit-computed visible range for a 300x300 screen against the default
        // 80x25 document (not (0,0)/(1,1) — the fit centers the doc and clips a couple of columns
        // off the left edge, which a smaller/edge-adjacent coordinate would silently fall outside).
        app.doc.set_cell(0, 10, 10, gascii_core::Cell { ch: 'X', fg: Rgba::WHITE, bg: seeded_bg });

        let ctx = headless_ctx();
        let via_plugins = ctx.run_ui(raw_input_with_screen(300.0, 300.0, false), |ui| show(ui, &mut app, false));
        let seeded_color = color32(seeded_bg);
        let via_plugins_bg_count = via_plugins
            .shapes
            .iter()
            .filter(|cs| matches!(&cs.shape, Shape::Rect(r) if r.fill == seeded_color))
            .count();
        assert_eq!(via_plugins_bg_count, 1, "sanity: the real per-app renderer painted the seeded cell's background exactly once");

        app.renderer = Box::new(NaiveRenderer);
        let bare = ctx.run_ui(raw_input_with_screen(300.0, 300.0, false), |ui| show(ui, &mut app, false));
        let bare_bg_count =
            bare.shapes.iter().filter(|cs| matches!(&cs.shape, Shape::Rect(r) if r.fill == seeded_color)).count();

        assert_eq!(
            via_plugins.shapes.len(),
            bare.shapes.len(),
            "the plugin-composed renderer must paint the exact same shape count as a bare NaiveRenderer \
             — BrushPlugin contributes no wrap_renderer override this phase"
        );
        assert_eq!(via_plugins_bg_count, bare_bg_count, "both must paint the seeded cell's background the same number of times");
    }

    /// The focus-loss cancel path (`canvas.rs`'s own focus-edge block) must clear the pressure
    /// override alongside the stroke it belongs to — otherwise a stale override could leak into
    /// whatever stroke happens next after focus returns.
    #[test]
    fn a_focus_loss_mid_pressure_modulated_stroke_clears_both_the_stroke_and_its_pressure_override() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Brush);
        app.brush_plugin_mut().set_pressure_enabled(true);
        begin_gesture(&mut app, Binding::L, 0, 0, false, false);
        app.pressure_stamp_size = Some(2); // as if a light-pressure dab already landed
        app.was_focused = true;

        let ctx = headless_ctx();
        let mut raw = raw_input_with_screen(900.0, 700.0, false);
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().focused = Some(false);
        let _ = ctx.run_ui(raw, |ui| show(ui, &mut app, false));

        assert_eq!(app.stroke_owner, None, "focus loss must cancel the in-progress stroke");
        assert_eq!(
            app.pressure_stamp_size, None,
            "the pressure override must not survive a focus-loss cancel"
        );
    }

    /// The trace-image overlay's `texture: None` guard (a headless image background — never
    /// uploaded, or a decode that hasn't reached the GPU yet) must be a pure no-op: `show` renders
    /// without panicking and leaves `image_bg` itself untouched by a no-input frame.
    #[test]
    fn a_trace_image_with_no_texture_renders_without_panicking_or_mutating_image_bg() {
        let mut app = GasciiApp::headless();
        app.image_bg = Some(crate::image_bg::ImageBackground::new(
            image::RgbaImage::new(4, 3),
            None,
            None,
        ));

        let ctx = headless_ctx();
        let _ = ctx.run_ui(raw_input_with_screen(900.0, 700.0, false), |ui| show(ui, &mut app, false));

        let bg = app.image_bg.as_ref().expect("a no-input render must not clear the loaded image");
        assert!(bg.texture.is_none(), "still no texture: the render must not have synthesized one");
        assert!((bg.trace_opacity - 0.5).abs() < f32::EPSILON, "a no-input render must not change opacity");
        assert!(bg.show_as_trace, "a no-input render must not change trace visibility");
    }

    /// Layering regression guard for the trace-invisible-under-an-opaque-background bug: the trace
    /// image must be painted ABOVE the document's full-`doc_rect` background fill, not underneath
    /// it, or an opaque background (every new document's default) hides it entirely. Confirmed
    /// structurally rather than pixel-by-pixel — a real GPU rasterizer isn't available headlessly —
    /// by capturing `show`'s returned `FullOutput` and asserting the trace's shape is submitted
    /// AFTER the background fill's shape: `show` paints everything through one `Painter` bound to a
    /// single layer, and within a layer, later-submitted shapes are drawn on top of earlier ones, so
    /// submission order here is a direct, load-bearing proxy for paint (and visibility) order.
    #[test]
    fn the_trace_image_paints_above_the_documents_opaque_background_fill_not_beneath_it() {
        let mut app = GasciiApp::headless();
        assert_eq!(
            app.doc.background,
            gascii_core::Rgba(0, 0, 0, 255),
            "must exercise the default opaque background — the case that was broken"
        );

        let ctx = headless_ctx();
        let pixels = image::RgbaImage::new(4, 3);
        let color_image = egui::ColorImage::from_rgba_unmultiplied([4, 3], pixels.as_raw());
        let texture = ctx.load_texture("trace_layering_test", color_image, egui::TextureOptions::LINEAR);
        let tex_id = texture.id();
        app.image_bg = Some(crate::image_bg::ImageBackground::new(pixels, Some(texture), None));
        assert!(app.image_bg.as_ref().unwrap().show_as_trace, "must exercise the visible-trace path");

        let output = ctx.run_ui(raw_input_with_screen(900.0, 700.0, false), |ui| show(ui, &mut app, false));

        let doc_bg = color32(app.doc.background);
        let bg_index = output
            .shapes
            .iter()
            .position(|cs| matches!(&cs.shape, Shape::Rect(r) if r.fill == doc_bg))
            .expect("the document's full-rect opaque background fill must be painted");
        let trace_index = output
            .shapes
            .iter()
            .position(|cs| matches!(&cs.shape, Shape::Mesh(m) if m.texture_id == tex_id))
            .expect("the trace image must be painted: a texture is loaded and show_as_trace is set");

        assert!(
            trace_index > bg_index,
            "the trace image (submitted at shape index {trace_index}) must come AFTER the opaque \
             background fill (index {bg_index}) so it paints on top instead of being hidden under it"
        );
    }

    /// The Alt-hold temporary eyedropper: Alt held at press time samples color exactly like a real
    /// Eyedropper press, regardless of the bound kind — and never touches the slot's own `kind`.
    #[test]
    fn alt_held_press_samples_color_regardless_of_the_bound_kind() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.doc.set_cell(app.active_layer, 2, 2, gascii_core::Cell {
            ch: 'x',
            fg: gascii_core::Rgba(10, 20, 30, 255),
            bg: gascii_core::Rgba(40, 50, 60, 255),
        });
        let (expected_fg, expected_bg) = gascii_core::eyedrop(&app.doc.cell(app.active_layer, 2, 2).copied().unwrap());

        let started = begin_gesture(&mut app, Binding::L, 2, 2, true, false);

        assert!(!started, "an Alt-sample press is a one-shot pick, not a gesture");
        assert_eq!(app.active_fg, expected_fg);
        assert_eq!(app.active_bg, expected_bg);
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "alt_sample must never rebind the slot's own kind");
        assert_eq!(app.stroke_owner, None, "a one-shot pick must not claim stroke ownership");
    }

    /// The `!stroke_in_progress` gate `begin_gesture`'s doc comment describes is inherited from
    /// `show()`'s own press-branch condition (`app.stroke_owner.is_none()`), not from `begin_gesture`
    /// itself — proven here by driving the real `show()` while a stroke on the OTHER binding is
    /// already active: an Alt-held primary press must not resample, must not disturb the in-progress
    /// stroke, and must not reassign stroke ownership.
    #[test]
    fn alt_hold_while_another_bindings_stroke_is_active_does_not_resample() {
        let mut app = GasciiApp::headless();
        app.pending_fit = false;
        app.bind(Binding::R, ToolKind::Pencil);
        begin_gesture(&mut app, Binding::R, 0, 0, false, false);
        assert_eq!(app.stroke_owner, Some(Binding::R), "sanity: R's stroke is active");
        let (fg_before, bg_before) = (app.active_fg, app.active_bg);

        let ctx = headless_ctx();
        let pos = Pos2::new(50.0, 50.0);
        let mut raw = raw_input_with_screen(900.0, 700.0, false);
        raw.modifiers = egui::Modifiers::ALT;
        raw.events.push(egui::Event::PointerMoved(pos));
        // R's own stroke must still read as "held" this frame — `secondary_down()` is a synthetic
        // `RawInput` reads fresh each `run_ui`, so without this event the frame would (incorrectly,
        // for this test's purposes) read R's button as already released regardless of Alt.
        raw.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::ALT,
        });
        raw.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::ALT,
        });
        let _ = ctx.run_ui(raw, |ui| show(ui, &mut app, false));

        assert_eq!(app.active_fg, fg_before, "an Alt press while another binding's stroke is active must not resample fg");
        assert_eq!(app.active_bg, bg_before, "an Alt press while another binding's stroke is active must not resample bg");
        assert_eq!(app.stroke_owner, Some(Binding::R), "R's in-progress stroke must be untouched");
    }

    const ALL_TOOL_KINDS: [ToolKind; 9] = [
        ToolKind::Pencil,
        ToolKind::Eraser,
        ToolKind::Eyedropper,
        ToolKind::Text,
        ToolKind::Fill,
        ToolKind::Rectangle,
        ToolKind::Line,
        ToolKind::Selection,
        ToolKind::Brush,
    ];

    /// The unconstrained path (`shift: false`, for every `ToolKind`, including `Line`/`Rectangle`
    /// themselves) must be a byte-for-byte passthrough of the input — Line/Rectangle drag behavior
    /// must stay byte-identical when Shift is not held.
    #[test]
    fn shift_constrain_with_shift_false_is_a_byte_for_byte_passthrough_for_every_tool_kind() {
        let anchor = (3, 4);
        let cases: [(u16, u16); 5] = [(3, 4), (10, 4), (3, 12), (10, 12), (0, 0)];
        for kind in ALL_TOOL_KINDS {
            for cur in cases {
                assert_eq!(
                    shift_constrain(kind, anchor, cur, false),
                    cur,
                    "{kind:?}: shift=false must return cur unchanged for cur={cur:?}"
                );
            }
        }
    }

    /// A non-Line/Rectangle kind must be a passthrough even with Shift held — only Line and
    /// Rectangle support the constraint at all.
    #[test]
    fn shift_constrain_is_a_passthrough_for_every_non_line_non_rectangle_kind_even_with_shift_held() {
        let anchor = (0, 0);
        let cur = (10, 3);
        for kind in ALL_TOOL_KINDS {
            if matches!(kind, ToolKind::Line | ToolKind::Rectangle) {
                continue;
            }
            assert_eq!(shift_constrain(kind, anchor, cur, true), cur, "{kind:?} must ignore shift entirely");
        }
    }

    #[test]
    fn shift_constrain_returns_cur_unchanged_when_cur_equals_anchor() {
        let anchor = (5, 5);
        assert_eq!(shift_constrain(ToolKind::Line, anchor, anchor, true), anchor);
        assert_eq!(shift_constrain(ToolKind::Rectangle, anchor, anchor, true), anchor);
    }

    #[test]
    fn shift_constrain_snaps_line_to_the_nearest_45_degree_ray() {
        let anchor = (10, 10);
        // (cursor delta from anchor, expected snapped delta)
        let cases: [((i32, i32), (i32, i32)); 6] = [
            ((10, 0), (10, 0)),    // already horizontal
            ((0, 10), (0, 10)),    // already vertical
            ((10, 10), (10, 10)),  // already diagonal (45°)
            ((10, 3), (10, 0)),    // ~16.7°, within 22.5° of horizontal
            ((3, 10), (0, 10)),    // ~16.7°, within 22.5° of vertical
            ((10, 8), (10, 10)),   // ~38.7°, closer to the 45° diagonal than either axis
        ];
        for ((dx, dy), (edx, edy)) in cases {
            let cur = ((anchor.0 as i32 + dx) as u16, (anchor.1 as i32 + dy) as u16);
            let expected = ((anchor.0 as i32 + edx) as u16, (anchor.1 as i32 + edy) as u16);
            assert_eq!(
                shift_constrain(ToolKind::Line, anchor, cur, true),
                expected,
                "delta ({dx},{dy}) from anchor must snap to delta ({edx},{edy})"
            );
        }
    }

    /// Direction (sign) must be preserved for every quadrant, not just the positive one the other
    /// tests exercise.
    #[test]
    fn shift_constrain_preserves_direction_for_negative_deltas() {
        let anchor = (20, 20);
        let cur = (10, 20); // dx = -10, dy = 0: already horizontal, leftward
        assert_eq!(shift_constrain(ToolKind::Line, anchor, cur, true), cur);

        let cur = (20, 10); // dx = 0, dy = -10: already vertical, upward
        assert_eq!(shift_constrain(ToolKind::Line, anchor, cur, true), cur);

        let cur = (10, 10); // dx = -10, dy = -10: already diagonal, up-left
        assert_eq!(shift_constrain(ToolKind::Line, anchor, cur, true), cur);
    }

    #[test]
    fn shift_constrain_clamps_rectangle_to_a_square_using_the_larger_axis() {
        let anchor = (5, 5);
        // dx=10, dy=3: the larger axis (x) wins, y stretches to match.
        assert_eq!(shift_constrain(ToolKind::Rectangle, anchor, (15, 8), true), (15, 15));
        // dx=3, dy=10: the larger axis (y) wins, x stretches to match.
        assert_eq!(shift_constrain(ToolKind::Rectangle, anchor, (8, 15), true), (15, 15));
        // Already square: unchanged.
        assert_eq!(shift_constrain(ToolKind::Rectangle, anchor, (12, 12), true), (12, 12));
        // Negative direction: signs preserved on both axes.
        assert_eq!(shift_constrain(ToolKind::Rectangle, anchor, (0, 2), true), (0, 0));
    }

    /// Shift toggled mid-drag (not held at press time) must re-snap on the very next frame, not
    /// only at press time — `shift_constrain` is called every `Drag` event via `drive_stroke_tail`,
    /// so this falls out of the design for free; still worth pinning directly.
    #[test]
    fn shift_toggled_mid_drag_changes_the_pending_preview_on_the_very_next_frame() {
        let mut app = GasciiApp::headless();
        app.pending_fit = false;
        app.bind(Binding::L, ToolKind::Line);
        begin_gesture(&mut app, Binding::L, 0, 0, false, false);
        assert_eq!(app.stroke_press_cell, Some((0, 0)));

        let ctx = headless_ctx();
        let pos = Pos2::new(50.0, 50.0);

        // Frame 1: drag to a point off any 45° ray, Shift NOT held — the raw, unconstrained cell.
        let mut raw1 = raw_input_with_screen(900.0, 700.0, false);
        raw1.events.push(egui::Event::PointerMoved(pos));
        raw1.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw1, |ui| show(ui, &mut app, false));
        let unconstrained_pending = app.slots[Binding::L.ix()].tool.pending().to_vec();

        // Frame 2: same pointer position, Shift now held — the preview must change (re-snap),
        // proving the constraint re-applies live rather than only at press time.
        let mut raw2 = raw_input_with_screen(900.0, 700.0, false);
        raw2.modifiers = egui::Modifiers::SHIFT;
        raw2.events.push(egui::Event::PointerMoved(pos));
        raw2.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::SHIFT,
        });
        let _ = ctx.run_ui(raw2, |ui| show(ui, &mut app, false));
        let constrained_pending = app.slots[Binding::L.ix()].tool.pending().to_vec();

        assert_ne!(
            unconstrained_pending, constrained_pending,
            "toggling Shift mid-drag at the same pointer position must change the pending preview"
        );
    }

    /// The shift-click-continue integration path, end to end through `begin_gesture`: a Shift-held
    /// press on a Line binding with a remembered `last_line_point` must continue the line from that
    /// point rather than starting a fresh one at the click.
    #[test]
    fn shift_held_press_on_line_with_a_remembered_point_continues_from_it() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Line);
        app.slots[Binding::L.ix()].last_line_point = Some((2, 2));

        let started = begin_gesture(&mut app, Binding::L, 8, 2, false, true);
        assert!(started, "a continued line is still a live gesture");
        assert_eq!(
            app.stroke_press_cell,
            Some((2, 2)),
            "the constraint anchor must be the remembered point, not the click"
        );

        let tctx = tool_ctx(&app, Binding::L);
        let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        let ToolResponse::Commit(Some(gascii_core::Edit::Cells(cells))) = resp else {
            panic!("expected a committed Edit::Cells spanning the continued line");
        };
        assert!(cells.iter().any(|c| c.x == 2 && c.y == 2), "the line must include its remembered start point");
        assert!(cells.iter().any(|c| c.x == 8 && c.y == 2), "the line must reach the click point");
    }

    /// Without Shift held, the same setup must behave exactly like today: a fresh line starting at
    /// the click, ignoring any remembered point.
    #[test]
    fn press_on_line_without_shift_starts_a_fresh_line_at_the_click_even_with_a_remembered_point() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Line);
        app.slots[Binding::L.ix()].last_line_point = Some((2, 2));

        begin_gesture(&mut app, Binding::L, 8, 2, false, false);
        assert_eq!(app.stroke_press_cell, Some((8, 2)), "the anchor must be the click, not the remembered point");
    }

    /// `set_tool`'s rebind-away-from-Line reset (`app.rs`): a rebind to any other kind must clear
    /// `last_line_point`, so a later rebind back to Line starts with no stale continuation point.
    #[test]
    fn rebinding_away_from_line_clears_the_remembered_last_line_point() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Line);
        app.slots[Binding::L.ix()].last_line_point = Some((2, 2));

        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::L, ToolKind::Line);

        assert_eq!(
            app.slots[Binding::L.ix()].last_line_point, None,
            "rebinding away from Line and back must not resurrect a stale continuation point"
        );
    }
}
