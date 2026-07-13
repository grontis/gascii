use std::sync::{Arc, OnceLock};

use eframe::egui;

/// Named family used EXCLUSIVELY for the canvas. UI chrome keeps egui defaults.
pub const CANVAS_FONT: &str = "iosevka-fixed";

/// The bundled canvas font's raw bytes, shared by the live egui canvas and `png_export.rs`'s
/// off-screen `fontdue` rasterizer — one embedded font asset, one source of truth.
pub(crate) const CANVAS_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/IosevkaFixed-Regular.ttf");

/// `Arc<str>` backing `FontFamily::Name`, built once and cloned (cheap refcount bump) on every
/// `canvas_font_id` call instead of re-allocating a fresh `Arc<str>` from `CANVAS_FONT` each time.
static CANVAS_FONT_NAME: OnceLock<Arc<str>> = OnceLock::new();

fn canvas_font_name() -> Arc<str> {
    CANVAS_FONT_NAME
        .get_or_init(|| Arc::from(CANVAS_FONT))
        .clone()
}

pub fn install_canvas_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default(); // keep chrome defaults
    fonts.font_data.insert(
        CANVAS_FONT.to_owned(),
        Arc::new(egui::FontData::from_static(CANVAS_FONT_BYTES)),
    );
    // Dedicated Name family; NOT inserted into Proportional/Monospace, so chrome is unaffected.
    fonts.families.insert(
        egui::FontFamily::Name(canvas_font_name()),
        vec![CANVAS_FONT.to_owned()],
    );
    ctx.set_fonts(fonts);
}

pub fn canvas_font_id(px: f32) -> egui::FontId {
    egui::FontId::new(px, egui::FontFamily::Name(canvas_font_name()))
}
