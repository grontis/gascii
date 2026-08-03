//! `gascii-layers`: the layers panel plugin — add/duplicate/remove/reorder/rename/show-hide, and
//! active-layer selection. Contributes no `Tool`, no canvas overlay (compositing is host-owned, see
//! `gascii_core::io::composite_cell`) — every capability here is plugin-drawn UI.
//!
//! Boundary policy mirrors `gascii-density-brush`/`gascii-anim`'s own: no eframe/winit/wgpu, only
//! `egui` and `gascii-core`.

mod kiosk;
mod panel;
mod plugin;
mod theme;
mod widgets;

pub use plugin::{make, LayersPlugin, DESCRIPTOR};
