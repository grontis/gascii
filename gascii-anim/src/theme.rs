//! A minimal, vendored copy of the host's color tokens — the fields `widgets::button`/`checkbox`/
//! `micro_label` and the timeline/kiosk panel bodies actually read. Duplicated rather than threaded
//! through `PluginHost`, mirroring `gascii-density-brush::theme`'s own precedent exactly (a pure
//! per-theme constant table, no `GasciiApp` dependency, not worth growing the host trait for one
//! panel's styling). Every value is copied verbatim from `gascii::ui::theme::Tokens`.

use egui::Color32;

/// A translucent colour from straight (un-premultiplied) components — mirrors
/// `gascii::ui::theme::translucent` exactly.
const fn translucent(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied((r as u16 * a as u16 / 255) as u8, (g as u16 * a as u16 / 255) as u8, (b as u16 * a as u16 / 255) as u8, a)
}

#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub bg_panel: Color32,
    pub fg_text: Color32,
    pub fg_secondary: Color32,
    pub border_strong: Color32,
    pub border_soft: Color32,
    pub bg_hover: Color32,
    pub bg_inverse: Color32,
    pub fg_inverse: Color32,
    pub window_edge: Color32,
}

const LIGHT: Tokens = Tokens {
    bg_panel: Color32::from_rgb(0xF6, 0xF5, 0xF2),
    fg_text: Color32::from_rgb(0x1C, 0x1B, 0x19),
    fg_secondary: Color32::from_rgb(0x71, 0x6C, 0x63),
    border_strong: Color32::from_rgb(0x1C, 0x1B, 0x19),
    border_soft: Color32::from_rgb(0xC9, 0xC5, 0xBD),
    bg_hover: translucent(0x1C, 0x1B, 0x19, 0x17),
    bg_inverse: Color32::from_rgb(0x1C, 0x1B, 0x19),
    fg_inverse: Color32::from_rgb(0xF6, 0xF5, 0xF2),
    window_edge: Color32::from_rgb(0x1C, 0x1B, 0x19),
};

const DARK: Tokens = Tokens {
    bg_panel: Color32::from_rgb(0x2E, 0x2D, 0x2B),
    fg_text: Color32::from_rgb(0xE6, 0xE3, 0xDE),
    fg_secondary: Color32::from_rgb(0x98, 0x93, 0x8A),
    border_strong: Color32::from_rgb(0x5A, 0x57, 0x50),
    border_soft: Color32::from_rgb(0x45, 0x43, 0x40),
    bg_hover: translucent(0xE6, 0xE3, 0xDE, 0x1F),
    bg_inverse: Color32::from_rgb(0xE6, 0xE3, 0xDE),
    fg_inverse: Color32::from_rgb(0x1C, 0x1B, 0x19),
    window_edge: Color32::from_rgb(0x06, 0x06, 0x06),
};

/// The palette matching whatever theme egui currently resolves to.
pub(crate) fn current(ctx: &egui::Context) -> Tokens {
    match ctx.theme() {
        egui::Theme::Light => LIGHT,
        egui::Theme::Dark => DARK,
    }
}
