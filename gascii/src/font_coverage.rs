//! Glyph coverage backstop for the bundled fonts: any curated codepoint missing from the embedded
//! TTFs fails loudly here instead of only showing as tofu (notdef) at runtime.
//!
//! The canvas font is checked alone because its family has no fallback chain — a gap there IS a
//! tofu. The UI faces sit in front of egui's stock families, so a gap only costs the intended
//! typeface, not the glyph; they are checked for the symbols the chrome actually draws.

#[cfg(test)]
mod tests {
    const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/IosevkaFixed-Regular.ttf");
    const UI_REGULAR: &[u8] = include_bytes!("../assets/fonts/InstrumentSans-Regular.ttf");
    const UI_MEDIUM: &[u8] = include_bytes!("../assets/fonts/InstrumentSans-Medium.ttf");
    const UI_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/InstrumentSans-SemiBold.ttf");
    const MONO: &[u8] = include_bytes!("../assets/fonts/FragmentMono-Regular.ttf");

    fn missing_from(bytes: &[u8], codepoints: impl Iterator<Item = u32>) -> Vec<u32> {
        let face = ttf_parser::Face::parse(bytes, 0).expect("valid TTF");
        codepoints
            .filter(|&cp| {
                char::from_u32(cp)
                    .map(|ch| face.glyph_index(ch).is_none())
                    .unwrap_or(true)
            })
            .collect()
    }

    fn missing_glyphs(codepoints: impl Iterator<Item = u32>) -> Vec<u32> {
        missing_from(FONT_BYTES, codepoints)
    }

    /// The UI faces must at least carry printable ASCII — every label, menu item and readout is
    /// built from it, and falling back mid-word would mix typefaces inside one string.
    #[test]
    fn ui_faces_cover_printable_ascii() {
        for (name, bytes) in [
            ("Instrument Sans Regular", UI_REGULAR),
            ("Instrument Sans Medium", UI_MEDIUM),
            ("Instrument Sans SemiBold", UI_SEMIBOLD),
            ("Fragment Mono", MONO),
        ] {
            let missing = missing_from(bytes, 0x0020u32..=0x007E);
            assert!(missing.is_empty(), "{name} is missing ASCII glyphs: {missing:?}");
        }
    }

    /// The non-ASCII symbols the chrome draws. Not asserted against a single face — they may come
    /// from any face in the fallback chain — but recorded, so a redesign that leans on a new symbol
    /// finds out here which faces carry it.
    #[test]
    fn chrome_symbol_coverage_recorded() {
        const SYMBOLS: &[(char, &str)] = &[
            ('\u{21C4}', "swap FG/BG"),
            ('\u{2713}', "checkbox tick"),
            ('\u{25B2}', "stepper up"),
            ('\u{25BC}', "stepper down"),
            ('\u{2190}', "anchor left"),
            ('\u{2192}', "anchor right"),
            ('\u{2191}', "anchor up"),
            ('\u{2193}', "anchor down"),
            ('\u{2013}', "minimize box"),
            ('\u{25A1}', "maximize box"),
            ('\u{00D7}', "close box"),
        ];
        for &(ch, role) in SYMBOLS {
            let carriers: Vec<&str> = [
                ("Instrument Sans Regular", UI_REGULAR),
                ("Fragment Mono", MONO),
                ("Iosevka Fixed", FONT_BYTES),
            ]
            .iter()
            .filter(|(_, bytes)| missing_from(bytes, std::iter::once(ch as u32)).is_empty())
            .map(|(name, _)| *name)
            .collect();
            eprintln!("{ch:?} ({role}): carried by {carriers:?}");
        }
    }

    /// Pins that the three Instrument Sans cuts really are three weights. `varLib.instancer` on the
    /// wrong axis, or three copies of the same instance, would leave the chrome flat with no other
    /// symptom — the files would still parse and render.
    #[test]
    fn instrument_sans_cuts_are_three_distinct_weights() {
        let weights: Vec<u16> = [UI_REGULAR, UI_MEDIUM, UI_SEMIBOLD]
            .iter()
            .map(|bytes| ttf_parser::Face::parse(bytes, 0).expect("valid TTF").weight().to_number())
            .collect();
        assert_eq!(weights, vec![400, 500, 600], "expected Regular/Medium/SemiBold");
    }

    /// Every glyph a user can draw must be in the canvas face. For this family the cmap IS the
    /// resolution: it holds exactly one font and no fallback, so a cmap miss is a guaranteed tofu.
    ///
    /// (`epaint`'s own `Fonts::has_glyph` cannot be used to check this. It is implemented as
    /// `resolve_face(c) != cached_family.replacement_face_key`, and in a single-font family the
    /// replacement face *is* the only face — so it answers `false` for every char, including ones
    /// that render perfectly. Its source carries a `TODO` acknowledging the false negative.)
    #[test]
    fn every_palette_and_ramp_glyph_is_in_the_canvas_face() {
        let mut missing: Vec<(String, char)> = Vec::new();
        for page in gascii_core::builtin_pages() {
            for ch in page.glyphs {
                if !missing_glyphs(std::iter::once(ch as u32)).is_empty() {
                    missing.push((page.name.to_owned(), ch));
                }
            }
        }
        for ramp in gascii_core::builtin_ramps() {
            for ch in ramp.chars {
                if !missing_glyphs(std::iter::once(ch as u32)).is_empty() {
                    missing.push((format!("ramp {}", ramp.name), ch));
                }
            }
        }
        assert!(missing.is_empty(), "glyphs that would render as tofu on the canvas: {missing:?}");
    }

    /// Every symbol the chrome draws must be carried by at least one bundled face. Both chrome
    /// chains end in Iosevka, so a symbol present in any of the three resolves — this is what that
    /// backstop is for, and `⇄` is why it exists (no other bundled or stock face has it).
    #[test]
    fn every_chrome_symbol_is_carried_by_some_bundled_face() {
        let uncarried: Vec<char> = "⇄✓▲▼←→↑↓–□×"
            .chars()
            .filter(|&ch| {
                [UI_REGULAR, MONO, FONT_BYTES]
                    .iter()
                    .all(|bytes| !missing_from(bytes, std::iter::once(ch as u32)).is_empty())
            })
            .collect();
        assert!(uncarried.is_empty(), "chrome symbols carried by no bundled face: {uncarried:?}");
    }

    /// A variable font would rasterize as its default instance only — `ab_glyph` has no axis
    /// selection — so shipping one by mistake would silently flatten every weight to Regular.
    #[test]
    fn instrument_sans_cuts_are_static_not_variable() {
        for (name, bytes) in [
            ("Regular", UI_REGULAR),
            ("Medium", UI_MEDIUM),
            ("SemiBold", UI_SEMIBOLD),
        ] {
            let face = ttf_parser::Face::parse(bytes, 0).expect("valid TTF");
            assert!(!face.is_variable(), "InstrumentSans-{name} is still a variable font");
        }
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
