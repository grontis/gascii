use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);
impl Rgba {
    pub const WHITE: Rgba = Rgba(255, 255, 255, 255);
    pub const TRANSPARENT: Rgba = Rgba(0, 0, 0, 0);
    pub const fn is_transparent(&self) -> bool {
        self.3 == 0
    }
}

/// Parses `"#RRGGBBAA"` (case-insensitive), requiring exactly 8 hex digits after the leading `#`.
/// Parses the whole 8-character span as one `u32` rather than byte-slicing fixed 2-byte cut
/// points: a crafted multi-byte-UTF-8 string can total exactly 8 *bytes* while its char
/// boundaries don't land on those cut points, which would otherwise panic on a mid-character
/// slice. `from_str_radix` walks `hex` char-by-char and simply rejects any non-hex-digit
/// character (including multi-byte ones) instead of panicking. Every byte is checked against
/// `is_ascii_hexdigit` up front, since `from_str_radix` otherwise treats a leading `'+'` as a
/// sign to strip rather than an invalid digit, silently accepting a 7-hex-digit value one byte
/// short of this format's own 8-digit contract.
fn parse_hex_rgba(s: &str) -> Option<Rgba> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let [r, g, b, a] = value.to_be_bytes();
    Some(Rgba(r, g, b, a))
}

impl Serialize for Rgba {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("#{:02X}{:02X}{:02X}{:02X}", self.0, self.1, self.2, self.3))
    }
}
impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_hex_rgba(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color {s:?}, expected #RRGGBBAA")))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgba,
    pub bg: Rgba,
}
impl Cell {
    /// Canonical empty cell: space glyph + fully transparent bg
    pub const BLANK: Cell = Cell {
        ch: ' ',
        fg: Rgba::WHITE,
        bg: Rgba::TRANSPARENT,
    };
    pub fn is_blank(&self) -> bool {
        self.ch == ' ' && self.bg.3 == 0
    }
}
impl Default for Cell {
    fn default() -> Self {
        Cell::BLANK
    }
}

/// One full-canvas sheet of Cells, row-major, length == width*height. `cells` stays private so
/// all indexing goes through Document (which owns the dimensions).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Layer {
    cells: Vec<Cell>,
}
impl Layer {
    pub fn blank(width: u16, height: u16) -> Self {
        Layer {
            cells: vec![Cell::BLANK; width as usize * height as usize],
        }
    }
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DocSettings {
    pub strict_ascii: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DocExtent {
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Document {
    pub width: u16,
    pub height: u16,
    pub layers: Vec<Layer>,
    pub settings: DocSettings,
}
impl Document {
    pub const DEFAULT_WIDTH: u16 = 80;
    pub const DEFAULT_HEIGHT: u16 = 25;
    /// Sane upper bound on canvas extent, matching the size the app is designed to remain usable
    /// at. Shared by every caller that must validate an untrusted width/height *before*
    /// allocating anything sized by it (currently: the `.gascii` loader) — a single definition so
    /// that bound can never drift out of sync with the value the rest of the app assumes.
    pub const MAX_WIDTH: u16 = 1024;
    pub const MAX_HEIGHT: u16 = 1024;
    /// Sane upper bound on layer count for the same untrusted-input-validation reason as
    /// `MAX_WIDTH`/`MAX_HEIGHT` — generous enough that no real document gets close to it (today's
    /// app never writes more than one layer), tight enough that a file can't force an unbounded
    /// number of full-size blank layers to be allocated before any per-row shape check runs.
    pub const MAX_LAYERS: usize = 256;

    pub fn new(width: u16, height: u16) -> Self {
        assert!(width > 0 && height > 0, "canvas must be non-empty");
        Document {
            width,
            height,
            layers: vec![Layer::blank(width, height)],
            settings: DocSettings::default(),
        }
    }
    /// Default new document: 80×25.
    pub fn default_document() -> Self {
        Self::new(Self::DEFAULT_WIDTH, Self::DEFAULT_HEIGHT)
    }

    pub fn extent(&self) -> DocExtent {
        DocExtent {
            width: self.width,
            height: self.height,
        }
    }
    pub fn in_bounds(&self, x: u16, y: u16) -> bool {
        x < self.width && y < self.height
    }
    #[inline]
    fn index(&self, x: u16, y: u16) -> usize {
        y as usize * self.width as usize + x as usize
    }

    pub fn cell(&self, layer: usize, x: u16, y: u16) -> Option<&Cell> {
        if !self.in_bounds(x, y) {
            return None;
        }
        let i = self.index(x, y);
        self.layers.get(layer).and_then(|l| l.cells.get(i))
    }
    /// Returns false (no-op) if out of bounds or layer missing.
    pub fn set_cell(&mut self, layer: usize, x: u16, y: u16, value: Cell) -> bool {
        if !self.in_bounds(x, y) {
            return false;
        }
        let i = self.index(x, y);
        match self.layers.get_mut(layer).and_then(|l| l.cells.get_mut(i)) {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_cell_is_blank() {
        assert!(Cell::BLANK.is_blank());
    }

    #[test]
    fn opaque_bg_space_is_not_blank() {
        let cell = Cell {
            ch: ' ',
            fg: Rgba::WHITE,
            bg: Rgba::WHITE,
        };
        assert!(!cell.is_blank());
    }

    #[test]
    fn non_space_transparent_bg_is_not_blank() {
        let cell = Cell {
            ch: 'x',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        };
        assert!(!cell.is_blank());
    }

    #[test]
    fn rgba_transparency() {
        assert!(Rgba::TRANSPARENT.is_transparent());
        assert!(!Rgba::WHITE.is_transparent());
    }

    #[test]
    fn rgba_hex_serialize_known_values() {
        assert_eq!(serde_json::to_string(&Rgba::WHITE).unwrap(), "\"#FFFFFFFF\"");
        assert_eq!(serde_json::to_string(&Rgba::TRANSPARENT).unwrap(), "\"#00000000\"");
        assert_eq!(serde_json::to_string(&Rgba(18, 52, 86, 120)).unwrap(), "\"#12345678\"");
    }

    #[test]
    fn rgba_hex_round_trips() {
        for c in [Rgba::WHITE, Rgba::TRANSPARENT, Rgba(1, 2, 3, 4), Rgba(255, 0, 128, 64)] {
            let json = serde_json::to_string(&c).unwrap();
            let back: Rgba = serde_json::from_str(&json).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn rgba_hex_deserialize_accepts_lowercase() {
        let back: Rgba = serde_json::from_str("\"#abcdef12\"").unwrap();
        assert_eq!(back, Rgba(0xAB, 0xCD, 0xEF, 0x12));
    }

    #[test]
    fn rgba_hex_deserialize_rejects_malformed_strings() {
        for bad in ["\"red\"", "\"#FFF\"", "\"FFFFFFFF\"", "\"#GGGGGGGG\"", "\"#FFFFFFFFFF\""] {
            assert!(serde_json::from_str::<Rgba>(bad).is_err(), "expected {bad} to be rejected");
        }
    }

    /// Regression for a byte-slicing panic: `'€'` (U+20AC) encodes to 3 UTF-8 bytes, so
    /// `"€ABCDE"` is 8 *bytes* (passing a `hex.len() != 8` byte-length check) but its char
    /// boundaries don't land on the fixed 2-byte cut points the old implementation sliced at.
    /// Must return `Err`, never panic.
    #[test]
    fn rgba_hex_deserialize_rejects_multi_byte_utf8_without_panicking() {
        let json = "\"#€ABCDE\"";
        assert!(serde_json::from_str::<Rgba>(json).is_err());
    }

    /// A wider battery of malformed/adversarial color inputs, catching the multi-byte case
    /// alongside the more ordinary malformations already covered above.
    #[test]
    fn rgba_hex_deserialize_rejects_a_battery_of_malformed_inputs() {
        let bad = [
            "\"#€ABCDE\"",       // multi-byte UTF-8, byte-length 8, not char-length 8
            "\"#日本語ABCDE\"",  // several multi-byte chars
            "\"#\u{0301}FFFFFF\"", // combining mark
            "\"#FFFFFF\u{200D}\"", // ZWJ
            "\"\"",              // empty string
            "\"#\"",             // just the prefix
            "\"##FFFFFFF\"",     // double leading '#'
            "\"# FFFFFF\"",      // whitespace where a hex digit is expected
            "\"#-FFFFFFF\"",     // non-hex punctuation
            "\"#+1234567\"",     // leading '+': from_str_radix's sign-stripping, not a hex digit
            "42",                // not a string at all
            "null",
        ];
        for json in bad {
            let result = std::panic::catch_unwind(|| serde_json::from_str::<Rgba>(json));
            match result {
                Ok(Ok(rgba)) => panic!("expected {json} to be rejected, got {rgba:?}"),
                Ok(Err(_)) => {} // rejected cleanly, as expected
                Err(_) => panic!("expected {json} to be rejected cleanly, but it panicked"),
            }
        }
    }

    /// Regression for `from_str_radix`'s sign-stripping artifact: a leading `'+'` is not a hex
    /// digit, so `"#+1234567"` (a `'+'` plus 7 valid hex digits, 8 bytes total) must be rejected
    /// rather than silently parsed as the 7-digit value `0x01234567`.
    #[test]
    fn rgba_hex_deserialize_rejects_a_leading_plus_sign() {
        assert!(serde_json::from_str::<Rgba>("\"#+1234567\"").is_err());
    }

    #[test]
    fn default_document_is_80x25_blank() {
        let doc = Document::default_document();
        assert_eq!(doc.width, 80);
        assert_eq!(doc.height, 25);
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].cells().len(), 2000);
        assert!(doc.layers[0].cells().iter().all(Cell::is_blank));
    }

    #[test]
    fn in_bounds_edges() {
        let doc = Document::default_document();
        assert!(doc.in_bounds(79, 24));
        assert!(!doc.in_bounds(80, 24));
        assert!(!doc.in_bounds(79, 25));
    }

    #[test]
    fn set_cell_and_cell_round_trip() {
        let mut doc = Document::new(10, 5);
        let value = Cell {
            ch: 'x',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        };
        assert!(doc.set_cell(0, 3, 2, value));
        assert_eq!(doc.cell(0, 3, 2), Some(&value));
        assert_eq!(doc.layers[0].cells()[2 * 10 + 3], value);

        // neighbours untouched
        assert_eq!(doc.cell(0, 2, 2), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 4, 2), Some(&Cell::BLANK));
    }

    #[test]
    fn set_cell_out_of_bounds_is_noop() {
        let mut doc = Document::new(10, 5);
        let value = Cell {
            ch: 'x',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        };
        assert!(!doc.set_cell(0, 10, 0, value));
        assert!(!doc.set_cell(0, 0, 5, value));
        assert!(doc.layers[0].cells().iter().all(Cell::is_blank));
        assert_eq!(doc.cell(0, 10, 0), None);
    }

    #[test]
    fn serde_round_trip() {
        let doc = Document::default_document();
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    #[should_panic(expected = "canvas must be non-empty")]
    fn new_with_zero_width_panics() {
        Document::new(0, 10);
    }

    #[test]
    #[should_panic(expected = "canvas must be non-empty")]
    fn new_with_zero_height_panics() {
        Document::new(10, 0);
    }

    #[test]
    fn far_corner_set_cell_and_cell_at_1024x1024() {
        // Locks in the u16->usize widen-before-multiply index math: at ~1M cells the index
        // would overflow a multiply done in u16 space before widening.
        let mut doc = Document::new(1024, 1024);
        let value = Cell {
            ch: 'x',
            fg: Rgba::WHITE,
            bg: Rgba::TRANSPARENT,
        };
        assert!(doc.set_cell(0, 1023, 1023, value));
        assert_eq!(doc.cell(0, 1023, 1023), Some(&value));
        assert_eq!(doc.layers[0].cells()[1024 * 1024 - 1], value);

        // one-before-far-corner untouched
        assert_eq!(doc.cell(0, 1022, 1023), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 1023, 1022), Some(&Cell::BLANK));
    }
}
