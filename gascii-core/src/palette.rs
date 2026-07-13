//! Curated character Pages and the single-width validation choke point.

use unicode_width::UnicodeWidthChar;

use crate::model::DocSettings;

#[derive(Clone, Debug)]
pub struct Page {
    pub name: &'static str,
    pub glyphs: Vec<char>,
    pub ascii: bool,
}

/// Why a character was rejected from entering a Document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WidthReject {
    Control,
    ZeroWidth,
    DoubleWidth,
}

/// Every character-entry path must call this before storing a glyph. `char::width()` gives `None`
/// for control chars, `Some(0)` for combining/zero-width marks (easy to miss if you only check
/// `!= 1`), `Some(1)` for single-width, `Some(2)` for wide (CJK/emoji). Only `Some(1)` is accepted.
pub fn validate_width(ch: char) -> Result<(), WidthReject> {
    match ch.width() {
        None => Err(WidthReject::Control),
        Some(0) => Err(WidthReject::ZeroWidth),
        Some(1) => Ok(()),
        Some(_) => Err(WidthReject::DoubleWidth),
    }
}

/// Why a character was rejected from entering a Document under the combined choke point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryReject {
    Width(WidthReject),
    NonAscii,
}

/// The single choke point every character-entry path calls: width validation first, then the
/// document's strict-ASCII setting. Text entry (and, later, paste/import) call this and nothing
/// else.
pub fn allowed_in(ch: char, settings: &DocSettings) -> Result<(), EntryReject> {
    validate_width(ch).map_err(EntryReject::Width)?;
    if settings.strict_ascii && (ch as u32) > 0x007F {
        return Err(EntryReject::NonAscii);
    }
    Ok(())
}

/// Whether `page` should be selectable given the document's strict-ASCII setting.
pub fn page_available(page: &Page, settings: &DocSettings) -> bool {
    !settings.strict_ascii || page.ascii
}

/// Built-in Pages, all single-width and covered by the bundled canvas font (backstopped by the
/// glyph-coverage tests).
pub fn builtin_pages() -> Vec<Page> {
    let ascii: Vec<char> = (0x0020u32..=0x007E).filter_map(char::from_u32).collect();
    // The full Box Drawing block: single/double/heavy lines, rounded corners, dashes, diagonals.
    // The rectangle/line tools' auto-join only understands the single-line subset; the rest are
    // for direct placement.
    let box_drawing: Vec<char> = (0x2500u32..=0x257F).filter_map(char::from_u32).collect();
    let blocks_shades: Vec<char> = "░▒▓█▀▄▌▐".chars().collect();

    vec![
        Page { name: "ASCII", glyphs: ascii, ascii: true },
        Page { name: "Box Drawing", glyphs: box_drawing, ascii: false },
        Page { name: "Blocks & Shades", glyphs: blocks_shades, ascii: false },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_width_accepts_common_single_width_chars() {
        assert_eq!(validate_width(' '), Ok(()));
        assert_eq!(validate_width('A'), Ok(()));
        assert_eq!(validate_width('│'), Ok(()));
        assert_eq!(validate_width('█'), Ok(()));
    }

    #[test]
    fn validate_width_rejects_control_chars() {
        assert_eq!(validate_width('\t'), Err(WidthReject::Control));
        assert_eq!(validate_width('\n'), Err(WidthReject::Control));
    }

    #[test]
    fn validate_width_rejects_zero_width_combining_and_marks() {
        assert_eq!(validate_width('\u{0301}'), Err(WidthReject::ZeroWidth)); // combining acute
        assert_eq!(validate_width('\u{200D}'), Err(WidthReject::ZeroWidth)); // ZWJ
        assert_eq!(validate_width('\u{FE0F}'), Err(WidthReject::ZeroWidth)); // variation selector-16
    }

    #[test]
    fn validate_width_rejects_double_width_chars() {
        assert_eq!(validate_width('你'), Err(WidthReject::DoubleWidth));
        assert_eq!(validate_width('あ'), Err(WidthReject::DoubleWidth));
        assert_eq!(validate_width('😀'), Err(WidthReject::DoubleWidth));
    }

    #[test]
    fn every_builtin_page_glyph_passes_validate_width() {
        for page in builtin_pages() {
            for &ch in &page.glyphs {
                assert!(
                    validate_width(ch).is_ok(),
                    "page {:?} contains an invalid-width glyph: {ch:?}",
                    page.name
                );
            }
        }
    }

    #[test]
    fn page_ascii_flag_is_consistent_with_glyph_codepoints() {
        for page in builtin_pages() {
            let all_ascii = page.glyphs.iter().all(|&ch| (ch as u32) <= 0x007F);
            assert_eq!(
                page.ascii, all_ascii,
                "page {:?} ascii flag does not match its glyph codepoints",
                page.name
            );
        }
    }

    #[test]
    fn ascii_page_has_95_glyphs() {
        let pages = builtin_pages();
        let ascii_page = pages.iter().find(|p| p.name == "ASCII").unwrap();
        assert_eq!(ascii_page.glyphs.len(), 95);
    }

    #[test]
    fn box_drawing_page_spans_the_entire_box_drawing_block() {
        let pages = builtin_pages();
        let box_page = pages.iter().find(|p| p.name == "Box Drawing").unwrap();
        assert_eq!(box_page.glyphs.len(), 128);
        for cp in 0x2500u32..=0x257F {
            let ch = char::from_u32(cp).unwrap();
            assert!(box_page.glyphs.contains(&ch), "missing box-drawing glyph: {ch:?} (U+{cp:04X})");
        }
    }

    #[test]
    fn box_and_block_pages_are_non_empty() {
        let pages = builtin_pages();
        for name in ["Box Drawing", "Blocks & Shades"] {
            let page = pages.iter().find(|p| p.name == name).unwrap();
            assert!(!page.glyphs.is_empty(), "page {name:?} must not be empty");
        }
    }

    fn settings(strict_ascii: bool) -> DocSettings {
        DocSettings { strict_ascii }
    }

    #[test]
    fn allowed_in_accepts_ascii_glyph_regardless_of_strict_mode() {
        assert_eq!(allowed_in('A', &settings(false)), Ok(()));
        assert_eq!(allowed_in('A', &settings(true)), Ok(()));
    }

    #[test]
    fn allowed_in_accepts_non_ascii_glyph_when_not_strict() {
        assert_eq!(allowed_in('│', &settings(false)), Ok(()));
    }

    #[test]
    fn allowed_in_rejects_non_ascii_glyph_when_strict() {
        assert_eq!(allowed_in('│', &settings(true)), Err(EntryReject::NonAscii));
    }

    #[test]
    fn allowed_in_rejects_width_invalid_glyph_regardless_of_strict_mode() {
        assert_eq!(
            allowed_in('😀', &settings(false)),
            Err(EntryReject::Width(WidthReject::DoubleWidth))
        );
        assert_eq!(
            allowed_in('😀', &settings(true)),
            Err(EntryReject::Width(WidthReject::DoubleWidth))
        );
    }

    #[test]
    fn page_available_ascii_page_always_available() {
        let pages = builtin_pages();
        let ascii_page = pages.iter().find(|p| p.name == "ASCII").unwrap();
        assert!(page_available(ascii_page, &settings(false)));
        assert!(page_available(ascii_page, &settings(true)));
    }

    #[test]
    fn page_available_non_ascii_pages_only_when_not_strict() {
        let pages = builtin_pages();
        for name in ["Box Drawing", "Blocks & Shades"] {
            let page = pages.iter().find(|p| p.name == name).unwrap();
            assert!(page_available(page, &settings(false)));
            assert!(!page_available(page, &settings(true)));
        }
    }
}
