//! gascii-core is headless: it has ZERO GUI dependencies. Never add eframe/egui/winit/wgpu here.

pub mod brush;
pub mod edit;
pub mod model;
pub mod palette;
pub mod tools;

pub use brush::{builtin_ramps, Ramp};
pub use edit::{CellEdit, Edit, History};
pub use model::{Cell, DocExtent, DocSettings, Document, Layer, Rgba};
pub use palette::{builtin_pages, validate_width, Page, WidthReject};
pub use tools::{
    eyedrop, line_cells, mask_apply, Eraser, Pencil, PendingCell, PlaneMask, Tool, ToolCtx,
    ToolEvent, ToolResponse,
};
