/// Narrow, read-only app facts a plugin may consult from `options_ui`/`tick`/`panel`; never a path
/// to arbitrary app state — a plugin's own state is `&mut self`, never reached through this trait.
pub trait PluginHost {
    /// Whether this session has observed a pressure-bearing stylus contact.
    fn stylus_detected(&self) -> bool;
    /// Whether either binding currently holds the tool named `tool_name`.
    fn is_bound(&self, tool_name: &str) -> bool;
    /// Read-only document access — frame count/content/durations, for a panel building its own UI
    /// (thumbnails, the fps field, which frame is active) or a playback clock reading resolved
    /// frame timing. Still narrow: no path to mutate the document through this trait (see
    /// `PanelOutcome` for how a plugin requests a mutation instead) or to reach any other app state.
    fn document(&self) -> &gascii_core::Document;
}
