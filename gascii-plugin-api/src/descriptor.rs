use crate::{Plugin, PluginShortcut, PluginToolCapabilities};

/// One plugin's whole registration story, as pure `'static` data — no instance required to read
/// it. `app.rs`'s `const PLUGINS: &[PluginDescriptor]` is the single ordered list every consumer
/// (`build_tools`'s description harvest, `with_state`'s retained-instance construction, the
/// key-claim/`?`-overlay shortcut harvest) reads, so "two sites must iterate the same list in the
/// same order" is structurally guaranteed rather than a convention two separately-written
/// functions have to uphold by hand.
#[derive(Clone, Copy)]
pub struct PluginDescriptor {
    /// For diagnostics only (a collision message, a panic naming the offending plugin) — never a
    /// lookup key.
    pub name: &'static str,
    /// Constructs the one real, per-app instance this descriptor describes. A named `fn`, not a
    /// closure, so the descriptor stays a plain `const` value with no capture-related lifetime
    /// questions.
    pub make: fn() -> Box<dyn Plugin>,
    /// The tool rows this plugin contributes, described without constructing an instance —
    /// `Plugin::tool_capabilities`, called directly as an associated function.
    pub tools: fn() -> Vec<PluginToolCapabilities>,
    /// The `tick`-driven shortcuts this plugin declares — `Plugin::shortcuts`, called the same way.
    pub shortcuts: fn() -> Vec<PluginShortcut>,
}
