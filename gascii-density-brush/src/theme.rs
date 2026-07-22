//! A minimal, vendored copy of the host's color tokens — just the six fields
//! `widgets::segmented`/`checkbox`/`micro_label` actually read. Duplicated rather than threaded
//! through `PluginHost`: the values are pure per-theme constants (no `GasciiApp` dependency), and
//! growing the host trait for one options block's styling isn't warranted while there's only one
//! consumer. Every value is copied verbatim from `gascii`'s own `ui::theme::Tokens`.

use egui::Color32;

/// A translucent colour from straight (un-premultiplied) components — mirrors
/// `gascii::ui::theme::translucent` exactly, needed here for the same reason: `Color32` stores
/// premultiplied alpha and its unmultiplied constructor isn't `const`.
const fn translucent(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        (r as u16 * a as u16 / 255) as u8,
        (g as u16 * a as u16 / 255) as u8,
        (b as u16 * a as u16 / 255) as u8,
        a,
    )
}

#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub fg_text: Color32,
    pub fg_secondary: Color32,
    pub border_strong: Color32,
    pub border_soft: Color32,
    pub bg_hover: Color32,
    pub bg_inverse: Color32,
    pub fg_inverse: Color32,
}

const LIGHT: Tokens = Tokens {
    fg_text: Color32::from_rgb(0x1C, 0x1B, 0x19),
    fg_secondary: Color32::from_rgb(0x71, 0x6C, 0x63),
    border_strong: Color32::from_rgb(0x1C, 0x1B, 0x19),
    border_soft: Color32::from_rgb(0xC9, 0xC5, 0xBD),
    bg_hover: translucent(0x1C, 0x1B, 0x19, 0x17),
    bg_inverse: Color32::from_rgb(0x1C, 0x1B, 0x19),
    fg_inverse: Color32::from_rgb(0xF6, 0xF5, 0xF2),
};

const DARK: Tokens = Tokens {
    fg_text: Color32::from_rgb(0xE6, 0xE3, 0xDE),
    fg_secondary: Color32::from_rgb(0x98, 0x93, 0x8A),
    border_strong: Color32::from_rgb(0x5A, 0x57, 0x50),
    border_soft: Color32::from_rgb(0x45, 0x43, 0x40),
    bg_hover: translucent(0xE6, 0xE3, 0xDE, 0x1F),
    bg_inverse: Color32::from_rgb(0xE6, 0xE3, 0xDE),
    fg_inverse: Color32::from_rgb(0x1C, 0x1B, 0x19),
};

/// The palette matching whatever theme egui currently resolves to.
pub(crate) fn current(ctx: &egui::Context) -> Tokens {
    match ctx.theme() {
        egui::Theme::Light => LIGHT,
        egui::Theme::Dark => DARK,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors `gascii::ui::theme`'s own translucency pin: the hover wash must stay translucent
    /// and properly premultiplied, or it renders brighter than intended.
    #[test]
    fn bg_hover_is_translucent_and_premultiplied_in_both_themes() {
        for t in [LIGHT, DARK] {
            let c = t.bg_hover;
            assert!(c.a() < 128, "bg_hover is not translucent (a={})", c.a());
            assert!(c.r() <= c.a() && c.g() <= c.a() && c.b() <= c.a(), "bg_hover channel exceeds its alpha");
        }
    }

    /// Drift tripwire for `REVIEW_plugin-api_2026-07-20.md` Suggestion 1: nothing previously would
    /// fail if the host's `gascii::ui::theme::Tokens` values changed without this vendored copy
    /// being updated to match. This crate cannot depend on `gascii` (that's the wrong direction of
    /// the plugin/host boundary — `gascii` depends on `gascii-density-brush`, not the reverse), and
    /// `gascii::ui::theme::Tokens` isn't reachable from here even if it could, so the two `Tokens`
    /// structs can never be imported side by side in one test. This pins the host's own literal
    /// values instead — copied by hand from `gascii/src/ui/theme.rs`'s `Tokens::LIGHT`/`DARK` as of
    /// this writing, for exactly the 7 fields this crate's widgets read — so an edit to EITHER
    /// side's palette without a matching edit to the other now fails a test rather than silently
    /// drifting. Residual limitation, accepted per the review's own scoping: this only catches
    /// drift that shows up as a mismatch against the pinned literals below; it cannot detect the
    /// host and this copy drifting to the same NEW (but still-matching) values, and it requires a
    /// human to update the pinned literals here if the host's palette ever legitimately changes.
    #[test]
    fn vendored_tokens_pin_the_hosts_gascii_ui_theme_literal_values() {
        // Mirrors gascii::ui::theme::Tokens::LIGHT.
        let host_light = Tokens {
            fg_text: Color32::from_rgb(0x1C, 0x1B, 0x19),
            fg_secondary: Color32::from_rgb(0x71, 0x6C, 0x63),
            border_strong: Color32::from_rgb(0x1C, 0x1B, 0x19),
            border_soft: Color32::from_rgb(0xC9, 0xC5, 0xBD),
            bg_hover: translucent(0x1C, 0x1B, 0x19, 0x17),
            bg_inverse: Color32::from_rgb(0x1C, 0x1B, 0x19),
            fg_inverse: Color32::from_rgb(0xF6, 0xF5, 0xF2),
        };
        // Mirrors gascii::ui::theme::Tokens::DARK.
        let host_dark = Tokens {
            fg_text: Color32::from_rgb(0xE6, 0xE3, 0xDE),
            fg_secondary: Color32::from_rgb(0x98, 0x93, 0x8A),
            border_strong: Color32::from_rgb(0x5A, 0x57, 0x50),
            border_soft: Color32::from_rgb(0x45, 0x43, 0x40),
            bg_hover: translucent(0xE6, 0xE3, 0xDE, 0x1F),
            bg_inverse: Color32::from_rgb(0xE6, 0xE3, 0xDE),
            fg_inverse: Color32::from_rgb(0x1C, 0x1B, 0x19),
        };
        for (name, expected, actual) in [("light", host_light, LIGHT), ("dark", host_dark, DARK)] {
            assert_eq!(actual.fg_text, expected.fg_text, "{name}: fg_text drifted from the host's copy");
            assert_eq!(actual.fg_secondary, expected.fg_secondary, "{name}: fg_secondary drifted from the host's copy");
            assert_eq!(actual.border_strong, expected.border_strong, "{name}: border_strong drifted from the host's copy");
            assert_eq!(actual.border_soft, expected.border_soft, "{name}: border_soft drifted from the host's copy");
            assert_eq!(actual.bg_hover, expected.bg_hover, "{name}: bg_hover drifted from the host's copy");
            assert_eq!(actual.bg_inverse, expected.bg_inverse, "{name}: bg_inverse drifted from the host's copy");
            assert_eq!(actual.fg_inverse, expected.fg_inverse, "{name}: fg_inverse drifted from the host's copy");
        }
    }
}
