//! gascii-core is headless: it has ZERO GUI dependencies. Never add eframe/egui/winit/wgpu here.

pub mod brush;
pub mod clipboard;
pub mod edit;
pub mod io;
pub mod join;
pub mod model;
pub mod palette;
pub mod tools;

pub use brush::{builtin_ramps, Ramp};
pub use clipboard::CellPatch;
pub use edit::{CellEdit, Edit, History};
pub use io::export_text::export_text;
pub use io::gascii_json::{load_str, save_string, LoadError, CURRENT_VERSION};
pub use io::composite;
pub use join::{arms_of, char_of, join as join_arms, ArmSet};
pub use model::{Cell, DocExtent, DocSettings, Document, Layer, Rgba};
pub use palette::{allowed_in, builtin_pages, page_available, validate_width, EntryReject, Page, WidthReject};
pub use tools::{
    eyedrop, line_cells, mask_apply, CellRect, Direction, Eraser, FloodFill, Line, Pencil,
    PendingCell, PlaneMask, Rectangle, SelectionTool, SelectionView, TextTool, Tool, ToolCtx,
    ToolEvent, ToolResponse,
};
