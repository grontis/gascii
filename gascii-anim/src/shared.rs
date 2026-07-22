//! Playback/onion state shared between `AnimPlugin`'s own `panel`/`tick` (called on the retained
//! plugin instance every frame) and `OnionRenderer` (folded once into `app.renderer` at startup via
//! `wrap_renderer` — a *different* object after that one-time fold). `wrap_renderer` clones the
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
    pub onion_enabled: bool,
    pub onion_prev: u8,
    pub onion_next: u8,
}

#[derive(Clone)]
pub(crate) struct SharedState(Rc<RefCell<Inner>>);

impl SharedState {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(Inner {
            playing: false,
            playback_frame: 0,
            elapsed_ms: 0.0,
            onion_enabled: false,
            onion_prev: 1,
            onion_next: 1,
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
    fn new_state_starts_idle_with_default_onion_depth() {
        let s = SharedState::new();
        let inner = s.borrow();
        assert!(!inner.playing);
        assert!(!inner.onion_enabled);
        assert_eq!(inner.onion_prev, 1);
        assert_eq!(inner.onion_next, 1);
    }
}
