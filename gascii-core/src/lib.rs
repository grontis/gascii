//! gascii-core is headless: it has ZERO GUI dependencies. Never add eframe/egui/winit/wgpu here.

pub mod model;

pub use model::{Cell, DocExtent, DocSettings, Document, Layer, Rgba};
