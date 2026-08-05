use egui::Ui;
use gascii_plugin_api::{CanvasRenderer, DocProperty, PanelOutcome, Plugin, PluginDescriptor, PluginHost, PluginShortcut};

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

    /// Whether the timeline panel currently shows for a document with `frame_count` frames: the
    /// user's explicit ▲/▼ choice when they've made one, otherwise auto — shown once a second
    /// frame exists.
    pub fn timeline_visible(&self, frame_count: usize) -> bool {
        self.state.borrow().timeline_open.unwrap_or(frame_count > 1)
    }

    /// The explicit override half of `timeline_visible` — what the panel's ▼ hide button and the
    /// collapsed bar's ▲ reopen button write. `pub` as a test seam (host tests reach it via
    /// `as_any_mut`).
    pub fn set_timeline_open(&mut self, open: bool) {
        self.state.borrow_mut().timeline_open = Some(open);
    }
}

/// Constructs the one real, per-app `AnimPlugin` instance — the `PluginDescriptor.make` fn
/// pointer. A named fn, not a closure, so `DESCRIPTOR` stays a plain `const`.
pub fn make() -> Box<dyn Plugin> {
    Box::new(AnimPlugin::new())
}

/// This crate's whole registration story, harvested by the host's `const PLUGINS` table without
/// ever constructing a throwaway instance.
pub const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: "gascii-anim",
    name: "Animation",
    description: "Frame-based animation: a timeline panel, playback, and an onion-skin canvas overlay.",
    version: env!("CARGO_PKG_VERSION"),
    make,
    tools: AnimPlugin::tool_capabilities,
    shortcuts: AnimPlugin::shortcuts,
};

/// The next `(space_hold_active, space_hold_saw_primary_press)` state, plus whether this frame
/// should toggle playback — `AnimPlugin::tick`'s pure decision core, unit-testable without a live
/// `Ui`/`PluginHost`.
///
/// `Space` tap-on-release: a hold that never saw a primary press toggles play/pause the instant
/// `Space` is released; a hold that DID see one is a space-pan drag (or an attempt at one) and
/// never toggles. `saw_primary_press` latches for the whole hold, not just the frame it happened
/// on, so a pan that starts mid-hold still suppresses the toggle at release.
///
/// Known, accepted fidelity gap: canvas's real pan-start condition additionally requires the
/// primary press to land off the window's ~5px resize grip (`!pointer_on_resize_grip`). This
/// function has no way to know that — a primary press exactly on the resize grip while `Space` is
/// held is indistinguishable, from here, from a real pan attempt, so the toggle is suppressed even
/// though canvas itself never actually panned. Rare (requires clicking exactly on the grip while
/// holding Space) and corrupts no state, just occasionally swallows one play/pause tap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SpaceHoldOutcome {
    pub active: bool,
    pub saw_primary_press: bool,
    pub toggle_playback: bool,
}

pub(crate) fn resolve_space_hold(
    active: bool,
    saw_primary_press: bool,
    space_down: bool,
    primary_pressed_this_frame: bool,
) -> SpaceHoldOutcome {
    if !active {
        return if space_down {
            // A primary press landing on the very same frame Space is first pressed already reads
            // as pan-starting, not a play/pause tap.
            SpaceHoldOutcome { active: true, saw_primary_press: primary_pressed_this_frame, toggle_playback: false }
        } else {
            SpaceHoldOutcome { active: false, saw_primary_press: false, toggle_playback: false }
        };
    }
    let saw = saw_primary_press || primary_pressed_this_frame;
    if space_down {
        SpaceHoldOutcome { active: true, saw_primary_press: saw, toggle_playback: false }
    } else {
        // Release edge: toggle iff no primary press occurred anywhere during the hold.
        SpaceHoldOutcome { active: false, saw_primary_press: false, toggle_playback: !saw }
    }
}

// This plugin contributes no `Tool` — frame switching/playback are plugin-drawn UI and
// `tick`-driven input, never a canvas gesture — so `tool_capabilities` is left at its default
// (`Vec::new()`), with no override here.
impl Plugin for AnimPlugin {
    /// The five `tick`-driven shortcuts below, in the same order `tick` checks them. `Space` is
    /// declared as a hold, matching its `key_down`-driven mechanism rather than a one-shot press.
    fn shortcuts() -> Vec<PluginShortcut> {
        vec![
            PluginShortcut { name: "Play / Pause", label: "Space (hold)", keys: &[egui::Key::Space] },
            PluginShortcut { name: "Toggle Onion Skin", label: "O", keys: &[egui::Key::O] },
            PluginShortcut { name: "Previous Frame", label: ",", keys: &[egui::Key::Comma] },
            PluginShortcut { name: "Next Frame", label: ".", keys: &[egui::Key::Period] },
            PluginShortcut { name: "Duplicate Frame", label: "Shift+D", keys: &[egui::Key::D] },
        ]
    }

    /// While hidden (`timeline_visible` — auto-hidden for a fresh single-frame document, or
    /// explicitly collapsed via the panel's ▼ button), only a slim bottom bar with the ▲ reopen
    /// button is drawn. That bar is the timeline's whole discoverability story: it's always at the
    /// spot where the panel appears, so opening never requires the menu. Once open, the panel's
    /// own Add/Duplicate buttons bootstrap the first extra frame in plain sight; the host's
    /// "Add Frame" menu entry remains as the second path.
    fn panel(&mut self, ui: &mut Ui, kiosk: bool, host: &dyn PluginHost) -> PanelOutcome {
        let doc = host.document();
        if !self.timeline_visible(doc.frame_count()) {
            crate::timeline::collapsed_bar(ui, kiosk, &self.state);
            return PanelOutcome::default();
        }
        let top_edit_id = host.top_edit_id();
        if kiosk {
            crate::kiosk::show(ui, doc, &self.state, &mut self.thumbnails, top_edit_id)
        } else {
            crate::timeline::show(ui, doc, &self.state, &mut self.thumbnails, top_edit_id)
        }
    }

    /// `Space` play/pause, `O` onion-toggle, `,`/`.` frame navigation, and `Shift+D` duplicate frame
    /// are all gated on `!focused` — matching `BrushPlugin::tick`'s own digit-key gating precedent,
    /// so typing into a focused field never fires any of them. The playback clock below them
    /// ignores `focused` entirely (an animation preview must not freeze just because a text field
    /// somewhere has focus). `,`/`.`/`Shift+D` are the two shortcuts that need to reach the
    /// document — everything else here only ever mutates this plugin's own `SharedState`.
    fn tick(&mut self, ui: &mut Ui, focused: bool, resumed_after_suppression: bool, host: &dyn PluginHost) -> PanelOutcome {
        let mut outcome = PanelOutcome::default();
        // OS-level window-focus loss (`i.viewport().focused`) is a different axis from the
        // `focused` parameter above (widget focus / session suppression) — checked unconditionally,
        // not gated on it. egui-winit's own `Focused` handler never clears `keys_down`, so a `Space`
        // physically released while alt-tabbed away leaves `key_down(Space)` reading `true` on
        // return with nothing to end the hold; left alone, the next real Space tap would see a
        // release against a hold state that never actually happened and fire a stale toggle. Mirrors
        // the same falling-edge reset the host's own `was_focused` field already needs in
        // `canvas.rs`, tracked here instead since this plugin has no access to that copy.
        let os_focused = ui.input(|i| i.viewport().focused).unwrap_or(true);
        {
            let mut s = self.state.borrow_mut();
            if (s.was_focused && !os_focused) || resumed_after_suppression {
                // A modal dialog opened and closed spanning a Space hold is the same shape of
                // problem as an OS focus loss mid-hold (see the paragraph above): whatever edge
                // crossed while `tick` wasn't being called at all is unobservable here, so the hold
                // must be reset rather than resumed — otherwise a stale `space_hold_active` reads
                // the modal's own close as "Space was just released" and fires a spurious toggle.
                s.space_hold_active = false;
                s.space_hold_saw_primary_press = false;
            }
            s.was_focused = os_focused;
        }
        if !focused {
            let (space_down, primary_pressed) =
                ui.input(|i| (i.key_down(egui::Key::Space), i.pointer.primary_pressed()));
            let (active, saw) = {
                let s = self.state.borrow();
                (s.space_hold_active, s.space_hold_saw_primary_press)
            };
            let hold = resolve_space_hold(active, saw, space_down, primary_pressed);
            {
                let mut s = self.state.borrow_mut();
                s.space_hold_active = hold.active;
                s.space_hold_saw_primary_press = hold.saw_primary_press;
                if hold.toggle_playback {
                    // Mirrors `timeline.rs`'s own Play/Pause buttons exactly: play starts the
                    // clock at the editing cursor; pause parks the cursor on the frozen frame
                    // (see `Inner::pause_playback`).
                    if s.playing {
                        let frozen = s.pause_playback();
                        outcome.properties.push(DocProperty::ActiveFrame(frozen));
                    } else {
                        s.start_playback(host.document().active_frame());
                    }
                }
            }
            let onion_toggle = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::O));
            if onion_toggle {
                self.state.borrow_mut().onion_enabled ^= true;
            }

            // `,`/`.` prev/next frame — arrow keys are deliberately avoided (an active Text session
            // owns those). Clamped against `frame_count()`, which can shrink between ticks exactly
            // like the playback clock's own `s.playback_frame = s.playback_frame.min(...)` below.
            // Idle while playing, matching the transport's own disabled step buttons.
            let playing_now = self.state.borrow().playing;
            let (prev, next) =
                ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::Comma), i.consume_key(egui::Modifiers::NONE, egui::Key::Period)));
            let doc = host.document();
            if prev && !playing_now {
                if let Some(idx) = doc.active_frame().checked_sub(1) {
                    outcome.properties.push(DocProperty::ActiveFrame(idx));
                }
            } else if next && !playing_now {
                let active = doc.active_frame();
                if active + 1 < doc.frame_count() {
                    outcome.properties.push(DocProperty::ActiveFrame(active + 1));
                }
            }

            // Shift+D duplicate frame — the exact same pure helper `timeline.rs`'s own Duplicate
            // button uses, so both entry points behave identically at every boundary (MAX_FRAMES,
            // the cell budget). Idle while playing, like every other structural control.
            let duplicate = ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::D));
            if duplicate && !playing_now {
                match crate::timeline::duplicate_active(doc) {
                    Ok(edit) => outcome.edits.push(edit),
                    Err(e) => outcome.error = Some(crate::timeline::frame_op_error_message("duplicate frame", e)),
                }
            }
        }

        let mut s = self.state.borrow_mut();
        if !s.playing {
            return outcome;
        }
        let doc = host.document();
        if doc.frame_count() <= 1 {
            s.playing = false;
            return outcome;
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
        outcome
    }

    fn wrap_renderer(&self, inner: Box<dyn CanvasRenderer>) -> Box<dyn CanvasRenderer> {
        Box::new(OnionRenderer::new(inner, self.state.clone()))
    }

    /// While playing, the canvas shows `playback_frame`, not the editing cursor's frame — an edit
    /// landed then would silently target a frame the user can't see.
    fn blocks_editing(&self) -> bool {
        self.state.borrow().playing
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
        fn top_edit_id(&self) -> Option<u64> {
            None
        }
    }

    /// Extracts the single `DocProperty::ActiveFrame` a test outcome carries, if any — the read
    /// side of the migration from `PanelOutcome::set_active_frame`.
    fn active_frame_of(outcome: &PanelOutcome) -> Option<usize> {
        outcome.properties.iter().find_map(|p| match p {
            DocProperty::ActiveFrame(i) => Some(*i),
            _ => None,
        })
    }

    #[test]
    fn tool_capabilities_is_empty() {
        assert!(AnimPlugin::tool_capabilities().is_empty());
    }

    /// The visibility rule: auto (frame-count-driven) until the user's explicit ▲/▼ choice, which
    /// then wins in both directions — including hiding a multi-frame document's panel.
    #[test]
    fn timeline_visible_is_auto_by_frame_count_until_an_explicit_override() {
        let mut p = AnimPlugin::new();
        assert!(!p.timeline_visible(1), "auto: a fresh single-frame document shows no panel");
        assert!(p.timeline_visible(2), "auto: a multi-frame document shows the panel");
        p.set_timeline_open(true);
        assert!(p.timeline_visible(1), "explicit open wins at one frame");
        p.set_timeline_open(false);
        assert!(!p.timeline_visible(1));
        assert!(!p.timeline_visible(2), "explicit hide wins over the multi-frame auto-show");
    }

    /// The hidden state is not a no-op anymore: it draws the slim ▲ reopen bar (the timeline's
    /// discoverability affordance), and opening draws strictly more — the full panel.
    #[test]
    fn hidden_panel_draws_the_collapsed_bar_and_opening_draws_the_full_panel() {
        let mut p = AnimPlugin::new();
        let host = FakeHost(Document::default_document());
        let ctx = egui::Context::default();
        let collapsed = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = p.panel(ui, false, &host);
        });
        assert!(!collapsed.shapes.is_empty(), "the hidden state must still draw the reopen bar");

        p.set_timeline_open(true);
        let ctx2 = egui::Context::default();
        let open = ctx2.run_ui(egui::RawInput::default(), |ui| {
            let _ = p.panel(ui, false, &host);
        });
        assert!(open.shapes.len() > collapsed.shapes.len(), "the opened timeline must draw strictly more than the bar");
    }

    /// `blocks_editing` follows `playing` exactly — the signal the host's edit-initiation gates
    /// poll.
    #[test]
    fn blocks_editing_exactly_while_playing() {
        let p = AnimPlugin::new();
        assert!(!p.blocks_editing(), "idle must not block");
        p.state.borrow_mut().playing = true;
        assert!(p.blocks_editing(), "playing must block");
        p.state.borrow_mut().playing = false;
        assert!(!p.blocks_editing(), "pausing must unblock immediately");
    }

    /// `shortcuts()` must declare exactly the five keys `tick`'s own dispatch checks, in the same
    /// order — the pairing that keeps the declaration from going stale.
    #[test]
    fn shortcuts_declares_every_key_tick_actually_dispatches() {
        let rows = AnimPlugin::shortcuts();
        let keys: Vec<egui::Key> = rows.iter().flat_map(|r| r.keys.iter().copied()).collect();
        assert_eq!(keys, vec![egui::Key::Space, egui::Key::O, egui::Key::Comma, egui::Key::Period, egui::Key::D]);
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
        assert!(active_frame_of(&outcome).is_none());
    }

    #[test]
    fn tick_is_a_no_op_when_not_playing() {
        let mut p = AnimPlugin::new();
        let host = FakeHost(Document::default_document());
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| { p.tick(ui, false, false, &host); });
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
        let _ = ctx.run_ui(raw, |ui| { p.tick(ui, true, false, &host); });
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
        // `add_frame` selects each inserted frame — park the cursor back at 0 so tests state
        // their own starting frame explicitly.
        doc.set_active_frame(0);
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
        let _ = ctx.run_ui(raw, |ui| { p.tick(ui, false, false, &host); });
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
        let _ = ctx.run_ui(raw, |ui| { p.tick(ui, false, false, &host); });
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
        let _ = ctx.run_ui(raw, |ui| { p.tick(ui, false, false, &host); });
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
        let _ = ctx.run_ui(raw, |ui| { p.tick(ui, false, false, &host); });
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
        let _ = ctx.run_ui(raw, |ui| { p.tick(ui, false, false, &host); });
        assert_eq!(p.state.borrow().playback_frame, 2, "a stale out-of-range playback_frame must clamp to frame_count() - 1");
    }

    /// A plain tap — Space pressed then released with no primary press anywhere in between — must
    /// toggle playback on release.
    #[test]
    fn resolve_space_hold_tap_with_no_primary_press_toggles_on_release() {
        let after_press = resolve_space_hold(false, false, true, false);
        assert_eq!(after_press, SpaceHoldOutcome { active: true, saw_primary_press: false, toggle_playback: false });

        let after_release = resolve_space_hold(after_press.active, after_press.saw_primary_press, false, false);
        assert_eq!(
            after_release,
            SpaceHoldOutcome { active: false, saw_primary_press: false, toggle_playback: true },
            "releasing Space with no primary press seen during the hold must toggle playback"
        );
    }

    /// A hold that sees a primary press partway through (a space-pan drag) must NOT toggle on
    /// release, even though the primary press itself happened on an earlier frame than the release.
    #[test]
    fn resolve_space_hold_hold_then_primary_press_then_release_does_not_toggle() {
        let after_press = resolve_space_hold(false, false, true, false);
        let mid_hold = resolve_space_hold(after_press.active, after_press.saw_primary_press, true, true);
        assert!(mid_hold.saw_primary_press, "the primary press this frame must latch");

        let after_release = resolve_space_hold(mid_hold.active, mid_hold.saw_primary_press, false, false);
        assert_eq!(
            after_release,
            SpaceHoldOutcome { active: false, saw_primary_press: false, toggle_playback: false },
            "a primary press anywhere during the hold must suppress the release-time toggle"
        );
    }

    /// A primary press that happened before Space was ever pressed is irrelevant — it must not
    /// suppress a later, independent hold's own toggle.
    #[test]
    fn resolve_space_hold_a_primary_press_before_space_is_pressed_does_not_suppress_a_later_hold() {
        // Not active, Space not down: the primary press here has nowhere to latch onto.
        let before = resolve_space_hold(false, false, false, true);
        assert_eq!(before, SpaceHoldOutcome { active: false, saw_primary_press: false, toggle_playback: false });

        // A fresh hold starts clean, unaffected by the earlier isolated press.
        let after_press = resolve_space_hold(before.active, before.saw_primary_press, true, false);
        let after_release = resolve_space_hold(after_press.active, after_press.saw_primary_press, false, false);
        assert!(after_release.toggle_playback, "the later hold must toggle — the earlier press must not have suppressed it");
    }

    /// Documents the known resize-grip approximation: a primary press that lands on the window's
    /// resize grip while Space is held does not start a real pan in `canvas.rs` (it's gated on
    /// `!pointer_on_resize_grip` there), but `resolve_space_hold` has no way to know that — it sees
    /// only "a primary press happened," and suppresses the toggle exactly as if a real pan had
    /// started. This test pins that known, accepted behavior rather than "fixing" it.
    #[test]
    fn resolve_space_hold_cannot_distinguish_a_resize_grip_click_from_a_real_pan_attempt() {
        let after_press = resolve_space_hold(false, false, true, false);
        // Simulates a primary press that (in the real app) landed on the resize grip — canvas.rs
        // itself would never have started a pan for it, but this function still sees a press.
        let grip_press = resolve_space_hold(after_press.active, after_press.saw_primary_press, true, true);
        let after_release = resolve_space_hold(grip_press.active, grip_press.saw_primary_press, false, false);
        assert!(
            !after_release.toggle_playback,
            "a known, accepted limitation: a resize-grip press is indistinguishable from a real pan \
             attempt here, so it also suppresses the toggle"
        );
    }

    /// `egui::Context::run_ui`'s own `InputState.keys_down` persists across calls on the SAME
    /// `Context` (each call feeds new `RawInput.events` into the previous frame's carried-over
    /// state) — so "Space held" is simulated by pushing exactly one `pressed: true` event on the
    /// down frame and letting it stay down until a matching `pressed: false` event releases it.
    fn space_key_event(pressed: bool) -> egui::Event {
        egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    fn pointer_button_event(pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// End-to-end through the real `tick`: a Space tap (down on frame 1, up on frame 2, no primary
    /// press in between) with no other input must toggle `playing`.
    #[test]
    fn tick_toggles_playing_on_a_space_tap_with_no_primary_press() {
        let doc = doc_with_frames(2);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();

        let mut raw_down = egui::RawInput::default();
        raw_down.events.push(space_key_event(true));
        let _ = ctx.run_ui(raw_down, |ui| { p.tick(ui, false, false, &host); });
        assert!(!p.state.borrow().playing, "playback must not toggle while Space is still held");

        let mut raw_up = egui::RawInput::default();
        raw_up.events.push(space_key_event(false));
        let _ = ctx.run_ui(raw_up, |ui| { p.tick(ui, false, false, &host); });
        assert!(p.state.borrow().playing, "releasing a plain Space tap must toggle playback on");
    }

    /// The pause half of the Space tap: tapping while playing must freeze playback AND hand the
    /// frozen playback frame to the host as the new editing cursor — identical to the Pause
    /// button, so both pause entry points land the user on the frame they were looking at.
    #[test]
    fn a_space_tap_while_playing_pauses_and_parks_the_cursor_on_the_frozen_frame() {
        let doc = doc_with_frames(3);
        let mut p = AnimPlugin::new();
        p.state.borrow_mut().playing = true;
        p.state.borrow_mut().playback_frame = 2;
        let host = FakeHost(doc);
        let ctx = egui::Context::default();

        let mut raw_down = egui::RawInput::default();
        raw_down.events.push(space_key_event(true));
        let _ = ctx.run_ui(raw_down, |ui| { p.tick(ui, false, false, &host); });

        let mut raw_up = egui::RawInput::default();
        raw_up.events.push(space_key_event(false));
        let mut outcome = None;
        let _ = ctx.run_ui(raw_up, |ui| outcome = Some(p.tick(ui, false, false, &host)));

        assert!(!p.state.borrow().playing, "the tap must pause");
        assert_eq!(active_frame_of(&outcome.unwrap()), Some(2), "pausing must park the cursor on the frozen frame");
    }

    /// The pan-aware half, end to end: a primary press during the Space hold must suppress the
    /// toggle on release.
    #[test]
    fn tick_does_not_toggle_playing_when_a_primary_press_occurred_during_the_space_hold() {
        let doc = doc_with_frames(2);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();

        let mut raw_down = egui::RawInput::default();
        raw_down.events.push(space_key_event(true));
        let _ = ctx.run_ui(raw_down, |ui| { p.tick(ui, false, false, &host); });

        let mut raw_press = egui::RawInput::default();
        raw_press.events.push(pointer_button_event(true));
        let _ = ctx.run_ui(raw_press, |ui| { p.tick(ui, false, false, &host); });

        let mut raw_up = egui::RawInput::default();
        raw_up.events.push(space_key_event(false));
        let _ = ctx.run_ui(raw_up, |ui| { p.tick(ui, false, false, &host); });
        assert!(!p.state.borrow().playing, "a primary press mid-hold must suppress the release-time toggle");
    }

    /// The `!focused` gate: Space play/pause must not fire while a widget has focus (or an active
    /// session suppresses shortcuts) — matching `BrushPlugin::tick`'s own digit-key gate precedent.
    #[test]
    fn tick_does_not_toggle_playing_on_a_space_tap_while_focused() {
        let doc = doc_with_frames(2);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();

        let mut raw_down = egui::RawInput::default();
        raw_down.events.push(space_key_event(true));
        let _ = ctx.run_ui(raw_down, |ui| { p.tick(ui, true, false, &host); });
        let mut raw_up = egui::RawInput::default();
        raw_up.events.push(space_key_event(false));
        let _ = ctx.run_ui(raw_up, |ui| { p.tick(ui, true, false, &host); });

        assert!(!p.state.borrow().playing, "Space must be suppressed while focused");
    }

    /// OS-level window-focus loss mid-hold (e.g. alt-tabbing away while still physically holding
    /// Space) must reset the hold — otherwise the stale `space_hold_active`/
    /// `space_hold_saw_primary_press` survive the interruption untouched, and the next tick that
    /// observes `Space` no longer down reads that as "the original hold just ended", firing a
    /// toggle for a hold the user never actually completed.
    ///
    /// The real integration (confirmed against the vendored `egui-winit-0.35.0` `WindowEvent::
    /// Focused` handler) delivers a focus transition as both `ViewportInfo.focused` flipping AND an
    /// `egui::Event::WindowFocused(false)` in the same frame's events — the latter is how egui's own
    /// `InputState` clears its *own* `keys_down` set on focus loss, so `Space` reads not-down again
    /// immediately. This plugin's own `space_hold_active`/`space_hold_saw_primary_press` are
    /// separate state `egui` has no way to know about or clear — this test simulates that same
    /// real event shape and pins that this plugin's own state is reset in step with it.
    #[test]
    fn a_window_focus_loss_mid_space_hold_resets_it_so_it_does_not_fire_a_spurious_toggle() {
        let doc = doc_with_frames(2);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();

        // Frame 1: Space pressed, window focused — a hold begins.
        let mut raw_down = egui::RawInput::default();
        raw_down.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().focused = Some(true);
        raw_down.events.push(space_key_event(true));
        let _ = ctx.run_ui(raw_down, |ui| { p.tick(ui, false, false, &host); });
        assert!(p.state.borrow().space_hold_active, "sanity: the hold is active");

        // Frame 2: the window loses OS focus, delivered the same way the real integration does.
        let mut raw_unfocus = egui::RawInput::default();
        raw_unfocus.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().focused = Some(false);
        raw_unfocus.events.push(egui::Event::WindowFocused(false));
        let _ = ctx.run_ui(raw_unfocus, |ui| { p.tick(ui, false, false, &host); });

        assert!(!p.state.borrow().space_hold_active, "the focus-loss edge must reset the hold");
        assert!(!p.state.borrow().playing, "the reset itself must never toggle playback");
    }

    /// The host-latch counterpart to the focus-loss test above: a Space hold spanning a modal
    /// dialog's open/close window must reset exactly the same way, and must NOT fire the release
    /// toggle it would have if the hold state had simply been carried through untouched (this test
    /// deliberately still sends the `Space` release event on the resumed tick — proving the reset
    /// happened, not merely that the release event was suppressed some other way).
    #[test]
    fn resumed_after_suppression_resets_a_space_hold_so_the_modals_close_does_not_fire_a_spurious_toggle() {
        let doc = doc_with_frames(2);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();

        // Frame 1: Space pressed — a hold begins, ordinary tick.
        let mut raw_down = egui::RawInput::default();
        raw_down.events.push(space_key_event(true));
        let _ = ctx.run_ui(raw_down, |ui| { p.tick(ui, false, false, &host); });
        assert!(p.state.borrow().space_hold_active, "sanity: the hold is active");

        // Frames 2..N (a modal is open): `handle_keys`/`tick` are skipped entirely by the host —
        // simulated here simply by not calling `tick` at all for those frames.

        // The first tick after the modal closes: the host delivers `resumed_after_suppression`.
        // The same Space-release event a plain tap would resolve as "release, toggle" arrives here
        // too, proving the *reset* is what suppresses the toggle, not merely a missing event.
        let mut raw_resume = egui::RawInput::default();
        raw_resume.events.push(space_key_event(false));
        let _ = ctx.run_ui(raw_resume, |ui| { p.tick(ui, false, true, &host); });

        assert!(!p.state.borrow().space_hold_active, "resuming after suppression must reset the hold");
        assert!(!p.state.borrow().playing, "the reset must win over the release event — no spurious toggle");
    }

    /// `O` flips `onion_enabled`, gated the same way as Space.
    #[test]
    fn tick_toggles_onion_enabled_on_o_while_unfocused() {
        let doc = doc_with_frames(2);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::O,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| { p.tick(ui, false, false, &host); });

        assert!(p.state.borrow().onion_enabled, "O must toggle onion_enabled on");
    }

    /// `O` must be suppressed while focused, matching Space's own gate.
    #[test]
    fn tick_does_not_toggle_onion_enabled_on_o_while_focused() {
        let doc = doc_with_frames(2);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::O,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| { p.tick(ui, true, false, &host); });

        assert!(!p.state.borrow().onion_enabled, "O must be suppressed while focused");
    }

    fn no_modifier_key_event(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    fn shift_d_event() -> egui::Event {
        egui::Event::Key { key: egui::Key::D, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT }
    }

    #[test]
    fn tick_comma_requests_the_previous_frame_via_panel_outcome() {
        let mut doc = doc_with_frames(3);
        doc.set_active_frame(1);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(no_modifier_key_event(egui::Key::Comma));
        let mut outcome = None;
        let _ = ctx.run_ui(raw, |ui| outcome = Some(p.tick(ui, false, false, &host)));
        assert_eq!(active_frame_of(&outcome.unwrap()), Some(0));
    }

    #[test]
    fn tick_comma_at_frame_zero_requests_nothing() {
        let doc = doc_with_frames(3); // active_frame defaults to 0
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(no_modifier_key_event(egui::Key::Comma));
        let mut outcome = None;
        let _ = ctx.run_ui(raw, |ui| outcome = Some(p.tick(ui, false, false, &host)));
        assert_eq!(active_frame_of(&outcome.unwrap()), None, "there is no frame before 0");
    }

    #[test]
    fn tick_period_requests_the_next_frame_via_panel_outcome() {
        let doc = doc_with_frames(3); // active_frame defaults to 0
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(no_modifier_key_event(egui::Key::Period));
        let mut outcome = None;
        let _ = ctx.run_ui(raw, |ui| outcome = Some(p.tick(ui, false, false, &host)));
        assert_eq!(active_frame_of(&outcome.unwrap()), Some(1));
    }

    /// Mirrors `tick_clamps_playback_frame_after_frame_count_shrinks_between_ticks`'s own edge
    /// case, for `.` instead of the playback clock: at the last valid frame, `.` must request
    /// nothing rather than requesting an out-of-range index.
    #[test]
    fn tick_period_at_the_last_frame_requests_nothing() {
        let mut doc = doc_with_frames(3);
        doc.set_active_frame(2); // the last valid index
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(no_modifier_key_event(egui::Key::Period));
        let mut outcome = None;
        let _ = ctx.run_ui(raw, |ui| outcome = Some(p.tick(ui, false, false, &host)));
        assert_eq!(active_frame_of(&outcome.unwrap()), None, "there is no frame past the last one");
    }

    #[test]
    fn tick_shift_d_requests_a_duplicate_frame_edit_matching_timelines_own_duplicate_active() {
        let doc = doc_with_frames(2);
        let mut p = AnimPlugin::new();
        let expected = crate::timeline::duplicate_active(&doc).unwrap();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(shift_d_event());
        let mut outcome = None;
        let _ = ctx.run_ui(raw, |ui| outcome = Some(p.tick(ui, false, false, &host)));
        let outcome = outcome.unwrap();
        assert_eq!(outcome.edits.len(), 1);
        match (&outcome.edits[0], &expected) {
            (gascii_core::Edit::AddFrame { index: a, .. }, gascii_core::Edit::AddFrame { index: b, .. }) => {
                assert_eq!(a, b, "Shift+D must duplicate at the exact same index timeline.rs's own button would")
            }
            other => panic!("expected two AddFrame edits, got {other:?}"),
        }
    }

    /// Shift+D at `Document::MAX_FRAMES` must surface the same `frame_op_error_message` wording
    /// `timeline.rs`'s own Duplicate button uses, via `PanelOutcome::error` rather than panicking or
    /// silently dropping the failure.
    #[test]
    fn tick_shift_d_at_max_frames_surfaces_the_same_error_timelines_duplicate_button_would() {
        let mut doc = Document::default_document();
        let mut history = gascii_core::History::new();
        for i in 1..Document::MAX_FRAMES {
            let edit = gascii_core::add_frame(&doc, i, gascii_core::Frame::blank(doc.width, doc.height)).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.frame_count(), Document::MAX_FRAMES);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(shift_d_event());
        let mut outcome = None;
        let _ = ctx.run_ui(raw, |ui| outcome = Some(p.tick(ui, false, false, &host)));
        let outcome = outcome.unwrap();
        assert!(outcome.edits.is_empty(), "a rejected duplicate must not also carry a partial edit");
        assert_eq!(outcome.error, Some(format!("duplicate frame: exceeds the {} maximum", Document::MAX_FRAMES)));
    }

    #[test]
    fn tick_comma_period_and_shift_d_are_suppressed_while_focused() {
        let mut doc = doc_with_frames(3);
        doc.set_active_frame(1);
        let mut p = AnimPlugin::new();
        let host = FakeHost(doc);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(no_modifier_key_event(egui::Key::Comma));
        raw.events.push(shift_d_event());
        let mut outcome = None;
        let _ = ctx.run_ui(raw, |ui| outcome = Some(p.tick(ui, true, false, &host)));
        let outcome = outcome.unwrap();
        assert_eq!(active_frame_of(&outcome), None, "',' must be suppressed while focused");
        assert!(outcome.edits.is_empty(), "Shift+D must be suppressed while focused");
    }
}
