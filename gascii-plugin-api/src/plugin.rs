use egui::Ui;
use gascii_core::DensityMode;

use crate::{CanvasRenderer, OptionsGeom, PanelOutcome, PluginHost, PluginToolCapabilities};

/// The host-facing contract every plugin implements. Every method but `register_tools` defaults to
/// a true no-op, so a plugin that only contributes tools needs to override nothing else.
pub trait Plugin: 'static {
    /// The tools this plugin contributes, described but not yet identified — the host merges each
    /// bundle into its own tool registry row (assigning tool identity and, for sized tools, the
    /// stamp-slot index).
    fn register_tools(&self) -> Vec<PluginToolCapabilities>;

    /// Custom per-tool options-panel content beyond the host's generic size/shape rows, rendered at
    /// most once per frame per owning plugin even if both bindings hold the same tool.
    fn options_ui(&mut self, _tool_name: &str, _ui: &mut Ui, _geom: OptionsGeom, _host: &dyn PluginHost) {}

    /// Per-frame input a plugin wants outside a canvas gesture (a keyboard shortcut, a playback
    /// clock tick). `focused`: true while a widget holds keyboard focus or an active session
    /// suppresses shortcuts (the same value the host's own shortcut handling already computes) —
    /// called unconditionally every frame, so a plugin that doesn't care about shortcuts (a
    /// playback clock) isn't starved whenever any field has focus. A plugin that DOES consume a
    /// shortcut (a digit-key intensity pick) checks `focused` itself before reacting.
    fn tick(&mut self, _ui: &mut Ui, _focused: bool, _host: &dyn PluginHost) {}

    /// A plugin-drawn panel, painted once per frame regardless of which tool is bound. `ui` is the
    /// host's own live root `Ui` — the same one every other chrome panel (titlebar, sidebar,
    /// status) is sequentially declared against — so a real `egui::Panel::bottom(..)`/`Panel::
    /// left(..)` called here correctly claims space from what's left after those, and the host's
    /// own `CentralPanel` (called after every plugin panel) sees the shrunk remainder in turn. A
    /// plain `&egui::Context` is deliberately NOT enough for this: egui's `Panel` reads and mutates
    /// its *parent `Ui`'s* own placer/cursor state directly (confirmed against egui 0.35's
    /// `containers/panel.rs`), which lives on the `Ui` value itself, not on `Context` — a panel
    /// built from a freshly constructed `Ui` would re-claim the whole screen, overlapping every
    /// panel already declared against the host's real one. `kiosk` reports which chrome mode is
    /// active so the plugin can pick its own geometry. The return value is how a mutation request
    /// reaches the document — see `PanelOutcome`.
    fn panel(&mut self, _ui: &mut Ui, _kiosk: bool, _host: &dyn PluginHost) -> PanelOutcome {
        PanelOutcome::default()
    }

    /// Wraps the canvas renderer, so a plugin can layer its own drawing above or below the host's.
    /// The default identity wrap is correct for a plugin that draws nothing of its own on the
    /// canvas.
    fn wrap_renderer(&self, inner: Box<dyn CanvasRenderer>) -> Box<dyn CanvasRenderer> {
        inner
    }

    /// Extra `ToolCtx` fields (density mode, ramp) for a plugin-owned tool that reads them —
    /// consulted only when the merged tool row's `wants_extra_ctx` is set.
    fn extra_tool_ctx(&self, _tool_name: &str) -> Option<(DensityMode, Vec<char>)> {
        None
    }

    /// Whether stylus pressure should currently override this tool's stamp size — the plugin's own
    /// runtime opt-in (e.g. a "Pressure" checkbox), distinct from the static `pressure_sizeable`
    /// capability a tool registers. Consulted only when the merged tool row's `pressure_sizeable`
    /// is set.
    fn pressure_override_enabled(&self, _tool_name: &str) -> bool {
        false
    }

    /// Downcast escape hatch for a live instance held behind `Box<dyn Plugin>`. Used by tests that
    /// need to inspect or mutate one specific plugin's own state (e.g. confirming it survives
    /// being rendered through two different chrome geometries unchanged) rather than only what the
    /// rest of this trait exposes. No default body: a `Self: Sized` bound would make it
    /// uncallable on `Box<dyn Plugin>`, so every implementor provides the trivial `{ self }`.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::CellGrid;
    use egui::{Pos2, Vec2};
    use gascii_core::{Document, PendingCell, SelectionView};

    struct NullPlugin;
    impl Plugin for NullPlugin {
        fn register_tools(&self) -> Vec<PluginToolCapabilities> {
            Vec::new()
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

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
    }

    struct MarkerRenderer;
    impl CanvasRenderer for MarkerRenderer {
        fn paint(
            &mut self,
            _painter: &egui::Painter,
            _doc: &Document,
            _vp: &dyn CellGrid,
            _origin: Pos2,
            _cell: Vec2,
            _visible: (u16, u16, u16, u16),
            _pending: &[PendingCell],
            _hover: &[(u16, u16)],
            _caret: Option<(u16, u16, bool)>,
            _selection: Option<SelectionView>,
        ) {
        }
    }

    fn geom() -> OptionsGeom {
        OptionsGeom { stepper_h: 1.0, shape_indent: 0.0, item_spacing_y: None, wrap_brush_mode: false, brush_slider_h: 1.0 }
    }

    /// Every default method must be callable without panicking and must produce exactly the
    /// documented no-op result — the contract a plugin that overrides nothing relies on.
    #[test]
    fn default_plugin_methods_are_true_no_ops() {
        let mut p = NullPlugin;
        let host = FakeHost(Document::default_document());

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            p.options_ui("anything", ui, geom(), &host);
            p.tick(ui, false, &host);
            let outcome = p.panel(ui, false, &host);
            assert!(outcome.edits.is_empty(), "default panel must request no edits");
            assert!(outcome.set_active_frame.is_none(), "default panel must not request a frame switch");
        });

        assert!(p.register_tools().is_empty());
        assert!(p.extra_tool_ctx("anything").is_none());
        assert!(!p.pressure_override_enabled("anything"));

        let inner: Box<dyn CanvasRenderer> = Box::new(MarkerRenderer);
        let inner_addr = (inner.as_ref() as *const dyn CanvasRenderer).cast::<()>();
        let wrapped = p.wrap_renderer(inner);
        let wrapped_addr = (wrapped.as_ref() as *const dyn CanvasRenderer).cast::<()>();
        assert_eq!(inner_addr, wrapped_addr, "default wrap_renderer must return the exact Box it was given");
    }

    /// `as_any_mut`'s default body must actually resolve to the concrete type behind the trait
    /// object, not merely compile — a plugin-crate test double downcasts through it here so the
    /// same escape hatch `gascii`'s own tests lean on is proven against a type outside this crate.
    #[test]
    fn as_any_mut_downcasts_to_the_concrete_plugin_type() {
        let mut p: Box<dyn Plugin> = Box::new(NullPlugin);
        assert!(p.as_any_mut().downcast_mut::<NullPlugin>().is_some());
    }
}
