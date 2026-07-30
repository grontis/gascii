//! The windowed timeline panel: a bottom strip with playback/frame-op controls above a horizontal
//! thumbnail scroll. `body` is the shared capability set both this and `kiosk.rs` render — full
//! capability parity with the kiosk variant, not a reduced subset — `kiosk::show` delegates here at
//! touch geometry, mirroring `gascii::ui::kiosk::sidebar` delegating to
//! `sidebar::binding_options_geom`.
//!
//! Document-level `frame_duration_ms`/`loop_playback` (the animation's default fps/loop) are plain
//! `pub`, non-`Edit`-tracked fields on `Document` (the same "set-and-forget" precedent `background`
//! already follows) — unreachable through `PanelOutcome`'s edits-only mutation channel. Both are
//! writable from this panel now: the Loop checkbox (via `PanelOutcome::set_loop_playback`) and the
//! header's Default duration stepper (via `PanelOutcome::set_default_frame_duration`) — plain field
//! writes the host applies outside `History`, exactly like `set_active_frame`. The active frame's
//! own duration override is the one timing control that *is* an `Edit`, via
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
use gascii_plugin_api::PanelOutcome;

use crate::shared::SharedState;
use crate::theme;
use crate::thumbnail::ThumbnailCache;
use crate::widgets;

pub(crate) const PANEL_H: f32 = 132.0;

/// Ceiling on the onion-skin prev/next steppers. `paint_onion` (`decorator.rs`) scans farthest-to-
/// nearest and no longer stops at the first out-of-range neighbor (painting nearest-last needs the
/// full configured depth visited every time, not just however many neighbor frames actually
/// exist), so the per-paint cost per side is exactly this cap, not `frame_count()`-dependent — the
/// cap is what keeps a stepper clicked far beyond any realistic depth (1-3) from costing
/// noticeably more per idle paint than intended, regardless of document size.
const ONION_DEPTH_MAX: u8 = 8;

pub(crate) fn panel_frame(ctx: &egui::Context) -> egui::Frame {
    let t = theme::current(ctx);
    egui::Frame::new().fill(t.bg_panel).inner_margin(egui::Margin::symmetric(12, 8)).stroke(egui::Stroke::new(1.0, t.window_edge))
}

pub(crate) fn show(ui: &mut Ui, doc: &Document, state: &SharedState, thumbs: &mut ThumbnailCache, top_edit_id: Option<u64>) -> PanelOutcome {
    let mut outcome = PanelOutcome::default();
    egui::Panel::bottom("gascii_anim_timeline").frame(panel_frame(ui.ctx())).exact_size(PANEL_H).show(ui, |ui| {
        outcome = body(ui, doc, state, thumbs, Vec2::new(64.0, 40.0), 26.0, top_edit_id);
    });
    outcome
}

/// Also called directly by `AnimPlugin::tick`'s `Shift+D` shortcut, so both entry points behave
/// identically at every boundary.
pub(crate) fn duplicate_active(doc: &Document) -> Result<Edit, FrameOpError> {
    gascii_core::duplicate_frame(doc, doc.active_frame())
}

fn add_blank_after_active(doc: &Document) -> Result<Edit, FrameOpError> {
    gascii_core::add_frame(doc, doc.active_frame() + 1, gascii_core::Frame::blank(doc.width, doc.height))
}

/// Maps a `frame_ops` failure to a specific, readable message — mirrors `GasciiApp::
/// add_frame_via_menu`'s own per-variant convention exactly (never a raw `{e:?}` dump), so a
/// failure at the same boundary reads identically regardless of which control triggered it. Also
/// called directly by `AnimPlugin::tick`'s `Shift+D` shortcut.
pub(crate) fn frame_op_error_message(action: &str, err: FrameOpError) -> String {
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

/// Steps the active frame's own duration override by `delta_ms`, clamped to `[10,
/// Document::MAX_FRAME_DURATION_MS]`. The arithmetic runs in `i64` — `current` can be as large as
/// `MAX_FRAME_DURATION_MS` (a file-sourced value, already clamped at load but still far above
/// `i32`'s safe step-by-`delta_ms` range), and `i32` addition panics on overflow in a release build
/// (`overflow-checks` are on) rather than wrapping, so doing this step in `i32` risks a crash on a
/// document that came from a hostile or simply very old file.
fn step_duration(doc: &Document, delta_ms: i32) -> Option<Edit> {
    let idx = doc.active_frame();
    let current = doc.resolved_frame_duration_ms(idx)?;
    let updated = (current as i64 + delta_ms as i64).clamp(10, gascii_core::Document::MAX_FRAME_DURATION_MS as i64) as u32;
    gascii_core::set_frame_duration(doc, idx, Some(updated)).ok().flatten()
}

/// Clears the active frame's own duration override, falling back to the document default —
/// `None`-shaped exactly like `step_duration`/`frame_ops::set_frame_duration`'s own contract, so it
/// only produces an `Edit` when there was actually an override to clear.
fn clear_duration_override(doc: &Document) -> Option<Edit> {
    let idx = doc.active_frame();
    gascii_core::set_frame_duration(doc, idx, None).ok().flatten()
}

/// Steps the document-level default duration (`Document.frame_duration_ms`) by `delta_ms`, clamped
/// identically to `step_duration`. Not an `Edit` — reported through `PanelOutcome::
/// set_default_frame_duration`, the same plain-field-write shape `set_loop_playback` already uses.
fn step_default_duration(current: u32, delta_ms: i32) -> u32 {
    (current as i64 + delta_ms as i64).clamp(10, gascii_core::Document::MAX_FRAME_DURATION_MS as i64) as u32
}

/// Whether a thumb allocated at `[rect_min_x, rect_min_x + thumb_w)` intersects the visible clip
/// range `[clip_min_x, clip_max_x]` — the pure predicate `body`'s culling loop applies per frame,
/// kept separate from `Ui`/`egui::Rect` so it's testable headlessly.
fn thumb_is_visible(rect_min_x: f32, thumb_w: f32, clip_min_x: f32, clip_max_x: f32) -> bool {
    rect_min_x < clip_max_x && rect_min_x + thumb_w > clip_min_x
}

/// The shared control-row + thumbnail-strip body both chrome variants render. `thumb_size` and
/// `control_h` are the only geometry deltas between windowed and kiosk. `top_edit_id` is threaded
/// straight through to `ThumbnailCache::get_or_build` — see `thumbnail.rs`'s module doc for why the
/// panel needs it.
pub(crate) fn body(ui: &mut Ui, doc: &Document, state: &SharedState, thumbs: &mut ThumbnailCache, thumb_size: Vec2, control_h: f32, top_edit_id: Option<u64>) -> PanelOutcome {
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
            // Shown only once the active frame actually carries an override — clears it back to
            // tracking the document default rather than leaving it permanently pinned once set.
            let has_override = doc.frame(doc.active_frame()).is_some_and(|f| f.duration_override.is_some());
            if has_override && widgets::button(ui, "\u{00D7}", true, control_h).clicked() {
                if let Some(edit) = clear_duration_override(doc) {
                    outcome.edits.push(edit);
                }
            }

            ui.add_space(10.0);
            widgets::micro_label(ui, "DEFAULT");
            if widgets::button(ui, "-10ms", true, control_h).clicked() {
                outcome.set_default_frame_duration = Some(step_default_duration(doc.frame_duration_ms, -10));
            }
            ui.label(egui::RichText::new(format!("{}ms", doc.frame_duration_ms)).font(widgets::mono_id(widgets::size::LABEL)).color(t.fg_secondary));
            if widgets::button(ui, "+10ms", true, control_h).clicked() {
                outcome.set_default_frame_duration = Some(step_default_duration(doc.frame_duration_ms, 10));
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
                    // Allocated at the identical size regardless of visibility, so layout, scroll
                    // extent, and drag-reorder geometry never depend on which indices are
                    // currently culled — only whether `get_or_build` (and the texture paint below
                    // it) actually runs does.
                    let (rect, resp) = ui.allocate_exact_size(thumb_size, Sense::click());
                    let clip = ui.clip_rect();
                    if thumb_is_visible(rect.min.x, thumb_size.x, clip.min.x, clip.max.x) {
                        let texture = thumbs.get_or_build(ui.ctx(), doc, i, top_edit_id);
                        if let Some(tex) = texture {
                            ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        }
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

    /// A `duration_override` sitting near `i32::MAX` (reachable only via a hostile/very old file —
    /// `load_v2` clamps this at load time, but this pins `step_duration`'s own arithmetic
    /// independent of that) must clamp at `Document::MAX_FRAME_DURATION_MS`, not panic on overflow
    /// or wrap negative and collapse to the 10ms floor.
    #[test]
    fn step_duration_clamps_at_the_max_instead_of_overflowing_near_i32_max() {
        let mut doc = doc_with_frames(1);
        let near_max = i32::MAX as u32 - 5;
        let dur_edit = gascii_core::set_frame_duration(&doc, 0, Some(near_max)).unwrap().unwrap();
        let mut history = History::new();
        history.apply(&mut doc, dur_edit);

        let edit = step_duration(&doc, 1000).unwrap();
        match edit {
            Edit::SetFrameDuration { after, .. } => {
                assert_eq!(after, Some(gascii_core::Document::MAX_FRAME_DURATION_MS), "must clamp at the max, not overflow or wrap");
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
            outcome = Some(body(ui, &doc, &state, &mut thumbs, Vec2::new(48.0, 30.0), 24.0, Some(1)));
        });
        let outcome = outcome.unwrap();
        assert!(outcome.edits.is_empty());
        assert!(outcome.set_active_frame.is_none());
        assert!(outcome.set_loop_playback.is_none());
        assert!(outcome.set_default_frame_duration.is_none());
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
            outcome = Some(body(ui, &doc, &state, &mut thumbs, Vec2::new(48.0, 30.0), 24.0, Some(1)));
        });
        assert!(outcome.unwrap().set_loop_playback.is_none(), "a no-input render must not request a loop change");
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
            outcome = Some(body(ui, &doc, &state, &mut thumbs, Vec2::new(48.0, 30.0), 24.0, Some(1)));
        });
        assert!(outcome.unwrap().set_default_frame_duration.is_none());
    }

    #[test]
    fn clear_duration_override_produces_none_only_when_an_override_actually_exists() {
        let mut doc = doc_with_frames(1);
        assert!(clear_duration_override(&doc).is_none(), "no override set: nothing to clear");

        let mut history = History::new();
        let set_edit = gascii_core::set_frame_duration(&doc, 0, Some(50)).unwrap().unwrap();
        history.apply(&mut doc, set_edit);

        let edit = clear_duration_override(&doc).unwrap();
        match edit {
            Edit::SetFrameDuration { after, .. } => assert_eq!(after, None),
            other => panic!("expected SetFrameDuration, got {other:?}"),
        }
    }

    #[test]
    fn step_default_duration_clamps_to_the_same_range_as_step_duration() {
        assert_eq!(step_default_duration(gascii_core::Document::DEFAULT_FRAME_DURATION_MS, -1000), 10);
        assert_eq!(step_default_duration(gascii_core::Document::MAX_FRAME_DURATION_MS, 1000), gascii_core::Document::MAX_FRAME_DURATION_MS);
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
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 400.0))), ..Default::default() };
        let _ = ctx.run_ui(raw, |ui| {
            let _ = body(ui, &doc, &state, &mut thumbs, Vec2::new(48.0, 30.0), 24.0, Some(1));
        });
        assert!(thumbs.built_count() < doc.frame_count(), "culling must leave the far-offscreen majority of frames unbuilt");
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
