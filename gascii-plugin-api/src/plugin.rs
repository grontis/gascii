use egui::Ui;

use crate::{CanvasRenderer, OptionsGeom, PanelOutcome, PluginHost, PluginShortcut, PluginToolCapabilities, ToolCtxPatch};

/// The host-facing contract every plugin implements. Every method has a true-no-op default except
/// `as_any_mut` (which cannot: a `Self: Sized` bound would make it uncallable on `Box<dyn Plugin>`)
/// — a plugin that only contributes tools (or only draws a panel, or only wants a `tick`) overrides
/// nothing else.
pub trait Plugin: 'static {
    /// The tools this plugin contributes, described but not yet identified — the host merges each
    /// bundle into its own tool registry row (assigning tool identity and, for sized tools, the
    /// stamp-slot index). An associated function, not a method: the host's `PluginDescriptor.tools`
    /// calls this directly, so no instance is ever constructed purely to read a description off it.
    /// `where Self: Sized` excludes it from the vtable, the same reasoning `as_any_mut`'s own doc
    /// comment already relies on for object safety.
    fn tool_capabilities() -> Vec<PluginToolCapabilities>
    where
        Self: Sized,
    {
        Vec::new()
    }

    /// The `tick`-driven shortcuts this plugin declares — feeds the `?` overlay's PLUGINS section,
    /// the app's key-claim set, and the startup collision check. Same associated-function shape as
    /// `tool_capabilities`, for the same reason.
    fn shortcuts() -> Vec<PluginShortcut>
    where
        Self: Sized,
    {
        Vec::new()
    }

    /// Custom per-tool options-panel content beyond the host's generic size/shape rows, rendered at
    /// most once per frame per owning plugin even if both bindings hold the same tool.
    fn options_ui(&mut self, _tool_name: &str, _ui: &mut Ui, _geom: OptionsGeom, _host: &dyn PluginHost) {}

    /// Per-frame input a plugin wants outside a canvas gesture (a keyboard shortcut, a playback
    /// clock tick). `focused`: true while a widget holds keyboard focus or an active session
    /// suppresses shortcuts (the same value the host's own shortcut handling already computes) —
    /// called unconditionally every frame, so a plugin that doesn't care about shortcuts (a
    /// playback clock) isn't starved whenever any field has focus. A plugin that DOES consume a
    /// shortcut (a digit-key intensity pick) checks `focused` itself before reacting.
    ///
    /// Returns a `PanelOutcome` exactly like `panel` does — the same document-mutation channel, for
    /// a shortcut that needs to apply an `Edit` or move the editing cursor (a frame-navigation key,
    /// a duplicate-frame chord) rather than only mutate the plugin's own session state directly. The
    /// host applies it via the same drain pass `panel`'s outcome goes through. A plugin whose
    /// shortcuts only ever mutate their own state (or none at all) needs no override here beyond the
    /// default, which returns `PanelOutcome::default()` — a true no-op.
    ///
    /// `resumed_after_suppression`: `true` on the first `tick` call after one or more frames were
    /// skipped because a modal dialog was open (`tick` only runs while `!modal_open()`) — the host
    /// latches this the moment it skips a frame and delivers it once, here, the next time it
    /// actually calls `tick` again. A plugin holding cross-frame key-hold state (a press-and-hold
    /// gesture spanning several ticks) must treat this exactly like an OS focus-loss interruption:
    /// the hold's `pressed`/`released` edges never crossed this plugin's own `tick` while suppressed
    /// (egui's own input state is a per-viewport global the modal's own event loop also consumed
    /// from), so any state built up before the suppression started is stale and must be reset, not
    /// resumed. A plugin with no such state ignores this safely.
    fn tick(&mut self, _ui: &mut Ui, _focused: bool, _resumed_after_suppression: bool, _host: &dyn PluginHost) -> PanelOutcome {
        PanelOutcome::default()
    }

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

    /// A named, defaultable patch over `ToolCtx`'s extra fields (density mode, ramp) for a
    /// plugin-owned tool that reads them — consulted only when the merged tool row's
    /// `wants_ctx_patch` is set. `canvas::tool_ctx` applies whichever fields are `Some` over the
    /// context's own defaults; `None` (the whole return value, or either field) leaves the
    /// corresponding default untouched.
    fn tool_ctx_patch(&self, _tool_name: &str) -> Option<ToolCtxPatch> {
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
        fn top_edit_id(&self) -> Option<u64> {
            None
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
        OptionsGeom { stepper_h: 1.0, shape_indent: 0.0, item_spacing_y: None, inline_controls: false, slider_h: 1.0 }
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
            let tick_outcome = p.tick(ui, false, false, &host);
            assert!(tick_outcome.edits.is_empty(), "default tick must request no edits");
            assert!(tick_outcome.properties.is_empty(), "default tick must not request a document property change");
            let outcome = p.panel(ui, false, &host);
            assert!(outcome.edits.is_empty(), "default panel must request no edits");
            assert!(outcome.properties.is_empty(), "default panel must not request a document property change");
        });

        assert!(NullPlugin::tool_capabilities().is_empty());
        assert!(NullPlugin::shortcuts().is_empty());
        assert!(p.tool_ctx_patch("anything").is_none());
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
