//! The windowed timeline panel: a bottom strip with playback/frame-op controls above a horizontal
//! thumbnail scroll. `body` is the shared capability set both this and `kiosk.rs` render — full
//! capability parity with the kiosk variant, not a reduced subset — `kiosk::show` delegates here at
//! touch geometry, mirroring `gascii::ui::kiosk::sidebar` delegating to
//! `sidebar::binding_options_geom`.
//!
//! Document-level `frame_duration_ms`/`loop_playback` (the animation's default fps/loop) are plain
//! `pub`, non-`Edit`-tracked fields on `Document` (the same "set-and-forget" precedent `background`
//! already follows) — unreachable through `PanelOutcome`'s edits-only mutation channel. Both are
//! writable from this panel now: the Loop checkbox (via `DocProperty::LoopPlayback`) and the
//! header's Default duration stepper (via `DocProperty::DefaultFrameDuration`) — plain field
//! writes the host applies outside `History`, exactly like `DocProperty::ActiveFrame`. The active
//! frame's own duration override is the one timing control that *is* an `Edit`, via
//! `frame_ops::set_frame_duration` — reachable normally, with a clear-override control next to it
//! once a frame actually carries one.
//!
//! `frame_ops::*` failures from Add/Duplicate (hitting `MAX_FRAMES`, the cell budget) surface
//! through `PanelOutcome::error`, using `frame_op_error_message`'s per-variant wording — the host
//! writes it into `last_error`, the same channel every other structural action already uses. Delete
//! instead disables itself outright at its one cheap-to-prevent boundary (`frame_count() <= 1`), so
//! it never reaches `frame_ops` in a state that could fail.
//!
//! The thumbnail strip only calls `ThumbnailCache::get_or_build` for a frame whose allocated rect
//! actually intersects the scroll area's visible clip rect (`thumb_is_visible`) — every frame still
//! gets `ui.allocate_exact_size`'d at the identical size regardless, so layout, scroll extent, and
//! drag-reorder geometry are unaffected by which indices are currently offscreen.

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use gascii_core::{Document, Edit, FrameOpError};
use gascii_plugin_api::{DocProperty, PanelOutcome};

use crate::shared::SharedState;
use crate::theme;
use crate::thumbnail::ThumbnailCache;
use crate::widgets;

pub(crate) const PANEL_H: f32 = 164.0;

pub(crate) fn panel_frame(ctx: &egui::Context) -> egui::Frame {
    let t = theme::current(ctx);
    egui::Frame::new()
        .fill(t.bg_panel)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .stroke(egui::Stroke::new(1.0, t.window_edge))
}

/// The hidden-timeline affordance: a slim always-present bottom bar at the exact spot the panel
/// opens into, holding only the ▲ reopen button — so showing the timeline never requires the menu.
pub(crate) fn collapsed_bar(ui: &mut Ui, kiosk: bool, state: &SharedState) {
    let (h, control_h) = if kiosk { (64.0, 48.0) } else { (36.0, 20.0) };
    egui::Panel::bottom("gascii_anim_collapsed")
        .frame(panel_frame(ui.ctx()))
        .exact_size(h)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if widgets::button(ui, "\u{25B2} ANIMATION", true, control_h).clicked() {
                    state.borrow_mut().timeline_open = Some(true);
                }
            });
        });
}

pub(crate) fn show(
    ui: &mut Ui,
    doc: &Document,
    state: &SharedState,
    thumbs: &mut ThumbnailCache,
    top_edit_id: Option<u64>,
) -> PanelOutcome {
    let mut outcome = PanelOutcome::default();
    let resp = egui::Panel::bottom("gascii_anim_timeline")
        .frame(panel_frame(ui.ctx()))
        .exact_size(PANEL_H)
        .show(ui, |ui| {
            outcome = body(
                ui,
                doc,
                state,
                thumbs,
                Vec2::new(64.0, 40.0),
                26.0,
                top_edit_id,
            );
        });
    outcome.pressed_inside = pressed_inside(ui, resp.response.rect);
    outcome
}

/// Whether this frame's primary press landed inside `rect` — the `PanelOutcome::pressed_inside`
/// fact both chrome variants report so the host can track which section the mouse last touched.
pub(crate) fn pressed_inside(ui: &Ui, rect: egui::Rect) -> bool {
    ui.input(|i| {
        i.pointer.primary_pressed() && i.pointer.interact_pos().is_some_and(|p| rect.contains(p))
    })
}

/// Also called directly by `AnimPlugin::tick`'s `Shift+D` shortcut, so both entry points behave
/// identically at every boundary.
pub(crate) fn duplicate_active(doc: &Document) -> Result<Edit, FrameOpError> {
    gascii_core::duplicate_frame(doc, doc.active_frame())
}

fn add_blank_after_active(doc: &Document) -> Result<Edit, FrameOpError> {
    gascii_core::add_frame(
        doc,
        doc.active_frame() + 1,
        gascii_core::Frame::blank_with_layers(doc.width, doc.height, doc.layer_count()),
    )
}

/// Maps a `frame_ops` failure to a specific, readable message — mirrors `GasciiApp::
/// add_frame_via_menu`'s own per-variant convention exactly (never a raw `{e:?}` dump), so a
/// failure at the same boundary reads identically regardless of which control triggered it. Also
/// called directly by `AnimPlugin::tick`'s `Shift+D` shortcut.
pub(crate) fn frame_op_error_message(action: &str, err: FrameOpError) -> String {
    match err {
        FrameOpError::TooManyFrames { max, .. } => format!("{action}: exceeds the {max} maximum"),
        FrameOpError::TotalCellBudgetExceeded { .. } => {
            format!("{action}: exceeds the maximum total cell budget")
        }
        FrameOpError::TooManyLayers { max, .. } => {
            format!("{action}: exceeds the {max} maximum layer count")
        }
        FrameOpError::IndexOutOfBounds { .. } | FrameOpError::LastFrame => {
            format!("{action}: unexpected error")
        }
    }
}

fn delete_active(doc: &Document) -> Option<Edit> {
    gascii_core::remove_frame(doc, doc.active_frame()).ok()
}

fn move_active_left(doc: &Document) -> Option<Edit> {
    let a = doc.active_frame();
    if a == 0 {
        return None;
    }
    gascii_core::reorder_frame(doc, a, a - 1).ok().flatten()
}

fn move_active_right(doc: &Document) -> Option<Edit> {
    let a = doc.active_frame();
    if a + 1 >= doc.frame_count() {
        return None;
    }
    gascii_core::reorder_frame(doc, a, a + 1).ok().flatten()
}

/// Steps the active frame's own duration override by `delta_ms`, clamped to `[10,
/// Document::MAX_FRAME_DURATION_MS]`. The arithmetic runs in `i64` — `current` can be as large as
/// `MAX_FRAME_DURATION_MS` (a file-sourced value, already clamped at load but still far above
/// `i32`'s safe step-by-`delta_ms` range), and `i32` addition panics on overflow in a release build
/// (`overflow-checks` are on) rather than wrapping, so doing this step in `i32` risks a crash on a
/// document that came from a hostile or simply very old file.
fn step_duration(doc: &Document, delta_ms: i32) -> Option<Edit> {
    let idx = doc.active_frame();
    let current = doc.resolved_frame_duration_ms(idx)?;
    let updated = (current as i64 + delta_ms as i64)
        .clamp(10, gascii_core::Document::MAX_FRAME_DURATION_MS as i64) as u32;
    gascii_core::set_frame_duration(doc, idx, Some(updated))
        .ok()
        .flatten()
}

/// Clears the active frame's own duration override, falling back to the document default —
/// `None`-shaped exactly like `step_duration`/`frame_ops::set_frame_duration`'s own contract, so it
/// only produces an `Edit` when there was actually an override to clear.
fn clear_duration_override(doc: &Document) -> Option<Edit> {
    let idx = doc.active_frame();
    gascii_core::set_frame_duration(doc, idx, None)
        .ok()
        .flatten()
}

/// Steps the document-level default duration (`Document.frame_duration_ms`) by `delta_ms`, clamped
/// identically to `step_duration`. Not an `Edit` — reported through `DocProperty::
/// DefaultFrameDuration`, the same plain-field-write shape `DocProperty::LoopPlayback` already
/// uses.
fn step_default_duration(current: u32, delta_ms: i32) -> u32 {
    (current as i64 + delta_ms as i64)
        .clamp(10, gascii_core::Document::MAX_FRAME_DURATION_MS as i64) as u32
}

/// Parses a typed duration field, clamped to the same `[10, MAX_FRAME_DURATION_MS]` range the
/// steppers enforce. `None` for text that isn't a number at all — the field reverts to the live
/// value instead of committing.
fn parse_duration_ms(text: &str) -> Option<u32> {
    let n: i64 = text.trim().parse().ok()?;
    Some(n.clamp(10, gascii_core::Document::MAX_FRAME_DURATION_MS as i64) as u32)
}

/// A compact editable duration readout with an `ms` suffix: mirrors `live` while idle, holds the
/// in-progress text in `buffer` while focused, and returns a parsed, clamped value exactly once —
/// on the paint where focus leaves (Enter included, since Enter surrenders focus) — and only when
/// that value differs from `live`, so merely focusing and leaving the field never commits
/// anything. Escape discards the edit instead of committing it. Unparseable text also discards.
/// `!enabled` renders read-only (playback in progress).
fn duration_field(
    ui: &mut Ui,
    buffer: &mut Option<String>,
    live: u32,
    enabled: bool,
) -> Option<u32> {
    let t = theme::current(ui.ctx());
    let mut text = buffer.clone().unwrap_or_else(|| live.to_string());
    let resp = ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(&mut text)
            .desired_width(48.0)
            .font(widgets::mono_id(widgets::size::LABEL)),
    );
    ui.label(
        egui::RichText::new("ms")
            .font(widgets::mono_id(widgets::size::LABEL))
            .color(t.fg_secondary),
    );
    if resp.lost_focus() {
        let committed = buffer.take();
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            return None;
        }
        return committed
            .as_deref()
            .and_then(parse_duration_ms)
            .filter(|v| *v != live);
    }
    if resp.has_focus() {
        *buffer = Some(text);
    }
    None
}

/// Whether a thumb allocated at `[rect_min_x, rect_min_x + thumb_w)` intersects the visible clip
/// range `[clip_min_x, clip_max_x]` — the pure predicate `body`'s culling loop applies per frame,
/// kept separate from `Ui`/`egui::Rect` so it's testable headlessly.
fn thumb_is_visible(rect_min_x: f32, thumb_w: f32, clip_min_x: f32, clip_max_x: f32) -> bool {
    rect_min_x < clip_max_x && rect_min_x + thumb_w > clip_min_x
}

/// The frame the canvas is showing right now — the playback frame while playing (clamped, since
/// the document can shrink under a running clock), the editing cursor otherwise. This is what the
/// strip's selection marker and the frame counter track, so during playback they follow what's
/// actually on screen.
fn shown_frame(
    playing: bool,
    playback_frame: usize,
    active_frame: usize,
    frame_count: usize,
) -> usize {
    if playing {
        playback_frame.min(frame_count.saturating_sub(1))
    } else {
        active_frame
    }
}

#[derive(PartialEq, Eq, Debug)]
enum ThumbClick {
    /// While playing: retarget the playback clock, never the editing cursor — a cursor move would
    /// be invisible under the playback display.
    Scrub(usize),
    /// While idle: move the editing cursor (a `DocProperty` the host applies).
    Select(usize),
    Nothing,
}

fn resolve_thumb_click(playing: bool, clicked: usize, shown: usize) -> ThumbClick {
    if playing {
        ThumbClick::Scrub(clicked)
    } else if clicked != shown {
        ThumbClick::Select(clicked)
    } else {
        ThumbClick::Nothing
    }
}

/// Which inter-thumb gap (0..=count) the pointer is nearest — the drop position a drag-reorder
/// caret marks and a release commits to. Uniform allocation makes this pure arithmetic: gap `b`
/// sits at `row_min_x + b * stride`.
fn insertion_boundary(pointer_x: f32, row_min_x: f32, stride: f32, count: usize) -> usize {
    if stride <= 0.0 || count == 0 {
        return 0;
    }
    let rel = (pointer_x - row_min_x) / stride + 0.5;
    (rel.floor().max(0.0) as usize).min(count)
}

/// Maps an insertion gap to `reorder_frame`'s destination index (remove-then-insert semantics).
/// Dropping into either gap adjacent to the dragged thumb resolves to its own index — a no-op
/// `reorder_frame` collapses to `None`.
fn drop_target(boundary: usize, from: usize) -> usize {
    if boundary > from {
        boundary - 1
    } else {
        boundary
    }
}

/// The shared control-row + thumbnail-strip body both chrome variants render. `thumb_size` and
/// `control_h` are the only geometry deltas between windowed and kiosk. `top_edit_id` is threaded
/// straight through to `ThumbnailCache::get_or_build` — see `thumbnail.rs`'s module doc for why the
/// panel needs it.
pub(crate) fn body(
    ui: &mut Ui,
    doc: &Document,
    state: &SharedState,
    thumbs: &mut ThumbnailCache,
    thumb_size: Vec2,
    control_h: f32,
    top_edit_id: Option<u64>,
) -> PanelOutcome {
    let t = theme::current(ui.ctx());
    let mut outcome = PanelOutcome::default();

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // ▼ collapses to the slim bar `collapsed_bar` draws — the explicit override wins over
            // the multi-frame auto-show, and the bar is always there to reopen from.
            if widgets::button(ui, "\u{25BC}", true, control_h).clicked() {
                state.borrow_mut().timeline_open = Some(false);
            }
            widgets::micro_label(ui, "ANIMATION");
            ui.add_space(6.0);

            // The transport: step-back, Play, Pause, Stop, step-forward. Play and Pause are
            // separate, state-gated buttons rather than one relabeling toggle. The step buttons
            // move the editing cursor itself (like `,`/`.`), so they idle while playback owns the
            // shown frame.
            let playing = state.borrow().playing;
            let active = doc.active_frame();
            let can_step_back = !playing && active > 0;
            if widgets::button(ui, "\u{25C0}", can_step_back, control_h).clicked() && can_step_back
            {
                outcome
                    .properties
                    .push(DocProperty::ActiveFrame(active - 1));
            }
            if widgets::button(ui, "Play", !playing, control_h).clicked() && !playing {
                state.borrow_mut().start_playback(active);
            }
            // Pausing parks the editing cursor on the frame playback froze at — see
            // `Inner::pause_playback` for why the park matters.
            if widgets::button(ui, "Pause", playing, control_h).clicked() && playing {
                let frozen = state.borrow_mut().pause_playback();
                outcome.properties.push(DocProperty::ActiveFrame(frozen));
            }
            // Stop rewinds to the first frame, whether playing or already parked mid-sequence.
            let can_stop = playing || active != 0;
            if widgets::button(ui, "Stop", can_stop, control_h).clicked() && can_stop {
                let mut s = state.borrow_mut();
                s.playing = false;
                s.elapsed_ms = 0.0;
                outcome.properties.push(DocProperty::ActiveFrame(0));
            }
            let can_step_fwd = !playing && active + 1 < doc.frame_count();
            if widgets::button(ui, "\u{25B6}", can_step_fwd, control_h).clicked() && can_step_fwd {
                outcome
                    .properties
                    .push(DocProperty::ActiveFrame(active + 1));
            }

            // `Document.loop_playback` itself, not plugin-session state — a plain field write via
            // `DocProperty::LoopPlayback`, applied outside `History` (see that variant's doc
            // comment), so the toggle survives exactly like every other document-level default.
            let mut loop_playback = doc.loop_playback;
            if widgets::checkbox(ui, &mut loop_playback, "Loop") {
                outcome
                    .properties
                    .push(DocProperty::LoopPlayback(loop_playback));
            }

            // The counter tracks what's on screen: the playback frame while playing (with a ▶
            // marker so the number visibly means "now showing"), the editing cursor otherwise.
            let shown = shown_frame(
                playing,
                state.borrow().playback_frame,
                active,
                doc.frame_count(),
            );
            let counter = if playing {
                format!("\u{25B6} {}/{}", shown + 1, doc.frame_count())
            } else {
                format!("{}/{}", shown + 1, doc.frame_count())
            };
            ui.label(
                egui::RichText::new(counter)
                    .font(widgets::mono_id(widgets::size::LABEL))
                    .color(t.fg_text),
            );

            // Structural edits idle while playing — the canvas is showing playback, not the
            // editing cursor's frame, so nothing should mutate the document underneath it.
            ui.add_space(6.0);
            ui.separator();
            let can_add = !playing;
            if widgets::button(ui, "Add", can_add, control_h).clicked() && can_add {
                match add_blank_after_active(doc) {
                    Ok(edit) => outcome.edits.push(edit),
                    Err(e) => outcome.error = Some(frame_op_error_message("add frame", e)),
                }
            }
            if widgets::button(ui, "Duplicate", can_add, control_h).clicked() && can_add {
                match duplicate_active(doc) {
                    Ok(edit) => outcome.edits.push(edit),
                    Err(e) => outcome.error = Some(frame_op_error_message("duplicate frame", e)),
                }
            }
            let can_delete = !playing && doc.frame_count() > 1;
            if widgets::button(ui, "Delete", can_delete, control_h).clicked() && can_delete {
                if let Some(edit) = delete_active(doc) {
                    outcome.edits.push(edit);
                }
            }
            // "Move", not bare arrows — those belong to the transport's step buttons now.
            let can_left = !playing && doc.active_frame() > 0;
            if widgets::button(ui, "Move \u{25C0}", can_left, control_h).clicked() && can_left {
                if let Some(edit) = move_active_left(doc) {
                    outcome.edits.push(edit);
                }
            }
            let can_right = !playing && doc.active_frame() + 1 < doc.frame_count();
            if widgets::button(ui, "Move \u{25B6}", can_right, control_h).clicked() && can_right {
                if let Some(edit) = move_active_right(doc) {
                    outcome.edits.push(edit);
                }
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // Fresh read rather than row 1's captured value: a Play/Pause click above has already
            // flipped the state by the time this row paints.
            let playing = state.borrow().playing;

            // The two timing clusters: FRAME (the active frame's own duration, an override once
            // touched) and DEFAULT (the document-wide fallback) — each labeled and fenced off by a
            // separator so they read as distinct groups, not one run-on stepper row. Timing edits
            // idle while playing, like the structural row above.
            widgets::micro_label(ui, "FRAME");
            let dur = doc
                .resolved_frame_duration_ms(doc.active_frame())
                .unwrap_or(gascii_core::Document::DEFAULT_FRAME_DURATION_MS);
            if widgets::button(ui, "-10ms", !playing, control_h).clicked() && !playing {
                if let Some(edit) = step_duration(doc, -10) {
                    outcome.edits.push(edit);
                }
            }
            // A typed value always lands as this frame's own override, exactly like the steppers.
            // The `!= live` filter inside `duration_field` keeps a touch-and-leave from pinning an
            // override equal to the document default.
            if let Some(v) =
                duration_field(ui, &mut state.borrow_mut().duration_text, dur, !playing)
            {
                if let Ok(Some(edit)) =
                    gascii_core::set_frame_duration(doc, doc.active_frame(), Some(v))
                {
                    outcome.edits.push(edit);
                }
            }
            if widgets::button(ui, "+10ms", !playing, control_h).clicked() && !playing {
                if let Some(edit) = step_duration(doc, 10) {
                    outcome.edits.push(edit);
                }
            }
            // Shown only once the active frame actually carries an override — resets it back to
            // tracking the document default rather than leaving it permanently pinned once set.
            let has_override = doc
                .frame(doc.active_frame())
                .is_some_and(|f| f.duration_override.is_some());
            if has_override
                && widgets::button(ui, "Reset", !playing, control_h).clicked()
                && !playing
            {
                if let Some(edit) = clear_duration_override(doc) {
                    outcome.edits.push(edit);
                }
            }

            ui.add_space(6.0);
            ui.separator();
            widgets::micro_label(ui, "DEFAULT");
            if widgets::button(ui, "-10ms", !playing, control_h).clicked() && !playing {
                outcome
                    .properties
                    .push(DocProperty::DefaultFrameDuration(step_default_duration(
                        doc.frame_duration_ms,
                        -10,
                    )));
            }
            if let Some(v) = duration_field(
                ui,
                &mut state.borrow_mut().default_duration_text,
                doc.frame_duration_ms,
                !playing,
            ) {
                outcome
                    .properties
                    .push(DocProperty::DefaultFrameDuration(v));
            }
            if widgets::button(ui, "+10ms", !playing, control_h).clicked() && !playing {
                outcome
                    .properties
                    .push(DocProperty::DefaultFrameDuration(step_default_duration(
                        doc.frame_duration_ms,
                        10,
                    )));
            }
        });

        ui.add_space(6.0);
        egui::ScrollArea::horizontal()
            .id_salt("gascii_anim_strip")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let playing = state.borrow().playing;
                    let shown = shown_frame(
                        playing,
                        state.borrow().playback_frame,
                        doc.active_frame(),
                        doc.frame_count(),
                    );
                    // Drag-reorder feedback collected across the loop: the live drag (for the caret),
                    // a completed drop (for the edit), and the first thumb's rect (the row geometry
                    // every gap position derives from).
                    let mut live_drag: Option<f32> = None;
                    let mut drop: Option<(usize, f32)> = None;
                    let mut first_rect: Option<Rect> = None;
                    let stride = thumb_size.x + 6.0;
                    for i in 0..doc.frame_count() {
                        // Allocated at the identical size regardless of visibility or active state, so
                        // layout, scroll extent, and drag-reorder geometry never depend on which
                        // indices are currently culled or selected — only whether `get_or_build` (and
                        // the texture paint below it) actually runs does. The active thumb pops by
                        // *painting* into a slightly expanded rect instead; the 6px item spacing keeps
                        // that expansion from touching its neighbors.
                        let (rect, resp) =
                            ui.allocate_exact_size(thumb_size, Sense::click_and_drag());
                        if first_rect.is_none() {
                            first_rect = Some(rect);
                        }
                        // The marker tracks `shown`, not the editing cursor: during playback it rides
                        // the frame actually on screen.
                        let active = i == shown;
                        let draw = if active { rect.expand(2.0) } else { rect };
                        let clip = ui.clip_rect();
                        if thumb_is_visible(rect.min.x, thumb_size.x, clip.min.x, clip.max.x) {
                            let texture = thumbs.get_or_build(ui.ctx(), doc, i, top_edit_id);
                            if let Some(tex) = texture {
                                ui.painter().image(
                                    tex.id(),
                                    draw,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                        }
                        // `bg_inverse`, not `border_strong`: inversion is the chrome's selection
                        // signal (see `gascii::ui::theme`'s module doc), and `border_strong` is a
                        // low-contrast gray in the dark theme.
                        let (border, width) = if active {
                            (t.bg_inverse, 2.0)
                        } else {
                            (t.border_soft, 1.0)
                        };
                        ui.painter().rect_stroke(
                            draw,
                            2.0,
                            Stroke::new(width, border),
                            StrokeKind::Inside,
                        );
                        if playing && active {
                            // Keep the playing frame in view — the strip follows the playhead.
                            ui.scroll_to_rect(draw, Some(egui::Align::Center));
                        }
                        // Reorder is a structural edit — idle while playing, like the Move buttons.
                        if !playing {
                            if resp.dragged() {
                                if let Some(p) = resp.interact_pointer_pos() {
                                    live_drag = Some(p.x);
                                    // Dim the thumb being dragged so the caret reads as "where it
                                    // goes", not a second selection.
                                    ui.painter().rect_filled(draw, 2.0, t.bg_hover);
                                }
                            }
                            if resp.drag_stopped() {
                                if let Some(p) = resp
                                    .interact_pointer_pos()
                                    .or(ui.input(|inp| inp.pointer.latest_pos()))
                                {
                                    drop = Some((i, p.x));
                                }
                            }
                        }
                        if resp.clicked() {
                            match resolve_thumb_click(playing, i, shown) {
                                ThumbClick::Scrub(frame) => {
                                    let mut s = state.borrow_mut();
                                    s.playback_frame = frame;
                                    s.elapsed_ms = 0.0;
                                }
                                ThumbClick::Select(frame) => {
                                    outcome.properties.push(DocProperty::ActiveFrame(frame))
                                }
                                ThumbClick::Nothing => {}
                            }
                        }
                    }
                    // The insertion caret: a bar in the gap the drag would drop into.
                    if let (Some(px), Some(first)) = (live_drag, first_rect) {
                        let b = insertion_boundary(px, first.min.x, stride, doc.frame_count());
                        let x = first.min.x + b as f32 * stride - 3.0;
                        ui.painter().line_segment(
                            [
                                Pos2::new(x, first.min.y - 2.0),
                                Pos2::new(x, first.max.y + 2.0),
                            ],
                            Stroke::new(3.0, t.bg_inverse),
                        );
                    }
                    if let (Some((from, px)), Some(first)) = (drop, first_rect) {
                        let b = insertion_boundary(px, first.min.x, stride, doc.frame_count());
                        if let Ok(Some(edit)) =
                            gascii_core::reorder_frame(doc, from, drop_target(b, from))
                        {
                            outcome.edits.push(edit);
                        }
                    }
                });
            });
    });

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::{add_frame, Frame, History};

    fn doc_with_frames(n: usize) -> Document {
        let mut doc = Document::default_document();
        let mut history = History::new();
        for i in 1..n {
            let edit = add_frame(&doc, i, Frame::blank(doc.width, doc.height)).unwrap();
            history.apply(&mut doc, edit);
        }
        // `add_frame` selects each inserted frame — park the cursor back at 0 so tests state
        // their own starting frame explicitly.
        doc.set_active_frame(0);
        doc
    }

    /// A cheap (2x2) document pushed to exactly `Document::MAX_FRAMES` — dimensions don't matter for
    /// this boundary, mirrors `gascii-core`'s own `add_frame_over_max_frames_is_rejected` precedent.
    fn doc_at_max_frames() -> Document {
        let mut doc = Document::new(2, 2);
        let mut history = History::new();
        for i in 1..Document::MAX_FRAMES {
            let edit = add_frame(&doc, i, Frame::blank(2, 2)).unwrap();
            history.apply(&mut doc, edit);
        }
        doc
    }

    #[test]
    fn add_blank_after_active_surfaces_too_many_frames_at_the_max_frames_boundary() {
        let doc = doc_at_max_frames();
        assert_eq!(doc.frame_count(), Document::MAX_FRAMES);
        let err = add_blank_after_active(&doc).unwrap_err();
        assert_eq!(
            err,
            FrameOpError::TooManyFrames {
                found: Document::MAX_FRAMES + 1,
                max: Document::MAX_FRAMES
            }
        );
    }

    #[test]
    fn duplicate_active_surfaces_too_many_frames_at_the_max_frames_boundary() {
        let doc = doc_at_max_frames();
        let err = duplicate_active(&doc).unwrap_err();
        assert_eq!(
            err,
            FrameOpError::TooManyFrames {
                found: Document::MAX_FRAMES + 1,
                max: Document::MAX_FRAMES
            }
        );
    }

    #[test]
    fn add_blank_after_active_matches_the_documents_real_layer_count() {
        let mut doc = doc_with_frames(1);
        let mut history = History::new();
        for _ in 0..2 {
            let edit = gascii_core::add_layer(&doc, doc.layer_count()).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.layer_count(), 3);

        let edit = add_blank_after_active(&doc).unwrap();
        let Edit::AddFrame { frame, .. } = edit else {
            panic!("expected AddFrame")
        };
        assert_eq!(
            frame.layers.len(),
            doc.layer_count(),
            "a new animation frame must carry every existing layer, not just the first"
        );
    }

    /// Pinned literally so a future wording change is a deliberate edit — must agree with
    /// `GasciiApp::add_frame_via_menu`'s own `last_error` text for the same `FrameOpError` variant
    /// (the two entry points hitting the same boundary must read identically to the user).
    #[test]
    fn frame_op_error_message_matches_add_frame_via_menus_own_wording_for_too_many_frames() {
        let err = FrameOpError::TooManyFrames {
            found: 257,
            max: 256,
        };
        assert_eq!(
            frame_op_error_message("add frame", err),
            "add frame: exceeds the 256 maximum"
        );
    }

    #[test]
    fn frame_op_error_message_covers_every_variant_with_the_given_action_prefix() {
        assert_eq!(
            frame_op_error_message(
                "add frame",
                FrameOpError::TotalCellBudgetExceeded {
                    total_cells: 1,
                    max: 2
                }
            ),
            "add frame: exceeds the maximum total cell budget"
        );
        assert_eq!(
            frame_op_error_message(
                "add frame",
                FrameOpError::TooManyLayers { found: 3, max: 2 }
            ),
            "add frame: exceeds the 2 maximum layer count"
        );
        assert_eq!(
            frame_op_error_message(
                "add frame",
                FrameOpError::IndexOutOfBounds {
                    index: 9,
                    frame_count: 1
                }
            ),
            "add frame: unexpected error"
        );
        assert_eq!(
            frame_op_error_message("add frame", FrameOpError::LastFrame),
            "add frame: unexpected error"
        );
    }

    #[test]
    fn duplicate_active_inserts_immediately_after_the_active_frame() {
        let doc = doc_with_frames(2);
        let edit = duplicate_active(&doc).unwrap();
        match edit {
            Edit::AddFrame { index, .. } => assert_eq!(index, 1),
            other => panic!("expected AddFrame, got {other:?}"),
        }
    }

    #[test]
    fn delete_active_is_none_shaped_the_same_as_frame_ops_when_it_is_the_last_frame() {
        let doc = doc_with_frames(1);
        assert!(
            delete_active(&doc).is_none(),
            "a single-frame document must not produce a delete edit"
        );
    }

    #[test]
    fn move_active_left_is_none_at_index_zero_and_some_otherwise() {
        let mut doc = doc_with_frames(3);
        assert!(move_active_left(&doc).is_none());
        doc.set_active_frame(1);
        assert!(move_active_left(&doc).is_some());
    }

    #[test]
    fn move_active_right_is_none_at_the_last_index_and_some_otherwise() {
        let mut doc = doc_with_frames(3);
        assert!(move_active_right(&doc).is_some());
        doc.set_active_frame(2);
        assert!(move_active_right(&doc).is_none());
    }

    #[test]
    fn step_duration_floors_at_10ms_and_never_goes_negative() {
        let doc = doc_with_frames(1);
        let edit = step_duration(&doc, -1000).unwrap();
        match edit {
            Edit::SetFrameDuration { after, .. } => assert_eq!(after, Some(10)),
            other => panic!("expected SetFrameDuration, got {other:?}"),
        }
    }

    /// A `duration_override` sitting near `i32::MAX` (reachable only via a hostile/very old file —
    /// `load_v2` clamps this at load time, but this pins `step_duration`'s own arithmetic
    /// independent of that) must clamp at `Document::MAX_FRAME_DURATION_MS`, not panic on overflow
    /// or wrap negative and collapse to the 10ms floor.
    #[test]
    fn step_duration_clamps_at_the_max_instead_of_overflowing_near_i32_max() {
        let mut doc = doc_with_frames(1);
        let near_max = i32::MAX as u32 - 5;
        let dur_edit = gascii_core::set_frame_duration(&doc, 0, Some(near_max))
            .unwrap()
            .unwrap();
        let mut history = History::new();
        history.apply(&mut doc, dur_edit);

        let edit = step_duration(&doc, 1000).unwrap();
        match edit {
            Edit::SetFrameDuration { after, .. } => {
                assert_eq!(
                    after,
                    Some(gascii_core::Document::MAX_FRAME_DURATION_MS),
                    "must clamp at the max, not overflow or wrap"
                );
            }
            other => panic!("expected SetFrameDuration, got {other:?}"),
        }
    }

    #[test]
    fn body_renders_a_thumbnail_strip_click_as_a_set_active_frame_request() {
        let doc = doc_with_frames(3);
        let state = SharedState::new();
        let mut thumbs = ThumbnailCache::new();
        let ctx = egui::Context::default();
        // A no-input render must produce a default (no-op) outcome — proves rendering alone never
        // requests a mutation.
        let mut outcome = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            outcome = Some(body(
                ui,
                &doc,
                &state,
                &mut thumbs,
                Vec2::new(48.0, 30.0),
                24.0,
                Some(1),
            ));
        });
        let outcome = outcome.unwrap();
        assert!(outcome.edits.is_empty());
        assert!(outcome.properties.is_empty());
        assert!(outcome.error.is_none());
    }

    /// The active frame's thumb must pop: painted into an expanded rect with the 2px
    /// inversion-color border, while inactive thumbs keep the 1px soft outline at the allocated
    /// size. Pinned through the emitted shapes so a regression back to the old low-contrast
    /// `border_strong` marker fails.
    #[test]
    fn body_paints_the_active_thumb_enlarged_with_the_inversion_border() {
        let mut doc = doc_with_frames(3);
        doc.set_active_frame(1);
        let state = SharedState::new();
        let mut thumbs = ThumbnailCache::new();
        let ctx = egui::Context::default();
        let thumb = Vec2::new(48.0, 30.0);
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = body(ui, &doc, &state, &mut thumbs, thumb, 24.0, Some(1));
        });
        let t = theme::current(&ctx);

        let rects: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Rect(r) => Some(r),
                _ => None,
            })
            .collect();

        let active: Vec<_> = rects
            .iter()
            .filter(|r| r.stroke.width == 2.0 && r.stroke.color == t.bg_inverse)
            .collect();
        assert_eq!(
            active.len(),
            1,
            "exactly one thumb carries the active border"
        );
        let a = active[0].rect;
        assert!(
            a.width() > thumb.x && a.height() > thumb.y,
            "the active thumb paints larger than its allocation"
        );

        let inactive = rects
            .iter()
            .filter(|r| {
                r.stroke.width == 1.0
                    && r.stroke.color == t.border_soft
                    && (r.rect.size() - thumb).length() < 0.5
            })
            .count();
        assert_eq!(
            inactive, 2,
            "the two inactive thumbs keep the soft outline at the allocated size"
        );
    }

    #[test]
    fn shown_frame_tracks_playback_only_while_playing_and_clamps() {
        assert_eq!(shown_frame(false, 2, 0, 3), 0, "idle: the editing cursor");
        assert_eq!(shown_frame(true, 2, 0, 3), 2, "playing: the playback frame");
        assert_eq!(
            shown_frame(true, 9, 0, 3),
            2,
            "a stale playback index clamps to the last frame"
        );
    }

    /// `pressed_inside` is the host's section-tracking fact: a primary press inside the panel's
    /// own rect reports true, one up in canvas territory reports false.
    #[test]
    fn show_reports_a_press_inside_the_panel_and_not_one_outside() {
        let press_at = |pos: Pos2| {
            let doc = doc_with_frames(2);
            let state = SharedState::new();
            let mut thumbs = ThumbnailCache::new();
            let ctx = egui::Context::default();
            let mut raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                ..Default::default()
            };
            raw.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            let mut outcome = None;
            let _ = ctx.run_ui(raw, |ui| {
                outcome = Some(show(ui, &doc, &state, &mut thumbs, Some(1)))
            });
            outcome.unwrap().pressed_inside
        };

        assert!(
            press_at(Pos2::new(400.0, 590.0)),
            "a press inside the bottom panel must report pressed_inside"
        );
        assert!(
            !press_at(Pos2::new(400.0, 50.0)),
            "a press in canvas territory must not"
        );
    }

    #[test]
    fn insertion_boundary_maps_pointer_x_to_the_nearest_gap_and_clamps() {
        // 3 thumbs at a 54px stride, row starting at x=100.
        let (min_x, stride, n) = (100.0, 54.0, 3);
        assert_eq!(
            insertion_boundary(100.0, min_x, stride, n),
            0,
            "the row's left edge is the leading gap"
        );
        assert_eq!(
            insertion_boundary(60.0, min_x, stride, n),
            0,
            "left of the row clamps to 0"
        );
        assert_eq!(
            insertion_boundary(120.0, min_x, stride, n),
            0,
            "left half of thumb 0: the gap before it"
        );
        assert_eq!(
            insertion_boundary(140.0, min_x, stride, n),
            1,
            "right half of thumb 0: the gap after it"
        );
        assert_eq!(
            insertion_boundary(500.0, min_x, stride, n),
            3,
            "far right clamps to the trailing gap"
        );
    }

    #[test]
    fn drop_target_adjusts_for_removal_and_adjacent_gaps_are_no_ops() {
        assert_eq!(
            drop_target(0, 2),
            0,
            "dropping at the row start lands at index 0"
        );
        assert_eq!(
            drop_target(3, 0),
            2,
            "a gap right of the source shifts down by the removal"
        );
        assert_eq!(
            drop_target(2, 2),
            2,
            "the gap immediately before the source is a no-op"
        );
        assert_eq!(
            drop_target(3, 2),
            2,
            "the gap immediately after the source is a no-op too"
        );
    }

    #[test]
    fn resolve_thumb_click_scrubs_while_playing_and_selects_while_idle() {
        assert_eq!(resolve_thumb_click(true, 1, 0), ThumbClick::Scrub(1));
        assert_eq!(
            resolve_thumb_click(true, 0, 0),
            ThumbClick::Scrub(0),
            "re-clicking the shown frame restarts it from its start"
        );
        assert_eq!(resolve_thumb_click(false, 1, 0), ThumbClick::Select(1));
        assert_eq!(resolve_thumb_click(false, 0, 0), ThumbClick::Nothing);
    }

    /// While playing, the strip's enlarged inversion marker rides the playback frame, not the
    /// editing cursor — pinned through the emitted shapes exactly like the idle-marker test above.
    #[test]
    fn body_marks_the_playback_frame_while_playing_instead_of_the_editing_cursor() {
        let doc = doc_with_frames(3); // the editing cursor is parked at 0
        let state = SharedState::new();
        state.borrow_mut().playing = true;
        state.borrow_mut().playback_frame = 2;
        let mut thumbs = ThumbnailCache::new();
        let ctx = egui::Context::default();
        let thumb = Vec2::new(48.0, 30.0);
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = body(ui, &doc, &state, &mut thumbs, thumb, 24.0, Some(1));
        });
        let t = theme::current(&ctx);

        let markers: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Rect(r) if r.stroke.width == 2.0 && r.stroke.color == t.bg_inverse => {
                    Some(r.rect)
                }
                _ => None,
            })
            .collect();
        assert_eq!(markers.len(), 1, "exactly one thumb carries the marker");

        let idle: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Rect(r)
                    if r.stroke.width == 1.0
                        && r.stroke.color == t.border_soft
                        && (r.rect.size() - thumb).length() < 0.5 =>
                {
                    Some(r.rect)
                }
                _ => None,
            })
            .collect();
        assert_eq!(idle.len(), 2);
        assert!(
            idle.iter().all(|r| markers[0].center().x > r.center().x),
            "the marker must sit on the last (playback) thumb, right of both idle thumbs — not on cursor frame 0"
        );
    }

    /// The Loop checkbox reads `Document.loop_playback` as its source of truth and, on a no-input
    /// render, must request no change — proves it never drifts from the document's own value.
    #[test]
    fn body_loop_checkbox_reflects_document_loop_playback_and_requests_nothing_on_a_no_input_render(
    ) {
        let mut doc = doc_with_frames(2);
        doc.loop_playback = false;
        let state = SharedState::new();
        let mut thumbs = ThumbnailCache::new();
        let ctx = egui::Context::default();
        let mut outcome = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            outcome = Some(body(
                ui,
                &doc,
                &state,
                &mut thumbs,
                Vec2::new(48.0, 30.0),
                24.0,
                Some(1),
            ));
        });
        assert!(
            !outcome
                .unwrap()
                .properties
                .iter()
                .any(|p| matches!(p, DocProperty::LoopPlayback(_))),
            "a no-input render must not request a loop change"
        );
    }

    /// The Default duration stepper reads `Document.frame_duration_ms` as its display value and, on
    /// a no-input render, must request no change — proves it never drifts, mirroring the Loop
    /// checkbox's own no-input contract above.
    #[test]
    fn body_default_duration_stepper_requests_nothing_on_a_no_input_render() {
        let doc = doc_with_frames(2);
        let state = SharedState::new();
        let mut thumbs = ThumbnailCache::new();
        let ctx = egui::Context::default();
        let mut outcome = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            outcome = Some(body(
                ui,
                &doc,
                &state,
                &mut thumbs,
                Vec2::new(48.0, 30.0),
                24.0,
                Some(1),
            ));
        });
        assert!(!outcome
            .unwrap()
            .properties
            .iter()
            .any(|p| matches!(p, DocProperty::DefaultFrameDuration(_))));
    }

    #[test]
    fn clear_duration_override_produces_none_only_when_an_override_actually_exists() {
        let mut doc = doc_with_frames(1);
        assert!(
            clear_duration_override(&doc).is_none(),
            "no override set: nothing to clear"
        );

        let mut history = History::new();
        let set_edit = gascii_core::set_frame_duration(&doc, 0, Some(50))
            .unwrap()
            .unwrap();
        history.apply(&mut doc, set_edit);

        let edit = clear_duration_override(&doc).unwrap();
        match edit {
            Edit::SetFrameDuration { after, .. } => assert_eq!(after, None),
            other => panic!("expected SetFrameDuration, got {other:?}"),
        }
    }

    #[test]
    fn parse_duration_ms_accepts_numbers_and_clamps_to_the_stepper_range() {
        assert_eq!(parse_duration_ms("250"), Some(250));
        assert_eq!(
            parse_duration_ms(" 250 "),
            Some(250),
            "surrounding whitespace is tolerated"
        );
        assert_eq!(
            parse_duration_ms("5"),
            Some(10),
            "below the floor clamps up"
        );
        assert_eq!(
            parse_duration_ms("-40"),
            Some(10),
            "negative clamps to the floor"
        );
        assert_eq!(
            parse_duration_ms("999999999999"),
            Some(gascii_core::Document::MAX_FRAME_DURATION_MS)
        );
        assert_eq!(parse_duration_ms("abc"), None);
        assert_eq!(parse_duration_ms(""), None);
        assert_eq!(
            parse_duration_ms("12.5"),
            None,
            "fractional ms is rejected rather than rounded"
        );
    }

    #[test]
    fn step_default_duration_clamps_to_the_same_range_as_step_duration() {
        assert_eq!(
            step_default_duration(gascii_core::Document::DEFAULT_FRAME_DURATION_MS, -1000),
            10
        );
        assert_eq!(
            step_default_duration(gascii_core::Document::MAX_FRAME_DURATION_MS, 1000),
            gascii_core::Document::MAX_FRAME_DURATION_MS
        );
        assert_eq!(step_default_duration(100, 10), 110);
    }

    #[test]
    fn thumb_is_visible_true_when_the_thumb_overlaps_the_clip_range_at_all() {
        // Fully inside.
        assert!(thumb_is_visible(10.0, 48.0, 0.0, 100.0));
        // Straddling the left clip edge.
        assert!(thumb_is_visible(-20.0, 48.0, 0.0, 100.0));
        // Straddling the right clip edge.
        assert!(thumb_is_visible(90.0, 48.0, 0.0, 100.0));
    }

    /// End-to-end through `body`: a document with far more frames than fit in a constrained
    /// viewport must leave most of the strip's textures unbuilt — the direct H1 regression, proving
    /// offscreen frames never reach `ThumbnailCache::get_or_build` at all, not merely that the
    /// pure `thumb_is_visible` predicate is correct in isolation.
    #[test]
    fn body_never_builds_a_thumbnail_for_a_frame_scrolled_well_outside_a_constrained_viewport() {
        let doc = doc_with_frames(200);
        let state = SharedState::new();
        let mut thumbs = ThumbnailCache::new();
        let ctx = egui::Context::default();
        // A narrow viewport: at ~52px/thumb (48px + 4px spacing) a 300px-wide window fits well
        // under 200 of them.
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 400.0))),
            ..Default::default()
        };
        let _ = ctx.run_ui(raw, |ui| {
            let _ = body(
                ui,
                &doc,
                &state,
                &mut thumbs,
                Vec2::new(48.0, 30.0),
                24.0,
                Some(1),
            );
        });
        assert!(
            thumbs.built_count() < doc.frame_count(),
            "culling must leave the far-offscreen majority of frames unbuilt"
        );
    }

    #[test]
    fn thumb_is_visible_false_when_the_thumb_is_entirely_outside_the_clip_range() {
        // Entirely to the left.
        assert!(!thumb_is_visible(-100.0, 48.0, 0.0, 100.0));
        // Entirely to the right.
        assert!(!thumb_is_visible(150.0, 48.0, 0.0, 100.0));
        // Exactly touching the right edge (open interval: touching, not overlapping, is invisible).
        assert!(!thumb_is_visible(100.0, 48.0, 0.0, 100.0));
    }
}
