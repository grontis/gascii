//! `gascii-anim`: the animation plugin — a timeline panel (windowed + kiosk), frame switching
//! wired through the host's `apply_edit` choke point via `PanelOutcome`, and playback
//! (`tick`-driven, `request_repaint_after`-only, never a thread; a `CanvasRenderer` decorator
//! sharing live state with this plugin's own retained instance via `Rc<RefCell<>>` paints the
//! played frame). Contributes no `Tool` — every capability here is plugin-drawn UI or
//! `tick`-driven input, never a canvas gesture.
//!
//! Boundary policy mirrors `gascii-density-brush`'s own: no eframe/winit/wgpu, only `egui` and
//! `gascii-core`.

mod decorator;
mod kiosk;
mod plugin;
mod shared;
mod theme;
mod thumbnail;
mod timeline;
mod widgets;

pub use plugin::{make, AnimPlugin, DESCRIPTOR};
