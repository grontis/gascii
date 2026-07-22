//! The windowed timeline panel: a bottom strip with playback/frame-op controls above a horizontal
//! thumbnail scroll. `body` is the shared capability set both this and `kiosk.rs` render — full
//! capability parity with the kiosk variant, not a reduced subset — `kiosk::show` delegates here at
//! touch geometry, mirroring `gascii::ui::kiosk::sidebar` delegating to
//! `sidebar::binding_options_geom`.
//!
//! Document-level `frame_duration_ms`/`loop_playback` (the animation's default fps/loop) are plain
//! `pub`, non-`Edit`-tracked fields on `Document` (the same "set-and-forget" precedent `background`
//! already follows) — unreachable through `PanelOutcome`'s edits-only mutation channel, so this
//! panel only ever *reads* them (for the tick's loop behavior and this display), never offers a
//! control to change them. The only mutable timing control exposed is the active frame's own
//! duration override, via `frame_ops::set_frame_duration` — an `Edit`, reachable normally.
//!
//! `frame_ops::*` failures from Add/Duplicate (hitting `MAX_FRAMES`, the cell budget) surface
//! through `PanelOutcome::error`, using `frame_op_error_message`'s per-variant wording — the host
//! writes it into `last_error`, the same channel every other structural action already uses. Delete
//! instead disables itself outright at its one cheap-to-prevent boundary (`frame_count() <= 1`), so
//! it never reaches `frame_ops` in a state that could fail.

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use gascii_core::{Document, Edit, FrameOpError};
use gascii_plugin_api::PanelOutcome;

use crate::shared::SharedState;
use crate::theme;
use crate::thumbnail::ThumbnailCache;
use crate::widgets;

pub(crate) const PANEL_H: f32 = 132.0;

/// Ceiling on the onion-skin prev/next steppers. `paint_onion` already stops as soon as it runs
/// past an actual neighbor frame, so the real per-paint cost is bounded by
/// `min(configured depth, frame_count() - 1)`, not the raw stepper value — this cap only removes the
/// rarely-reachable case of a document near `MAX_FRAMES` combined with a stepper clicked far beyond
/// any realistic depth (1-3), which would otherwise cost noticeably more per idle paint than intended.
const ONION_DEPTH_MAX: u8 = 8;

pub(crate) fn panel_frame(ctx: &egui::Context) -> egui::Frame {
    let t = theme::current(ctx);
    egui::Frame::new().fill(t.bg_panel).inner_margin(egui::Margin::symmetric(12, 8)).stroke(egui::Stroke::new(1.0, t.window_edge))
}

pub(crate) fn show(ui: &mut Ui, doc: &Document, state: &SharedState, thumbs: &mut ThumbnailCache) -> PanelOutcome {
    let mut outcome = PanelOutcome::default();
    egui::Panel::bottom("gascii_anim_timeline").frame(panel_frame(ui.ctx())).exact_size(PANEL_H).show(ui, |ui| {
        outcome = body(ui, doc, state, thumbs, Vec2::new(64.0, 40.0), 26.0);
    });
    outcome
}

fn duplicate_active(doc: &Document) -> Result<Edit, FrameOpError> {
    gascii_core::duplicate_frame(doc, doc.active_frame())
}

fn add_blank_after_active(doc: &Document) -> Result<Edit, FrameOpError> {
    gascii_core::add_frame(doc, doc.active_frame() + 1, gascii_core::Frame::blank(doc.width, doc.height))
}

/// Maps a `frame_ops` failure to a specific, readable message — mirrors `GasciiApp::
/// add_frame_via_menu`'s own per-variant convention exactly (never a raw `{e:?}` dump), so a
/// failure at the same boundary reads identically regardless of which control triggered it.
fn frame_op_error_message(action: &str, err: FrameOpError) -> String {
    match err {
        FrameOpError::TooManyFrames { max, .. } => format!("{action}: exceeds the {max} maximum"),
        FrameOpError::TotalCellBudgetExceeded { .. } => format!("{action}: exceeds the maximum total cell budget"),
        FrameOpError::TooManyLayers { max, .. } => format!("{action}: exceeds the {max} maximum layer count"),
        FrameOpError::IndexOutOfBounds { .. } | FrameOpError::LastFrame => format!("{action}: unexpected error"),
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

/// Steps the active frame's own duration override by `delta_ms`, floored at 10ms so a runaway
/// negative step can never reach (or go below) zero.
fn step_duration(doc: &Document, delta_ms: i32) -> Option<Edit> {
    let idx = doc.active_frame();
    let current = doc.resolved_frame_duration_ms(idx)?;
    let updated = (current as i32 + delta_ms).max(10) as u32;
    gascii_core::set_frame_duration(doc, idx, Some(updated)).ok().flatten()
}

/// The shared control-row + thumbnail-strip body both chrome variants render. `thumb_size` and
/// `control_h` are the only geometry deltas between windowed and kiosk.
pub(crate) fn body(ui: &mut Ui, doc: &Document, state: &SharedState, thumbs: &mut ThumbnailCache, thumb_size: Vec2, control_h: f32) -> PanelOutcome {
    let t = theme::current(ui.ctx());
    let mut outcome = PanelOutcome::default();

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            widgets::micro_label(ui, "TIMELINE");
            ui.add_space(6.0);

            let playing = state.borrow().playing;
            let play_label = if playing { "Pause" } else { "Play" };
            if widgets::button(ui, play_label, true, control_h).clicked() {
                let mut s = state.borrow_mut();
                s.playing = !s.playing;
                if s.playing {
                    s.playback_frame = doc.active_frame();
                    s.elapsed_ms = 0.0;
                }
            }

            // `Document.loop_playback` itself, not plugin-session state — a plain field write via
            // `PanelOutcome::set_loop_playback`, applied outside `History` (see that field's doc
            // comment), so the toggle survives exactly like every other document-level default.
            let mut loop_playback = doc.loop_playback;
            if widgets::checkbox(ui, &mut loop_playback, "Loop") {
                outcome.set_loop_playback = Some(loop_playback);
            }

            ui.label(
                egui::RichText::new(format!("{}/{}", doc.active_frame() + 1, doc.frame_count()))
                    .font(widgets::mono_id(widgets::size::LABEL))
                    .color(t.fg_text),
            );

            ui.add_space(10.0);
            if widgets::button(ui, "Add", true, control_h).clicked() {
                match add_blank_after_active(doc) {
                    Ok(edit) => outcome.edits.push(edit),
                    Err(e) => outcome.error = Some(frame_op_error_message("add frame", e)),
                }
            }
            if widgets::button(ui, "Duplicate", true, control_h).clicked() {
                match duplicate_active(doc) {
                    Ok(edit) => outcome.edits.push(edit),
                    Err(e) => outcome.error = Some(frame_op_error_message("duplicate frame", e)),
                }
            }
            let can_delete = doc.frame_count() > 1;
            if widgets::button(ui, "Delete", can_delete, control_h).clicked() && can_delete {
                if let Some(edit) = delete_active(doc) {
                    outcome.edits.push(edit);
                }
            }
            let can_left = doc.active_frame() > 0;
            if widgets::button(ui, "\u{25C0}", can_left, control_h).clicked() && can_left {
                if let Some(edit) = move_active_left(doc) {
                    outcome.edits.push(edit);
                }
            }
            let can_right = doc.active_frame() + 1 < doc.frame_count();
            if widgets::button(ui, "\u{25B6}", can_right, control_h).clicked() && can_right {
                if let Some(edit) = move_active_right(doc) {
                    outcome.edits.push(edit);
                }
            }

            ui.add_space(10.0);
            let dur = doc.resolved_frame_duration_ms(doc.active_frame()).unwrap_or(gascii_core::Document::DEFAULT_FRAME_DURATION_MS);
            if widgets::button(ui, "-10ms", true, control_h).clicked() {
                if let Some(edit) = step_duration(doc, -10) {
                    outcome.edits.push(edit);
                }
            }
            ui.label(egui::RichText::new(format!("{dur}ms")).font(widgets::mono_id(widgets::size::LABEL)).color(t.fg_secondary));
            if widgets::button(ui, "+10ms", true, control_h).clicked() {
                if let Some(edit) = step_duration(doc, 10) {
                    outcome.edits.push(edit);
                }
            }

            ui.add_space(10.0);
            let mut onion = state.borrow().onion_enabled;
            if widgets::checkbox(ui, &mut onion, "Onion") {
                state.borrow_mut().onion_enabled = onion;
            }
            if onion {
                let mut s = state.borrow_mut();
                if widgets::button(ui, "prev-", s.onion_prev > 0, control_h).clicked() && s.onion_prev > 0 {
                    s.onion_prev -= 1;
                }
                ui.label(format!("{}", s.onion_prev));
                let can_grow_prev = s.onion_prev < ONION_DEPTH_MAX;
                if widgets::button(ui, "prev+", can_grow_prev, control_h).clicked() && can_grow_prev {
                    s.onion_prev += 1;
                }
                if widgets::button(ui, "next-", s.onion_next > 0, control_h).clicked() && s.onion_next > 0 {
                    s.onion_next -= 1;
                }
                ui.label(format!("{}", s.onion_next));
                let can_grow_next = s.onion_next < ONION_DEPTH_MAX;
                if widgets::button(ui, "next+", can_grow_next, control_h).clicked() && can_grow_next {
                    s.onion_next += 1;
                }
            }
        });

        ui.add_space(6.0);
        egui::ScrollArea::horizontal().id_salt("gascii_anim_strip").auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for i in 0..doc.frame_count() {
                    let texture = thumbs.get_or_build(ui.ctx(), doc, i);
                    let (rect, resp) = ui.allocate_exact_size(thumb_size, Sense::click());
                    if let Some(tex) = texture {
                        ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
                    let active = i == doc.active_frame();
                    let (border, width) = if active { (t.border_strong, 2.0) } else { (t.border_soft, 1.0) };
                    ui.painter().rect_stroke(rect, 2.0, Stroke::new(width, border), StrokeKind::Inside);
                    if resp.clicked() && !active {
                        outcome.set_active_frame = Some(i);
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
        assert_eq!(err, FrameOpError::TooManyFrames { found: Document::MAX_FRAMES + 1, max: Document::MAX_FRAMES });
    }

    #[test]
    fn duplicate_active_surfaces_too_many_frames_at_the_max_frames_boundary() {
        let doc = doc_at_max_frames();
        let err = duplicate_active(&doc).unwrap_err();
        assert_eq!(err, FrameOpError::TooManyFrames { found: Document::MAX_FRAMES + 1, max: Document::MAX_FRAMES });
    }

    /// Pinned literally so a future wording change is a deliberate edit — must agree with
    /// `GasciiApp::add_frame_via_menu`'s own `last_error` text for the same `FrameOpError` variant
    /// (the two entry points hitting the same boundary must read identically to the user).
    #[test]
    fn frame_op_error_message_matches_add_frame_via_menus_own_wording_for_too_many_frames() {
        let err = FrameOpError::TooManyFrames { found: 257, max: 256 };
        assert_eq!(frame_op_error_message("add frame", err), "add frame: exceeds the 256 maximum");
    }

    #[test]
    fn frame_op_error_message_covers_every_variant_with_the_given_action_prefix() {
        assert_eq!(
            frame_op_error_message("add frame", FrameOpError::TotalCellBudgetExceeded { total_cells: 1, max: 2 }),
            "add frame: exceeds the maximum total cell budget"
        );
        assert_eq!(
            frame_op_error_message("add frame", FrameOpError::TooManyLayers { found: 3, max: 2 }),
            "add frame: exceeds the 2 maximum layer count"
        );
        assert_eq!(
            frame_op_error_message("add frame", FrameOpError::IndexOutOfBounds { index: 9, frame_count: 1 }),
            "add frame: unexpected error"
        );
        assert_eq!(frame_op_error_message("add frame", FrameOpError::LastFrame), "add frame: unexpected error");
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
        assert!(delete_active(&doc).is_none(), "a single-frame document must not produce a delete edit");
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
            outcome = Some(body(ui, &doc, &state, &mut thumbs, Vec2::new(48.0, 30.0), 24.0));
        });
        let outcome = outcome.unwrap();
        assert!(outcome.edits.is_empty());
        assert!(outcome.set_active_frame.is_none());
        assert!(outcome.set_loop_playback.is_none());
        assert!(outcome.error.is_none());
    }

    /// The Loop checkbox reads `Document.loop_playback` as its source of truth and, on a no-input
    /// render, must request no change — proves it never drifts from the document's own value.
    #[test]
    fn body_loop_checkbox_reflects_document_loop_playback_and_requests_nothing_on_a_no_input_render() {
        let mut doc = doc_with_frames(2);
        doc.loop_playback = false;
        let state = SharedState::new();
        let mut thumbs = ThumbnailCache::new();
        let ctx = egui::Context::default();
        let mut outcome = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            outcome = Some(body(ui, &doc, &state, &mut thumbs, Vec2::new(48.0, 30.0), 24.0));
        });
        assert!(outcome.unwrap().set_loop_playback.is_none(), "a no-input render must not request a loop change");
    }
}
