//! Pen barrel-button state, recovered from raw Windows pointer messages.
//!
//! Pen input reaches winit as `WM_POINTER` messages, which it translates into touch events
//! carrying only position and pressure — the barrel-button flag never survives, so egui sees
//! every pen tap as a primary click. The message hook installed here reads the flag straight off
//! the raw messages before winit consumes them; the app's `raw_input_hook` consults it to
//! reroute barrel taps to the secondary button.
//!
//! This crate is the workspace's one `unsafe` boundary (see its `Cargo.toml`), kept as small as
//! the FFI allows: the `MSG` layout and message constants come from `windows-sys` (Microsoft's
//! generated bindings), the hook null-checks and then reads exactly two integer fields inside
//! the single `unsafe` expression, and all interpretation happens in a pure, tested, safe
//! function. The hook never consumes a message, so winit's own pointer handling (including
//! pen pressure) is untouched.

use std::sync::atomic::{AtomicBool, Ordering};

static BARREL_DOWN: AtomicBool = AtomicBool::new(false);

/// Live barrel state as of the most recent pointer message. Already `false` again by the time a
/// release event reaches egui (`WM_POINTERUP` carries no button flags), so a caller pairing a
/// press with its release must latch this at press time.
pub fn barrel_down() -> bool {
    BARREL_DOWN.load(Ordering::Relaxed)
}

pub fn set_barrel_down(down: bool) {
    BARREL_DOWN.store(down, Ordering::Relaxed);
}

/// `Some(barrel state)` if this is a pointer message that carries button flags, `None` for every
/// other message — a `None` must leave the last-known state untouched rather than clearing it.
/// The high word of `wParam` holds the `POINTER_MESSAGE_FLAG_*` bits (the low word is the
/// pointer id); `SECONDBUTTON` is the pen barrel button. Touch contacts never set it, and mouse
/// input doesn't route through `WM_POINTER`.
#[cfg(windows)]
fn pointer_barrel_state(message: u32, w_param: usize) -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        POINTER_MESSAGE_FLAG_SECONDBUTTON, WM_POINTERDOWN, WM_POINTERUP, WM_POINTERUPDATE,
    };
    matches!(message, WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP)
        .then_some((w_param >> 16) as u32 & POINTER_MESSAGE_FLAG_SECONDBUTTON != 0)
}

#[cfg(windows)]
pub fn install<T>(builder: &mut winit::event_loop::EventLoopBuilder<T>) {
    use windows_sys::Win32::UI::WindowsAndMessaging::MSG;
    use winit::platform::windows::EventLoopBuilderExtWindows as _;

    builder.with_msg_hook(|msg| {
        let msg = msg.cast::<MSG>();
        if msg.is_null() {
            return false;
        }
        // SAFETY: winit documents the hook argument as the `*const MSG` of its message pump —
        // written by `GetMessageW` immediately before the hook runs — so a non-null pointer is
        // valid for reads. Only the two plain-integer fields the interpretation needs are read;
        // no reference to the whole struct is formed.
        let (message, w_param) = unsafe { ((*msg).message, (*msg).wParam) };
        if let Some(down) = pointer_barrel_state(message, w_param) {
            set_barrel_down(down);
        }
        false // never consume — winit still owns pointer-to-touch translation (incl. pressure)
    });
}

#[cfg(not(windows))]
pub fn install<T>(_builder: &mut T) {}

#[cfg(all(test, windows))]
mod tests {
    use super::pointer_barrel_state;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        POINTER_MESSAGE_FLAG_FIRSTBUTTON, POINTER_MESSAGE_FLAG_SECONDBUTTON, WM_MOUSEMOVE,
        WM_POINTERDOWN, WM_POINTERUP, WM_POINTERUPDATE,
    };

    /// Builds a `WM_POINTER*` `wParam`: pointer id in the low word, message flags in the high.
    fn w_param(pointer_id: u16, flags: u32) -> usize {
        (pointer_id as usize) | ((flags as usize) << 16)
    }

    #[test]
    fn barrel_press_and_plain_press_read_from_the_high_word_flags() {
        assert_eq!(
            pointer_barrel_state(WM_POINTERDOWN, w_param(1, POINTER_MESSAGE_FLAG_SECONDBUTTON)),
            Some(true),
            "a contact with the barrel held must read as down"
        );
        assert_eq!(
            pointer_barrel_state(WM_POINTERDOWN, w_param(1, POINTER_MESSAGE_FLAG_FIRSTBUTTON)),
            Some(false),
            "a plain tip contact must read as up"
        );
        assert_eq!(
            pointer_barrel_state(WM_POINTERUP, w_param(1, 0)),
            Some(false),
            "WM_POINTERUP carries no button flags — the state must drop with it"
        );
        assert_eq!(
            pointer_barrel_state(WM_POINTERUPDATE, w_param(1, POINTER_MESSAGE_FLAG_SECONDBUTTON)),
            Some(true),
            "mid-stroke updates keep the state current"
        );
    }

    #[test]
    fn non_pointer_messages_are_ignored_even_with_flag_shaped_w_params() {
        assert_eq!(
            pointer_barrel_state(WM_MOUSEMOVE, w_param(0, POINTER_MESSAGE_FLAG_SECONDBUTTON)),
            None,
            "other messages reuse wParam for unrelated data and must never be interpreted"
        );
    }

    #[test]
    fn the_pointer_id_low_word_never_leaks_into_the_flags() {
        // A pointer id whose bits coincide with SECONDBUTTON must not read as a barrel press.
        assert_eq!(
            pointer_barrel_state(WM_POINTERDOWN, w_param(POINTER_MESSAGE_FLAG_SECONDBUTTON as u16, 0)),
            Some(false)
        );
    }
}
