use egui::Ui;
use gascii_plugin_api::{CanvasRenderer, PanelOutcome, Plugin, PluginHost, PluginToolCapabilities};

use crate::decorator::OnionRenderer;
use crate::shared::SharedState;
use crate::thumbnail::ThumbnailCache;

pub struct AnimPlugin {
    state: SharedState,
    thumbnails: ThumbnailCache,
}

impl Default for AnimPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl AnimPlugin {
    pub fn new() -> Self {
        Self { state: SharedState::new(), thumbnails: ThumbnailCache::new() }
    }
}

impl Plugin for AnimPlugin {
    /// This plugin contributes no `Tool` — frame switching/playback are plugin-drawn UI and
    /// `tick`-driven input, never a canvas gesture.
    fn register_tools(&self) -> Vec<PluginToolCapabilities> {
        Vec::new()
    }

    /// A true no-op — claims zero screen space — while `frame_count() <= 1`, so a single-frame
    /// document's layout stays byte-identical to before this plugin existed. The *first* extra
    /// frame is bootstrapped by the host's own "Add Frame" menu entry, not by this panel (it has no
    /// toolbox/menu presence of its own to host that affordance at the one-frame boundary).
    fn panel(&mut self, ui: &mut Ui, kiosk: bool, host: &dyn PluginHost) -> PanelOutcome {
        let doc = host.document();
        if doc.frame_count() <= 1 {
            return PanelOutcome::default();
        }
        if kiosk {
            crate::kiosk::show(ui, doc, &self.state, &mut self.thumbnails)
        } else {
            crate::timeline::show(ui, doc, &self.state, &mut self.thumbnails)
        }
    }

    /// The playback clock — ignores `focused` (an animation preview must not freeze just because a
    /// text field somewhere has focus, unlike Brush's digit-key shortcut).
    fn tick(&mut self, ui: &mut Ui, _focused: bool, host: &dyn PluginHost) {
        let mut s = self.state.borrow_mut();
        if !s.playing {
            return;
        }
        let doc = host.document();
        if doc.frame_count() <= 1 {
            s.playing = false;
            return;
        }
        // Frame ops are host-applied, never by this plugin directly — `frame_count()` may have
        // shrunk since the last tick (e.g. a frame deleted while playing), so the cached playback
        // index can't be assumed valid.
        s.playback_frame = s.playback_frame.min(doc.frame_count() - 1);
        let dt_ms = ui.input(|i| i.stable_dt) * 1000.0;
        s.elapsed_ms += dt_ms;
        let dur = doc.resolved_frame_duration_ms(s.playback_frame).unwrap_or(gascii_core::Document::DEFAULT_FRAME_DURATION_MS) as f32;
        if s.elapsed_ms >= dur {
            s.elapsed_ms -= dur;
            let next = s.playback_frame + 1;
            if next >= doc.frame_count() {
                if doc.loop_playback {
                    s.playback_frame = 0;
                } else {
                    s.playing = false;
                }
            } else {
                s.playback_frame = next;
            }
        }
        // Recomputed from the (possibly just-advanced) `playback_frame` rather than reusing `dur`
        // above — the frame just entered can carry a different duration override than the one just
        // left, so scheduling off the pre-advance value would wake the UI too early or late whenever
        // per-frame durations differ.
        let next_dur = doc.resolved_frame_duration_ms(s.playback_frame).unwrap_or(gascii_core::Document::DEFAULT_FRAME_DURATION_MS) as f32;
        let remaining = (next_dur - s.elapsed_ms).max(1.0);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(remaining as u64));
    }

    fn wrap_renderer(&self, inner: Box<dyn CanvasRenderer>) -> Box<dyn CanvasRenderer> {
        Box::new(OnionRenderer::new(inner, self.state.clone()))
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::Document;

    struct FakeHost(Document);
    impl PluginHost for FakeHost {
        fn stylus_detected(&self) -> bool {
            false
        }
        fn is_bound(&self, _tool_name: &str) -> bool {
            false
        }
        fn document(&self) -> &Document {
            &self.0
        }
    }

    #[test]
    fn register_tools_is_empty() {
        assert!(AnimPlugin::new().register_tools().is_empty());
    }

    #[test]
    fn panel_returns_default_outcome_when_frame_count_is_one() {
        let mut p = AnimPlugin::new();
        let host = FakeHost(Document::default_document());
        let ctx = egui::Context::default();
        let mut outcome = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| outcome = Some(p.panel(ui, false, &host)));
        let outcome = outcome.unwrap();
        assert!(outcome.edits.is_empty());
        assert!(outcome.set_active_frame.is_none());
    }

    #[test]
    fn tick_is_a_no_op_when_not_playing() {
        let mut p = AnimPlugin::new();
        let host = FakeHost(Document::default_document());
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| p.tick(ui, false, &host));
        assert!(!p.state.borrow().playing);
        assert_eq!(p.state.borrow().playback_frame, 0);
    }

    /// Proves the focus-independence contract matters for this plugin specifically: unlike Brush's
    /// digit-key shortcut, a playback tick must keep advancing even while a widget has focus.
    #[test]
    fn tick_ignores_focused_unlike_brushs_digit_key_shortcut() {
        let mut doc = Document::default_document();
        let edit = gascii_core::add_frame(&doc, 1, gascii_core::Frame::blank(80, 25)).unwrap();
        let mut history = gascii_core::History::new();
        history.apply(&mut doc, edit);
        let mut p = AnimPlugin::new();
        p.state.borrow_mut().playing = true;
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let raw = egui::RawInput { predicted_dt: 1.0, ..Default::default() };
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, true, &host));
        // A whole second at 100ms/frame default duration must have advanced the playback frame at
        // least once, proving `focused: true` did not suppress the tick.
        assert!(p.state.borrow().elapsed_ms > 0.0 || p.state.borrow().playback_frame > 0);
    }

    fn doc_with_frames(n: usize) -> Document {
        let mut doc = Document::default_document();
        let mut history = gascii_core::History::new();
        for i in 1..n {
            let edit = gascii_core::add_frame(&doc, i, gascii_core::Frame::blank(doc.width, doc.height)).unwrap();
            history.apply(&mut doc, edit);
        }
        doc
    }

    /// A single `tick` call must advance `playback_frame` by exactly one step once its dt pushes
    /// `elapsed_ms` past the active frame's resolved duration (100ms default), carrying the
    /// remainder forward rather than resetting to zero — the core playback-advance property none of
    /// this crate's other tests exercise directly (they only prove focus-independence and the
    /// idle/no-op cases).
    #[test]
    fn tick_advances_playback_frame_by_exactly_one_step_once_the_resolved_duration_elapses() {
        let doc = doc_with_frames(3);
        let mut p = AnimPlugin::new();
        p.state.borrow_mut().playing = true;
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        // 150ms — safely past the 100ms default duration, with an unambiguous 50ms remainder.
        let raw = egui::RawInput { predicted_dt: 0.15, ..Default::default() };
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, false, &host));
        assert_eq!(p.state.borrow().playback_frame, 1, "150ms of dt at a 100ms duration must advance exactly one frame, not skip ahead within a single tick");
        assert!((p.state.borrow().elapsed_ms - 50.0).abs() < 1.0, "the 50ms remainder must carry over, not reset to zero");
    }

    #[test]
    fn tick_does_not_advance_before_the_full_duration_has_elapsed() {
        let doc = doc_with_frames(3);
        let mut p = AnimPlugin::new();
        p.state.borrow_mut().playing = true;
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let raw = egui::RawInput { predicted_dt: 0.05, ..Default::default() }; // 50ms < 100ms
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, false, &host));
        assert_eq!(p.state.borrow().playback_frame, 0);
        assert!((p.state.borrow().elapsed_ms - 50.0).abs() < 1.0);
    }

    #[test]
    fn tick_wraps_to_frame_zero_when_loop_playback_is_true_at_the_last_frame() {
        let doc = doc_with_frames(2);
        assert!(doc.loop_playback, "sanity: a fresh document defaults to looping");
        let mut p = AnimPlugin::new();
        p.state.borrow_mut().playing = true;
        p.state.borrow_mut().playback_frame = 1; // already on the last frame
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let raw = egui::RawInput { predicted_dt: 0.15, ..Default::default() };
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, false, &host));
        assert_eq!(p.state.borrow().playback_frame, 0, "looping playback must wrap back to frame 0 past the last frame");
        assert!(p.state.borrow().playing, "looping must not stop playback");
    }

    #[test]
    fn tick_stops_playing_at_the_last_frame_when_loop_playback_is_false() {
        let mut doc = doc_with_frames(2);
        doc.loop_playback = false;
        let mut p = AnimPlugin::new();
        p.state.borrow_mut().playing = true;
        p.state.borrow_mut().playback_frame = 1;
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let raw = egui::RawInput { predicted_dt: 0.15, ..Default::default() };
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, false, &host));
        assert!(!p.state.borrow().playing, "non-looping playback must stop at the last frame");
        assert_eq!(p.state.borrow().playback_frame, 1, "stopping must leave playback_frame on the last frame, not reset it");
    }

    /// The Edge Case this crate's own plan named explicitly ("frame ops are host-applied ... the
    /// cached playback index can't be assumed valid"): a stale `playback_frame` left over from a
    /// larger document (e.g. a frame deleted out from under a running playback) must clamp to the
    /// last valid index on the very next tick, before anything reads `resolved_frame_duration_ms`
    /// against the now-invalid index.
    #[test]
    fn tick_clamps_playback_frame_after_frame_count_shrinks_between_ticks() {
        let doc = doc_with_frames(3); // valid indices 0, 1, 2
        let mut p = AnimPlugin::new();
        p.state.borrow_mut().playing = true;
        p.state.borrow_mut().playback_frame = 7; // stale: valid when the document had 8+ frames
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        // Zero dt isolates the clamp from the separate advance-on-elapsed-duration logic.
        let raw = egui::RawInput { predicted_dt: 0.0, ..Default::default() };
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, false, &host));
        assert_eq!(p.state.borrow().playback_frame, 2, "a stale out-of-range playback_frame must clamp to frame_count() - 1");
    }
}
