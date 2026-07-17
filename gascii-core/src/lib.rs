//! gascii-core is headless: it has ZERO GUI dependencies. Never add eframe/egui/winit/wgpu here.

pub mod brush;
pub mod clear;
pub mod clipboard;
pub mod edit;
pub mod io;
pub mod join;
pub mod model;
pub mod palette;
pub mod resize;
pub mod tools;

pub use brush::{
    builtin_ramps, intensity_to_index, Buildup, DensityMode, Fixed, IntensitySource, Ramp,
    StrokeSample,
};
pub use clear::clear_document;
pub use clipboard::CellPatch;
pub use edit::{CellEdit, DocSnapshot, Edit, History};
pub use resize::{resize_document, AxisAnchor, ResizeAnchor, ResizeError};
pub use io::export_png::{validate_png_dimensions, PngExportError, MAX_PNG_PIXELS};
pub use io::export_text::export_text;
pub use io::gascii_json::{load_str, save_string, LoadError, CURRENT_VERSION};
pub use io::composite;
pub use join::{arms_of, char_of, join as join_arms, ArmSet};
pub use model::{Cell, DocExtent, Document, Layer, Rgba};
pub use palette::{builtin_pages, validate_width, Page, WidthReject};
pub use tools::{
    eyedrop, footprint, line_cells, mask_apply, BrushShape, CellRect, DensityBrush, Direction,
    Eraser, FloodFill, Line, Pencil, PendingCell, PlaneMask, Rectangle, SelectionTool,
    SelectionView, TextTool, Tool, ToolCtx, ToolEvent, ToolResponse, MAX_TOOL_SIZE,
};
