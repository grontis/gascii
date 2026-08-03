use gascii_core::Edit;

/// A non-`Edit` document property a plugin's `panel`/`tick` wants overwritten directly — the host
/// applies each as a plain field write outside `History`, not an undo entry. New document
/// properties join this enum, not a new field on `PanelOutcome` — this enum is the resolution of
/// the growth policy that struct used to carry.
#[derive(Debug, Clone, PartialEq)]
pub enum DocProperty {
    /// Moves the editing cursor to a frame index. Not itself undoable — only structural
    /// `frame_ops::*` edits touch `History`; the cursor move itself is a plain field write the
    /// host resyncs on undo/redo, mirroring `ActiveLayer`'s identical contract below.
    ActiveFrame(usize),
    /// Overwrites `Document.loop_playback` directly — that field is a plain, non-`Edit`-tracked
    /// "set-and-forget" document property (mirrors `background`'s own precedent).
    LoopPlayback(bool),
    /// Overwrites `Document.frame_duration_ms` (the document-level default playback duration)
    /// directly — the same plain, non-`Edit`-tracked "set-and-forget" shape as `LoopPlayback`, for
    /// the identical reason (mirrors `background`'s own precedent).
    DefaultFrameDuration(u32),
    /// Moves the editing cursor to a layer index. Not itself undoable, mirroring `ActiveFrame`.
    ActiveLayer(usize),
}

/// What a plugin's `panel` wants to happen to the document this frame — collected by the host,
/// never applied by the plugin itself. Mirrors `gascii_core::frame_ops`'s own "pure value describes
/// the change; the caller with full context applies it" contract, one layer further out.
#[derive(Default)]
pub struct PanelOutcome {
    /// Applied in order via the host's own `apply_edit`, each its own undo entry — never batched
    /// into one `Edit`, so History's granularity matches what the user actually did one click at a
    /// time.
    pub edits: Vec<Edit>,
    /// Non-`Edit` document properties this outcome requests, applied in order via a plain field
    /// write outside `History` — see `DocProperty`.
    pub properties: Vec<DocProperty>,
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
        assert!(outcome.properties.is_empty());
        assert!(outcome.error.is_none());
    }

    /// Every variant round-trips through construction and an exhaustive match — pins the enum's
    /// shape so a future variant addition is caught here as a non-exhaustive-match compile error,
    /// not silently ignored by a consumer's `match`.
    #[test]
    fn doc_property_variants_construct_and_match_exhaustively() {
        let props =
            vec![DocProperty::ActiveFrame(3), DocProperty::LoopPlayback(true), DocProperty::DefaultFrameDuration(120), DocProperty::ActiveLayer(2)];
        for p in props {
            match p {
                DocProperty::ActiveFrame(i) => assert_eq!(i, 3),
                DocProperty::LoopPlayback(v) => assert!(v),
                DocProperty::DefaultFrameDuration(ms) => assert_eq!(ms, 120),
                DocProperty::ActiveLayer(i) => assert_eq!(i, 2),
            }
        }
    }
}
