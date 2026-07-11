use eframe::egui;

use crate::fonts::canvas_font_id;

const GLYPH_PX: f32 = 20.0;

/// Dev-only glyph coverage sheet
/// Missing glyphs render as tofu (notdef) — the sheet itself is the coverage check.
pub fn show(ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        section(ui, "ASCII printable (U+0020-U+007E)", (0x0020u32..=0x007E).collect());
        section(ui, "Box drawing (U+2500-U+257F)", (0x2500u32..=0x257F).collect());
        section(ui, "Blocks & shades (U+2580-U+259F)", (0x2580u32..=0x259F).collect());
        section(
            ui,
            "Braille sample (U+2800-U+28FF, every 8th codepoint)",
            (0x2800u32..=0x28FF).step_by(8).collect(),
        );
    });
}

fn section(ui: &mut egui::Ui, title: &str, codepoints: Vec<u32>) {
    ui.heading(title);
    let font_id = canvas_font_id(GLYPH_PX);
    ui.horizontal_wrapped(|ui| {
        for cp in codepoints {
            if let Some(ch) = char::from_u32(cp) {
                ui.label(egui::RichText::new(ch.to_string()).font(font_id.clone()));
            }
        }
    });
    ui.separator();
}

#[cfg(test)]
mod tests {
    // GUI-free backstop for the visual sheet: any curated codepoint missing from the embedded
    // TTF fails loudly here instead of only showing as tofu on screen.
    const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/IosevkaFixed-Regular.ttf");

    fn missing_glyphs(codepoints: impl Iterator<Item = u32>) -> Vec<u32> {
        let face = ttf_parser::Face::parse(FONT_BYTES, 0).expect("valid TTF");
        codepoints
            .filter(|&cp| {
                char::from_u32(cp)
                    .map(|ch| face.glyph_index(ch).is_none())
                    .unwrap_or(true)
            })
            .collect()
    }

    #[test]
    fn ascii_printable_has_full_coverage() {
        let missing = missing_glyphs(0x0020u32..=0x007E);
        assert!(missing.is_empty(), "missing ASCII glyphs: {missing:?}");
    }

    #[test]
    fn box_drawing_has_full_coverage() {
        let missing = missing_glyphs(0x2500u32..=0x257F);
        assert!(missing.is_empty(), "missing box-drawing glyphs: {missing:?}");
    }

    #[test]
    fn blocks_and_shades_have_full_coverage() {
        let missing = missing_glyphs(0x2580u32..=0x259F);
        assert!(missing.is_empty(), "missing block/shade glyphs: {missing:?}");
    }

    #[test]
    fn braille_sample_coverage_recorded() {
        // Braille is "future page" per FR-14 — this test records gaps rather than asserting none.
        let missing = missing_glyphs((0x2800u32..=0x28FF).step_by(8));
        if !missing.is_empty() {
            eprintln!(
                "Iosevka Fixed Braille sample gaps (informational, not a v1 requirement): {missing:?}"
            );
        }
    }
}
