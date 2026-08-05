//! Playback/session state shared between `AnimPlugin`'s own `panel`/`tick` (called on the retained
//! plugin instance every frame) and `PlaybackRenderer` (folded once into `app.renderer` at startup
//! via `wrap_renderer` — a *different* object after that one-time fold). `wrap_renderer` clones the
//! `Rc` into the decorator it returns, so both sides read/write the same live state despite never
//! being the same Rust value again after construction. Single-threaded only (egui itself is
//! single-threaded) — `Rc<RefCell<..>>`, not `Arc<Mutex<..>>`.

use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

pub(crate) struct Inner {
    pub playing: bool,
    /// The frame playback is currently showing — independent of `Document.active_frame`/the
    /// editing cursor (playback never touches either). Meaningless while `!playing`.
    pub playback_frame: usize,
    /// Accumulated time since `playback_frame` last advanced, ms.
    pub elapsed_ms: f32,
    /// Whether a `Space` hold is currently in progress — the state `resolve_space_hold` tracks
    /// across frames to decide, at release, whether the hold was a play/pause tap or a space-pan
    /// drag. See `plugin.rs`'s `resolve_space_hold`.
    pub space_hold_active: bool,
    /// Whether a primary press occurred at any point during the current `Space` hold — latched for
    /// the whole hold, not just the frame it happened on.
    pub space_hold_saw_primary_press: bool,
    /// Previous tick's OS-level window-focus state, for edge-detecting focus loss — mirrors
    /// `GasciiApp::was_focused`'s own field exactly, tracked separately here because this plugin has
    /// no access to the host's copy. See `plugin.rs`'s `AnimPlugin::tick` for why the Space hold
    /// needs this.
    pub was_focused: bool,
    /// In-progress text for the active frame's duration field — `None` while the field just
    /// mirrors the live value. Committed (or discarded, on Escape) the moment the field loses
    /// focus; that always happens on the same paint as whatever click stole the focus, before the
    /// host drains any frame switch, so the commit can never land on a different frame than the
    /// one edited.
    pub duration_text: Option<String>,
    /// In-progress text for the document-default duration field — same lifecycle as
    /// `duration_text`.
    pub default_duration_text: Option<String>,
    /// The timeline panel's visibility override: `None` = auto (shown once the document has more
    /// than one frame), `Some(v)` = the user's explicit choice via the panel's own ▼ hide button /
    /// the collapsed bar's ▲ reopen button, which wins in both directions. When hidden, the slim
    /// collapsed bar always remains as the reopen affordance, so frame management is never more
    /// than one click away.
    pub timeline_open: Option<bool>,
}

impl Inner {
    /// Starts the playback clock at `at_frame` (the editing cursor at press time) — the one shape
    /// every play entry point (the Play button, the Space tap) uses.
    pub fn start_playback(&mut self, at_frame: usize) {
        self.playing = true;
        self.playback_frame = at_frame;
        self.elapsed_ms = 0.0;
    }

    /// Freezes playback and returns the frame it froze on, for the caller to park the editing
    /// cursor there — the canvas shows the active frame while idle, so without the park a pause
    /// would visually snap back to wherever the cursor was when Play was pressed.
    pub fn pause_playback(&mut self) -> usize {
        self.playing = false;
        self.playback_frame
    }
}

#[derive(Clone)]
pub(crate) struct SharedState(Rc<RefCell<Inner>>);

impl SharedState {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(Inner {
            playing: false,
            playback_frame: 0,
            elapsed_ms: 0.0,
            space_hold_active: false,
            space_hold_saw_primary_press: false,
            was_focused: true,
            duration_text: None,
            default_duration_text: None,
            timeline_open: None,
        })))
    }

    pub fn borrow(&self) -> Ref<'_, Inner> {
        self.0.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<'_, Inner> {
        self.0.borrow_mut()
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two `SharedState` handles obtained via `Clone` must observe each other's writes — the
    /// property `wrap_renderer`'s split leans on entirely.
    #[test]
    fn cloned_handles_share_the_same_underlying_state() {
        let a = SharedState::new();
        let b = a.clone();
        b.borrow_mut().playing = true;
        assert!(a.borrow().playing, "a clone must observe the other clone's write");
    }

    #[test]
    fn start_playback_resets_the_clock_and_pause_reports_the_frozen_frame() {
        let s = SharedState::new();
        {
            let mut inner = s.borrow_mut();
            inner.elapsed_ms = 42.0;
            inner.start_playback(3);
        }
        assert!(s.borrow().playing);
        assert_eq!(s.borrow().playback_frame, 3);
        assert_eq!(s.borrow().elapsed_ms, 0.0, "starting must reset the elapsed clock");

        s.borrow_mut().playback_frame = 5; // playback advanced
        let frozen = s.borrow_mut().pause_playback();
        assert!(!s.borrow().playing);
        assert_eq!(frozen, 5, "pause must report the frame it froze on, for the cursor park");
    }

    #[test]
    fn new_state_starts_idle() {
        let s = SharedState::new();
        let inner = s.borrow();
        assert!(!inner.playing);
        assert!(!inner.space_hold_active);
        assert!(!inner.space_hold_saw_primary_press);
        assert!(inner.was_focused, "starts focused, matching GasciiApp::was_focused's own default");
    }
}
