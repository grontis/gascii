//! The design system's color tokens and their mapping onto egui's `Visuals`.
//!
//! Every interactable in the chrome follows one four-state contract:
//!
//! - **Idle**: transparent fill, per-widget border.
//! - **Hover**: `bg_hover` wash, `border_strong` outline. Never a border-only darken — a hover
//!   that only moves the border is easy to miss; the wash makes "this responds to you" legible
//!   without reading as selection.
//! - **Pressed & selected**: inversion (`bg_inverse`/`fg_inverse`). No accent-colored fills
//!   anywhere in the chrome.
//! - **Disabled**: `fg_secondary` text, `border_soft`, no hover reaction at all.
//!
//! This is expressed in exactly two places: [`Tokens::visuals`] (stock widgets, menus, dialog
//! internals) and [`widgets::cell`](super::widgets) (the whole custom kit). Any new widget should
//! read one of those two rather than inventing its own state colors.
//!
//! **The accent belongs to the canvas.** [`CANVAS_ACCENT`] and [`CANVAS_SURFACE`] are deliberately
//! not part of [`Tokens`]: the document is not chrome, and its surface does not follow the chrome
//! theme. They are the same in light and dark.

use eframe::egui::{self, Color32, CornerRadius, Shadow, Stroke, Theme, Visuals};

/// The New dialog's starting background-well value, matching `Document::new`'s own default
/// (opaque black). A document's actual background lives on the `Document` itself
/// (`gascii_core::Document::background`) and is set once at creation — this constant is only the
/// dialog's initial swatch, not read anywhere a live document's background matters.
pub const CANVAS_SURFACE: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);

/// Canvas overlays only: marquee, cell cursor, size tags. The single non-monochrome color in the
/// app, and it never appears in the chrome.
pub const CANVAS_ACCENT: Color32 = Color32::from_rgb(0x7F, 0xA8, 0xD9);

/// Depth comes only from hard offset shadows — no blur, ever.
const SHADOW_OFFSET: [i8; 2] = [3, 3];

/// A translucent colour from straight (un-premultiplied) components.
///
/// `Color32` stores premultiplied alpha, and its `from_rgba_unmultiplied` is not `const`, so the
/// tokens below cannot call it. Handing full-brightness RGB straight to `from_rgba_premultiplied`
/// compiles fine and is wrong: those channels are treated as already scaled, and the colour renders
/// near-opaque — pinstripes as solid bars, the card shadow as a hard slab.
const fn translucent(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        (r as u16 * a as u16 / 255) as u8,
        (g as u16 * a as u16 / 255) as u8,
        (b as u16 * a as u16 / 255) as u8,
        a,
    )
}

/// One theme's palette. Every chrome color in the app resolves to a field here.
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    /// Window body.
    pub bg_chrome: Color32,
    /// Title/menu/status bars, sidebar, dialogs.
    pub bg_panel: Color32,
    /// The canvas area behind the document.
    pub bg_desk: Color32,
    /// Primary text and icons.
    pub fg_text: Color32,
    /// Labels, hints, inactive badges.
    pub fg_secondary: Color32,
    /// Structural borders and control outlines.
    pub border_strong: Color32,
    /// Separators and idle swatch borders.
    pub border_soft: Color32,
    /// Hover wash: a translucent tint over whatever surface a hovered control sits on. The one
    /// fill state below inversion — never opaque, or it would read as selection.
    pub bg_hover: Color32,
    /// The fill of a selected state.
    pub bg_inverse: Color32,
    /// Text/icons drawn on `bg_inverse`.
    pub fg_inverse: Color32,
    /// The outer window and dialog border.
    pub window_edge: Color32,
    /// Hard offset shadow under floating surfaces.
    pub shadow: Color32,
    /// Title-bar pinstripe lines, already at their intended opacity.
    pub pinstripe: Color32,
    /// Error text. The one non-monochrome chrome colour: an error must not read as ordinary
    /// telemetry, inversion already means selection, and the accent belongs to the canvas — so
    /// errors get their own warm red, toned to each theme.
    pub fg_error: Color32,
}

impl Tokens {
    pub const LIGHT: Self = Self {
        bg_chrome: Color32::from_rgb(0xEC, 0xE9, 0xE4),
        bg_panel: Color32::from_rgb(0xF6, 0xF5, 0xF2),
        bg_desk: Color32::from_rgb(0xDD, 0xD9, 0xD2),
        fg_text: Color32::from_rgb(0x1C, 0x1B, 0x19),
        fg_secondary: Color32::from_rgb(0x71, 0x6C, 0x63),
        border_strong: Color32::from_rgb(0x1C, 0x1B, 0x19),
        border_soft: Color32::from_rgb(0xC9, 0xC5, 0xBD),
        bg_hover: translucent(0x1C, 0x1B, 0x19, 0x17),
        bg_inverse: Color32::from_rgb(0x1C, 0x1B, 0x19),
        fg_inverse: Color32::from_rgb(0xF6, 0xF5, 0xF2),
        window_edge: Color32::from_rgb(0x1C, 0x1B, 0x19),
        shadow: translucent(0x1C, 0x1B, 0x19, 0x2E),
        pinstripe: translucent(0x1C, 0x1B, 0x19, 0x47),
        fg_error: Color32::from_rgb(0x9E, 0x2B, 0x25),
    };

    pub const DARK: Self = Self {
        bg_chrome: Color32::from_rgb(0x26, 0x25, 0x24),
        bg_panel: Color32::from_rgb(0x2E, 0x2D, 0x2B),
        bg_desk: Color32::from_rgb(0x1D, 0x1C, 0x1B),
        fg_text: Color32::from_rgb(0xE6, 0xE3, 0xDE),
        fg_secondary: Color32::from_rgb(0x98, 0x93, 0x8A),
        border_strong: Color32::from_rgb(0x5A, 0x57, 0x50),
        border_soft: Color32::from_rgb(0x45, 0x43, 0x40),
        bg_hover: translucent(0xE6, 0xE3, 0xDE, 0x1F),
        bg_inverse: Color32::from_rgb(0xE6, 0xE3, 0xDE),
        fg_inverse: Color32::from_rgb(0x1C, 0x1B, 0x19),
        window_edge: Color32::from_rgb(0x06, 0x06, 0x06),
        shadow: translucent(0x00, 0x00, 0x00, 0x66),
        pinstripe: translucent(0xE6, 0xE3, 0xDE, 0x2E),
        fg_error: Color32::from_rgb(0xE0, 0x6C, 0x5E),
    };

    pub fn of(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self::LIGHT,
            Theme::Dark => Self::DARK,
        }
    }

    /// This theme's palette expressed as egui `Visuals`, for the chrome egui paints itself: the
    /// menu bar, popups, dialogs, and the handful of stock widgets kept as-is. Custom-painted
    /// widgets read [`Tokens`] directly and ignore all of this.
    fn visuals(&self, theme: Theme) -> Visuals {
        let mut v = match theme {
            Theme::Light => Visuals::light(),
            Theme::Dark => Visuals::dark(),
        };

        v.panel_fill = self.bg_panel;
        v.window_fill = self.bg_chrome;
        v.window_stroke = Stroke::new(1.0, self.window_edge);
        v.faint_bg_color = self.border_soft;
        v.extreme_bg_color = self.bg_panel;
        // `override_text_color` stays None on purpose. It forces every label to one color, which
        // defeats the per-state `fg_stroke` that inversion depends on — a selected control would
        // paint `fg_text` on `bg_inverse`, i.e. the text color on the text color, and every
        // selected tool/tab/swatch would render as a blank filled block.
        v.override_text_color = None;

        // Square everywhere: no corner rounds anywhere in the chrome.
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;

        let shadow = Shadow { offset: SHADOW_OFFSET, blur: 0, spread: 0, color: self.shadow };
        v.window_shadow = shadow;
        v.popup_shadow = shadow;

        // Monochrome selection — the accent is reserved for the canvas.
        v.selection.bg_fill = self.bg_inverse;
        v.selection.stroke = Stroke::new(1.0, self.fg_inverse);
        v.hyperlink_color = self.fg_text;

        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = CornerRadius::ZERO;
            // Widgets must not grow on hover: a 1px box that swells by a pixel reads as a wobble
            // against a flat, square design.
            w.expansion = 0.0;
            w.bg_stroke.width = 1.0;
            w.fg_stroke.width = 1.0;
        }

        // Transparent fills with a 1px border; hover darkens the border rather than filling.
        v.widgets.noninteractive.bg_fill = self.bg_panel;
        v.widgets.noninteractive.weak_bg_fill = self.bg_panel;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.border_soft);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.fg_text);

        v.widgets.inactive.bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, self.border_soft);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, self.fg_text);

        v.widgets.hovered.bg_fill = self.bg_hover;
        v.widgets.hovered.weak_bg_fill = self.bg_hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, self.fg_text);

        // Pressed/selected inverts — the core selection rule.
        v.widgets.active.bg_fill = self.bg_inverse;
        v.widgets.active.weak_bg_fill = self.bg_inverse;
        v.widgets.active.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.active.fg_stroke = Stroke::new(1.0, self.fg_inverse);

        // An open menu button reads as engaged, the same wash a hover gets.
        v.widgets.open.bg_fill = self.bg_hover;
        v.widgets.open.weak_bg_fill = self.bg_hover;
        v.widgets.open.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.open.fg_stroke = Stroke::new(1.0, self.fg_text);

        v
    }
}

/// Registers both themes' visuals up front, so switching is `ctx.set_theme(preference)` and egui
/// picks the matching `Style` itself — including following the OS under `ThemePreference::System`.
pub fn install(ctx: &egui::Context) {
    for theme in [Theme::Light, Theme::Dark] {
        let visuals = Tokens::of(theme).visuals(theme);
        ctx.style_mut_of(theme, |style| style.visuals = visuals);
    }
}

/// The palette matching whatever theme egui currently resolves to.
pub fn current(ctx: &egui::Context) -> Tokens {
    Tokens::of(ctx.theme())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pinstripes and card shadow are decorative washes; they must stay translucent. Passing
    /// straight RGB to `from_rgba_premultiplied` compiles and renders them near-opaque, which is a
    /// visual-only failure — nothing else would catch it.
    #[test]
    fn translucent_tokens_are_actually_translucent() {
        for (name, t) in [("light", Tokens::LIGHT), ("dark", Tokens::DARK)] {
            for (what, c) in [("shadow", t.shadow), ("pinstripe", t.pinstripe)] {
                assert!(c.a() < 128, "{name} {what} is not translucent (a={})", c.a());
                // Premultiplied: no channel may exceed the alpha, or it is not a valid
                // premultiplied colour and will render brighter than intended.
                assert!(
                    c.r() <= c.a() && c.g() <= c.a() && c.b() <= c.a(),
                    "{name} {what} has a channel above its alpha — RGB was not premultiplied"
                );
            }
        }
    }

    /// The hover wash must actually be visible (nonzero alpha) and stay translucent (below
    /// selection's opacity) in both themes, and — like the other translucent tokens — its RGB must
    /// not exceed its own premultiplied alpha.
    #[test]
    fn hover_wash_is_visible_and_translucent() {
        for (name, t) in [("light", Tokens::LIGHT), ("dark", Tokens::DARK)] {
            let a = t.bg_hover.a();
            assert!(a > 0, "{name}: hover wash is fully transparent, invisible");
            assert!(a < 96, "{name}: hover wash is too opaque, would read as selection");
            assert!(
                t.bg_hover.r() <= a && t.bg_hover.g() <= a && t.bg_hover.b() <= a,
                "{name}: hover wash has a channel above its alpha — RGB was not premultiplied"
            );
        }
    }

    /// The document does not follow the chrome theme — its surface is a property of the document,
    /// not of the window it's shown in.
    #[test]
    fn canvas_colors_are_theme_independent() {
        assert_eq!(CANVAS_SURFACE, Color32::from_rgb(0, 0, 0));
        assert_eq!(CANVAS_ACCENT, Color32::from_rgb(0x7F, 0xA8, 0xD9));
    }

    /// Perceived brightness, 0.0–1.0. Rough Rec. 601 weighting — enough to tell ink from paper,
    /// which is all these tests ask of it.
    fn luminance(c: Color32) -> f32 {
        (0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32) / 255.0
    }

    /// Inversion IS the selection mechanism, so it has to actually invert: the selected fill is the
    /// theme's ink, and the text drawn on it must contrast hard against that fill. Getting this
    /// backwards in one theme yields black-on-black selected controls.
    #[test]
    fn inversion_swaps_ink_and_paper_in_both_themes() {
        for (name, t) in [("light", Tokens::LIGHT), ("dark", Tokens::DARK)] {
            assert_eq!(t.bg_inverse, t.fg_text, "{name}: the selected fill is the text color");
            let contrast = (luminance(t.fg_inverse) - luminance(t.bg_inverse)).abs();
            assert!(contrast > 0.5, "{name}: inverse pair contrast is only {contrast:.2}");
        }
    }

    /// The two themes must genuinely be light and dark — a token table copy-pasted between them is
    /// otherwise silent, since every contrast test above would still pass.
    #[test]
    fn light_is_light_and_dark_is_dark() {
        assert!(luminance(Tokens::LIGHT.bg_panel) > 0.8, "light panel is not light");
        assert!(luminance(Tokens::DARK.bg_panel) < 0.3, "dark panel is not dark");
        assert!(luminance(Tokens::LIGHT.fg_text) < 0.3, "light text is not ink");
        assert!(luminance(Tokens::DARK.fg_text) > 0.8, "dark text is not paper");
    }

    /// Depth comes only from hard offset shadows. A blur would reintroduce exactly the soft depth
    /// the design rules out.
    #[test]
    fn shadows_are_hard_offsets_with_no_blur() {
        for theme in [Theme::Light, Theme::Dark] {
            let v = Tokens::of(theme).visuals(theme);
            for shadow in [v.window_shadow, v.popup_shadow] {
                assert_eq!(shadow.blur, 0, "{theme:?}: shadows must not blur");
                assert_eq!(shadow.spread, 0, "{theme:?}: shadows must not spread");
                assert_eq!(shadow.offset, SHADOW_OFFSET);
            }
        }
    }

    /// "No rounded corners above 0px" and "widgets do not expand" are two of the flat/square rules
    /// that are easiest to lose to an egui upgrade changing a default.
    #[test]
    fn every_widget_state_is_square_and_non_expanding() {
        for theme in [Theme::Light, Theme::Dark] {
            let v = Tokens::of(theme).visuals(theme);
            for (i, w) in [
                v.widgets.noninteractive,
                v.widgets.inactive,
                v.widgets.hovered,
                v.widgets.active,
                v.widgets.open,
            ]
            .iter()
            .enumerate()
            {
                assert_eq!(w.corner_radius, CornerRadius::ZERO, "{theme:?} widget {i} is rounded");
                assert_eq!(w.expansion, 0.0, "{theme:?} widget {i} expands");
            }
            assert_eq!(v.window_corner_radius, CornerRadius::ZERO);
            assert_eq!(v.menu_corner_radius, CornerRadius::ZERO);
        }
    }

    /// Inversion works by giving each widget state its own `fg_stroke`. `override_text_color`
    /// forces one color onto every label and silently defeats that — selected controls render as
    /// blank filled blocks with no text. Observed for real; pinned so it cannot come back.
    #[test]
    fn text_color_is_never_globally_overridden() {
        for theme in [Theme::Light, Theme::Dark] {
            let v = Tokens::of(theme).visuals(theme);
            assert!(
                v.override_text_color.is_none(),
                "{theme:?}: a global text color would blank out every selected control"
            );
        }
    }

    /// The selected state must be legible: egui's `interact_selectable` paints selected text with
    /// `selection.stroke` on `selection.bg_fill`, so those two specifically have to contrast.
    #[test]
    fn selected_widgets_are_legible() {
        for theme in [Theme::Light, Theme::Dark] {
            let v = Tokens::of(theme).visuals(theme);
            let contrast =
                (luminance(v.selection.stroke.color) - luminance(v.selection.bg_fill)).abs();
            assert!(contrast > 0.5, "{theme:?}: selected text contrast is only {contrast:.2}");
        }
    }

    /// The accent is reserved for canvas overlays. If it ever leaks into a `Visuals` field, the
    /// chrome has stopped being monochrome — egui's stock blue selection is exactly how that
    /// happens by default.
    #[test]
    fn chrome_visuals_never_use_the_canvas_accent() {
        for theme in [Theme::Light, Theme::Dark] {
            let v = Tokens::of(theme).visuals(theme);
            let used = [
                v.selection.bg_fill,
                v.selection.stroke.color,
                v.hyperlink_color,
                v.panel_fill,
                v.window_fill,
                v.faint_bg_color,
                v.widgets.active.bg_fill,
                v.widgets.hovered.bg_stroke.color,
                v.widgets.hovered.bg_fill,
            ];
            assert!(
                !used.contains(&CANVAS_ACCENT),
                "{theme:?}: the canvas accent leaked into the chrome"
            );
        }
    }
}
