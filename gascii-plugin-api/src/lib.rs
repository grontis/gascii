//! Host-facing plugin contract: a plugin registers tools and contributes optional per-frame UI
//! (options rows, its own panel, a canvas-renderer wrap) — all without naming the host's
//! `GasciiApp`/`ToolKind`. Boundary policy mirrors `gascii-core`'s own: no eframe/winit/wgpu here,
//! ever — only `egui` (widget/paint types) and `gascii-core` (the document/tool model a plugin's
//! tools actually operate on).
//!
//! **Source stability, not semver.** This crate is source-stable only: every consumer is an
//! in-workspace crate compiled from the same tree, so a breaking change here is absorbed by fixing
//! consumers in the same commit. No semver contract, no deprecation cycles, no ABI stability, and
//! no dynamic loading — the workspace's `unsafe_code = "forbid"` forecloses `cdylib` plugins
//! outright. The trait has already absorbed one breaking change this way (the `register_tools`
//! method becoming the associated function `tool_capabilities`).
//!
//! **Plugin-crate `Tool` impls are permitted.** `gascii_core::Tool` and every type in its
//! signature are public core API, and `PluginToolCapabilities::make` is a plain fn pointer, so a
//! plugin crate may own a complete `Tool` implementation with no core changes at all — the
//! `NoopTool` test below demonstrates the seam. In practice a tool needing core-internal machinery
//! (box-join tables, ramps, plane-mask semantics) still wants its implementation in core, which is
//! why `gascii-density-brush` keeps `DensityBrush` there and owns only UI and session state. Both
//! arrangements are supported; the split is a per-tool judgement call, not a rule.
//! `ToolCtx.density`/`.ramp` living in core (rather than an opaque `&dyn Any` extras channel on
//! `ToolCtxPatch`) is a direct consequence of that same judgement call: every field there already
//! has a concrete, public core type, and an opaque channel would trade one honest coupling for a
//! runtime-typed one for no plugin this workspace ships.

mod batch;
mod descriptor;
mod host;
mod icon;
mod options;
mod panel;
mod plugin;
mod renderer;
mod shortcut;
mod tool;
mod tool_ctx_patch;

pub use batch::CellBatch;
pub use descriptor::PluginDescriptor;
pub use host::PluginHost;
pub use icon::IconPath;
pub use options::OptionsGeom;
pub use panel::{DocProperty, PanelOutcome};
pub use plugin::Plugin;
pub use renderer::{cell_rect_to_screen, CanvasRenderer, CellGrid};
pub use shortcut::PluginShortcut;
pub use tool::PluginToolCapabilities;
pub use tool_ctx_patch::ToolCtxPatch;

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::{Document, PendingCell, Tool, ToolCtx, ToolEvent, ToolResponse};

    /// A complete `gascii_core::Tool` implementation, owned entirely by this (non-core) crate — the
    /// seam the crate-doc policy above claims a plugin crate may use: `gascii_core::Tool` is public
    /// core API, and `PluginToolCapabilities::make` is a plain fn pointer, so no core change is
    /// needed to register a tool whose whole implementation lives outside `gascii-core`.
    struct NoopTool;

    impl Tool for NoopTool {
        fn update(&mut self, _ev: ToolEvent, _ctx: &ToolCtx, _doc: &Document) -> ToolResponse {
            ToolResponse::Idle
        }

        fn pending(&self) -> &[PendingCell] {
            &[]
        }
    }

    #[test]
    fn a_plugin_crate_tool_impl_registers_through_plugin_tool_capabilities() {
        let cap = PluginToolCapabilities {
            name: "Noop",
            key: egui::Key::Z,
            tip: "does nothing",
            make: || Box::new(NoopTool),
            icon: &[],
            sized: false,
            holds_session: false,
            shows_hover: false,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: false,
            pressure_sizeable: false,
            wants_ctx_patch: false,
        };
        let mut tool = (cap.make)();
        let doc = Document::default_document();
        let ctx = ToolCtx {
            frame: 0,
            layer: 0,
            glyph: '#',
            fg: gascii_core::Rgba::WHITE,
            bg: gascii_core::Rgba::TRANSPARENT,
            mask: gascii_core::PlaneMask::default(),
            density: gascii_core::DensityMode::Fixed(gascii_core::Fixed(1.0)),
            ramp: Vec::new(),
            size: 1,
            shape: gascii_core::BrushShape::default(),
        };
        assert!(matches!(
            tool.update(ToolEvent::Press { x: 0, y: 0 }, &ctx, &doc),
            ToolResponse::Idle
        ));
        assert!(tool.pending().is_empty());
    }
}
