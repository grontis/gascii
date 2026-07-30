use crate::IconPath;

/// What a plugin contributes for one tool row: display facts plus the capability booleans the
/// host merges into its own tool registry. Tool *identity* and persistence-critical indexing stay
/// host-assigned — a bundle only says "I'm sized" via `sized`, never which literal stamp slot it
/// lands in, and carries no tool-kind value at all.
pub struct PluginToolCapabilities {
    pub name: &'static str,
    pub key: egui::Key,
    pub tip: &'static str,
    pub make: fn() -> Box<dyn gascii_core::Tool>,
    /// This tool's own icon, authored in a 16x16 viewBox (see `IconPath`). An empty slice falls
    /// back to the tool name's first letter, painted centered in its cell — never a panic, never a
    /// blank cell.
    pub icon: &'static [IconPath],
    /// Whether this tool has a size/shape footprint; the host assigns the actual stamp-slot index.
    pub sized: bool,
    /// Whether this tool can hold a cross-frame session (uncommitted work outliving one stroke).
    pub holds_session: bool,
    /// Whether this tool gets a hover marker previewing its next application.
    pub shows_hover: bool,
    /// Whether a stroke of this tool that stamped the glyph plane counts toward RECENT.
    pub stamps_glyph: bool,
    /// Whether an active session of this tool swallows the single-letter tool-select shortcuts.
    pub suppresses_shortcuts: bool,
    /// Whether this tool gets a cell in kiosk's touch sidebar grid.
    pub kiosk_visible: bool,
    /// Whether a stylus-pressure stroke should override this tool's stamp size.
    pub pressure_sizeable: bool,
    /// Whether `tool_ctx` should ask the owning plugin for a `ToolCtxPatch`
    /// (`Plugin::tool_ctx_patch`) while this tool is bound.
    pub wants_ctx_patch: bool,
}
