//! Host-facing plugin contract: a plugin registers tools and contributes optional per-frame UI
//! (options rows, its own panel, a canvas-renderer wrap) — all without naming the host's
//! `GasciiApp`/`ToolKind`. Boundary policy mirrors `gascii-core`'s own: no eframe/winit/wgpu here,
//! ever — only `egui` (widget/paint types) and `gascii-core` (the document/tool model a plugin's
//! tools actually operate on).

mod host;
mod options;
mod panel;
mod plugin;
mod renderer;
mod tool;

pub use host::PluginHost;
pub use options::OptionsGeom;
pub use panel::PanelOutcome;
pub use plugin::Plugin;
pub use renderer::{cell_rect_to_screen, CanvasRenderer, CellGrid};
pub use tool::PluginToolCapabilities;
