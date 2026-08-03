use egui::Ui;
use gascii_plugin_api::{PanelOutcome, Plugin, PluginDescriptor, PluginHost};

/// Unlike `AnimPlugin`, `LayersPlugin` needs no `SharedState`/`Rc<RefCell<>>` — it has no canvas
/// decorator and no cross-frame session state; every row's state (which layer is active,
/// visibility, names) already lives on `Document` itself, read fresh from `host.document()` every
/// frame. The one piece of ephemeral UI state (which row, if any, is mid-rename) lives in egui's
/// own temp storage — see `panel.rs`.
pub struct LayersPlugin;

/// Constructs the one real, per-app `LayersPlugin` instance — the `PluginDescriptor.make` fn
/// pointer. A named fn, not a closure, so `DESCRIPTOR` stays a plain `const`.
pub fn make() -> Box<dyn Plugin> {
    Box::new(LayersPlugin)
}

/// This crate's whole registration story, harvested by the host's `const PLUGINS` table without
/// ever constructing a throwaway instance.
pub const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: "gascii-layers",
    name: "Layers",
    description: "A layer list: add, duplicate, remove, reorder, rename, and show/hide layers.",
    version: env!("CARGO_PKG_VERSION"),
    make,
    tools: LayersPlugin::tool_capabilities,
    shortcuts: LayersPlugin::shortcuts,
};

// This plugin contributes no `Tool` and no `PluginShortcut` (v1: all interaction is through the
// panel's own buttons/rows) — `tool_capabilities`/`shortcuts` are left at their defaults, and no
// `tick`/`wrap_renderer` override is needed either (compositing is host-owned, see
// `gascii_core::io::composite_cell`).
impl Plugin for LayersPlugin {
    fn panel(&mut self, ui: &mut Ui, kiosk: bool, host: &dyn PluginHost) -> PanelOutcome {
        let doc = host.document();
        if kiosk {
            crate::kiosk::show(ui, doc)
        } else {
            crate::panel::show(ui, doc)
        }
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

    #[test]
    fn tool_capabilities_and_shortcuts_are_empty() {
        assert!(LayersPlugin::tool_capabilities().is_empty());
        assert!(LayersPlugin::shortcuts().is_empty());
    }

    #[test]
    fn panel_delegates_to_the_windowed_module_when_not_kiosk() {
        let mut p = LayersPlugin;
        let host = FakeHost(Document::default_document());
        let ctx = egui::Context::default();
        let mut outcome = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| outcome = Some(p.panel(ui, false, &host)));
        let outcome = outcome.unwrap();
        assert!(outcome.edits.is_empty());
        assert!(outcome.properties.is_empty());
    }

    #[test]
    fn panel_delegates_to_the_kiosk_module_when_kiosk() {
        let mut p = LayersPlugin;
        let host = FakeHost(Document::default_document());
        let ctx = egui::Context::default();
        let mut outcome = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| outcome = Some(p.panel(ui, true, &host)));
        let outcome = outcome.unwrap();
        assert!(outcome.edits.is_empty());
        assert!(outcome.properties.is_empty());
    }
}
