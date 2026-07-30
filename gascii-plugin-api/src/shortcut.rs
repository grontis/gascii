/// One `tick`-driven shortcut a plugin declares — the structured counterpart to reading a raw key
/// out of `Ui::input_mut` inside `Plugin::tick` with nothing telling the host it happened. Feeds
/// three consumers: the `?` overlay's PLUGINS section, the app's key-claim set, and the startup
/// collision check.
///
/// Deliberately no gate/modifier-class field: a plugin's shortcut can only ever fire from inside
/// its own `tick`, which the host cannot see or condition on, so there is nothing structural for a
/// gate to drive — a conditional shortcut's condition lives in `name`'s own prose (e.g. "Brush
/// intensity (while Brush is bound)").
pub struct PluginShortcut {
    /// The action name for the `?` overlay — e.g. "Play / Pause", "Brush intensity (while Brush is
    /// bound)".
    pub name: &'static str,
    /// Display label, exactly as the overlay should show it — e.g. "Space (hold)", "Shift+D",
    /// "1-9, 0".
    pub label: &'static str,
    /// Every bare key this shortcut can claim. Modifiers are deliberately NOT modelled here:
    /// `egui::Modifiers::matches_logically` ignores extra Shift/Alt, so `Shift+D` and a bare tool
    /// key `D` genuinely shadow each other — reserving the bare key is the conservative, correct
    /// set. A COMMAND-modified plugin shortcut (none exist today) would over-reserve its bare key —
    /// a false positive that costs a plugin author one key, never a silent shadowing bug.
    pub keys: &'static [egui::Key],
}
