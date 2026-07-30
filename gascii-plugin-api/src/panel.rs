use gascii_core::Edit;

/// What a plugin's `panel` wants to happen to the document this frame — collected by the host,
/// never applied by the plugin itself. Mirrors `gascii_core::frame_ops`'s own "pure value describes
/// the change; the caller with full context applies it" contract, one layer further out.
#[derive(Default)]
pub struct PanelOutcome {
    /// Applied in order via the host's own `apply_edit`, each its own undo entry — never batched
    /// into one `Edit`, so History's granularity matches what the user actually did one click at a
    /// time.
    pub edits: Vec<Edit>,
    /// A request to move the editing cursor. Not itself undoable — mirrors `active_layer`'s own
    /// plain-session-state precedent; only structural `frame_ops::*` edits touch `History`. `None`
    /// means "no change requested this frame."
    pub set_active_frame: Option<usize>,
    /// A request to overwrite `Document.loop_playback` directly — that field is a plain, non-`Edit`-
    /// tracked "set-and-forget" document property (mirrors `background`'s own precedent), so this
    /// mirrors `set_active_frame`'s exact shape: a plain field write the host applies outside
    /// `History`, not an undo entry. `None` means "no change requested this frame."
    pub set_loop_playback: Option<bool>,
    /// A request to overwrite `Document.frame_duration_ms` (the document-level default playback
    /// duration) directly — the same plain, non-`Edit`-tracked "set-and-forget" shape as
    /// `set_loop_playback`, for the identical reason (mirrors `background`'s own precedent). `None`
    /// means "no change requested this frame."
    ///
    /// Growth policy for this struct: one `Option<T>` field per non-`Edit` document property the
    /// host applies outside `History`. If this list reaches four, replace it with a
    /// `Vec<DocProperty>` enum so `drain_panel_outcomes` gets an exhaustive match instead of one
    /// more hand-written `if let` per property.
    pub set_default_frame_duration: Option<u32>,
    /// A readable failure message for a control this panel drew that could not carry out its
    /// requested change (e.g. a `frame_ops::*` call rejected by `MAX_FRAMES`/the cell budget). The
    /// host writes this straight into `last_error`, the same channel every other structural action
    /// already uses (`add_frame_via_menu`, "Resize Canvas…") — never a raw `{e:?}` dump. `None`
    /// means "nothing failed this frame."
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trait's own default no-op contract relies on this being a true empty value — a plugin
    /// that overrides nothing must request nothing.
    #[test]
    fn default_outcome_requests_nothing() {
        let outcome = PanelOutcome::default();
        assert!(outcome.edits.is_empty());
        assert!(outcome.set_active_frame.is_none());
        assert!(outcome.set_loop_playback.is_none());
        assert!(outcome.set_default_frame_duration.is_none());
        assert!(outcome.error.is_none());
    }
}
