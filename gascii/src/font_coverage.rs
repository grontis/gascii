//! Glyph coverage backstop for the bundled canvas font: any curated codepoint missing from the
//! embedded TTF fails loudly here instead of only showing as tofu (notdef) on the canvas.

#[cfg(test)]
mod tests {
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
        // Braille is not a curated palette page — this test records gaps rather than asserting none.
        let missing = missing_glyphs((0x2800u32..=0x28FF).step_by(8));
        if !missing.is_empty() {
            eprintln!(
                "Iosevka Fixed Braille sample gaps (informational): {missing:?}"
            );
        }
    }
}
