use std::sync::{Arc, OnceLock};

use eframe::egui::{self, FontFamily, FontId, TextStyle};

/// Named family used EXCLUSIVELY for the canvas.
pub const CANVAS_FONT: &str = "iosevka-fixed";

/// Named families for the two UI weights above Regular. Instrument Sans is distributed only as a
/// variable font, and `ab_glyph` rasterizes a variable font's default instance with no axis
/// selection and no synthetic bold — so each weight ships as its own static cut and, to egui, as
/// its own family. `assets/fonts/README.md` records how the cuts are generated.
pub const UI_MEDIUM: &str = "ui-medium";
pub const UI_SEMIBOLD: &str = "ui-semibold";

/// The bundled canvas font's raw bytes, shared by the live egui canvas and `png_export.rs`'s
/// off-screen `fontdue` rasterizer — one embedded font asset, one source of truth.
pub(crate) const CANVAS_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/IosevkaFixed-Regular.ttf");

const UI_REGULAR_BYTES: &[u8] = include_bytes!("../assets/fonts/InstrumentSans-Regular.ttf");
const UI_MEDIUM_BYTES: &[u8] = include_bytes!("../assets/fonts/InstrumentSans-Medium.ttf");
const UI_SEMIBOLD_BYTES: &[u8] = include_bytes!("../assets/fonts/InstrumentSans-SemiBold.ttf");
const MONO_BYTES: &[u8] = include_bytes!("../assets/fonts/FragmentMono-Regular.ttf");

/// `Arc<str>` backing each `FontFamily::Name`, built once and cloned (cheap refcount bump) on every
/// id call instead of re-allocating a fresh `Arc<str>` each time.
static FAMILY_NAMES: OnceLock<[Arc<str>; 3]> = OnceLock::new();

fn family_name(which: usize) -> Arc<str> {
    FAMILY_NAMES.get_or_init(|| {
        [Arc::from(CANVAS_FONT), Arc::from(UI_MEDIUM), Arc::from(UI_SEMIBOLD)]
    })[which]
        .clone()
}

/// Registers every bundled face and pins the text styles to the design spec's sizes.
///
/// The UI faces go at the FRONT of egui's stock Proportional/Monospace families rather than
/// replacing them: the chrome draws symbols the two bundled faces don't carry (`⇄ ✓ ▲ ▼ × □`), and
/// keeping the defaults behind them preserves a fallback chain for those. The canvas family is the
/// deliberate exception — it stays single-font so every cell advances by exactly one glyph width.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    for (name, bytes) in [
        (CANVAS_FONT, CANVAS_FONT_BYTES),
        (UI_MEDIUM, UI_MEDIUM_BYTES),
        (UI_SEMIBOLD, UI_SEMIBOLD_BYTES),
        ("ui-regular", UI_REGULAR_BYTES),
        ("mono", MONO_BYTES),
    ] {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_static(bytes)));
    }

    // Iosevka goes on the TAIL of both chrome chains as a last-resort backstop. It is already
    // embedded, and its coverage is vast where the two design faces are narrow: `⇄` (the swap
    // control) exists in neither Instrument Sans nor Fragment Mono nor egui's stock faces, and `□`
    // (the maximize box) is missing from Instrument Sans. Being last, it is consulted only for
    // glyphs nothing ahead of it carries, so it never displaces the design's typefaces.
    for (family, primary) in [
        (FontFamily::Proportional, "ui-regular"),
        (FontFamily::Monospace, "mono"),
    ] {
        let chain = fonts.families.entry(family).or_default();
        chain.insert(0, primary.to_owned());
        chain.push(CANVAS_FONT.to_owned());
    }

    // The two heavier UI weights and the canvas face each get a dedicated Name family. The weights
    // still fall back to the stock proportional faces for symbols they lack; the canvas face
    // deliberately does not.
    let stock: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for (name, bytes_name) in [(UI_MEDIUM, UI_MEDIUM), (UI_SEMIBOLD, UI_SEMIBOLD)] {
        let mut chain = vec![bytes_name.to_owned()];
        chain.extend(stock.iter().cloned());
        fonts.families.insert(FontFamily::Name(Arc::from(name)), chain);
    }
    fonts
        .families
        .insert(FontFamily::Name(Arc::from(CANVAS_FONT)), vec![CANVAS_FONT.to_owned()]);

    ctx.set_fonts(fonts);

    // `all_styles_mut`, not `style_mut_of`: text styles don't vary by theme, and setting only the
    // active one would leave the other theme on egui's stock sizes after a swap.
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            // Menus and controls.
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            // Buttons, segments, strong labels.
            (TextStyle::Button, FontId::new(12.0, FontFamily::Proportional)),
            // Field labels.
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            // Status bar and other measurements.
            (TextStyle::Monospace, FontId::new(11.0, FontFamily::Monospace)),
            (TextStyle::Heading, FontId::new(13.0, ui_semibold_family())),
        ]
        .into();
    });
}

fn ui_semibold_family() -> FontFamily {
    FontFamily::Name(family_name(2))
}

// The three helpers below are the type ramp's call surface for the custom-painted widgets, which
// paint text at explicit sizes rather than through a `TextStyle`. They have no callers until that
// widget kit exists.
/// Instrument Sans Medium — segmented controls and palette tabs.
#[allow(dead_code)]
pub fn ui_medium_id(px: f32) -> FontId {
    FontId::new(px, FontFamily::Name(family_name(1)))
}

/// Instrument Sans SemiBold — titles only.
#[allow(dead_code)]
pub fn ui_semibold_id(px: f32) -> FontId {
    FontId::new(px, ui_semibold_family())
}

/// Fragment Mono — anything that is content or measurement.
#[allow(dead_code)]
pub fn mono_id(px: f32) -> FontId {
    FontId::new(px, FontFamily::Monospace)
}

pub fn canvas_font_id(px: f32) -> FontId {
    FontId::new(px, FontFamily::Name(family_name(0)))
}
