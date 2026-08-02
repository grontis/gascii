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
    /// Builds a Layer directly from an already-shaped, row-major cell buffer. The caller owns the
    /// width/height bookkeeping (same externally-tracked-dimensions convention `blank()` already
    /// relies on) — used by `resize_document` to assemble a resized layer's contents. The
    /// dimensions are taken purely to assert the `len == width*height` invariant at construction:
    /// it otherwise survives on caller discipline alone, and a mis-sized layer would surface as a
    /// distant index panic (e.g. in `resize_layer`) instead of at its source.
    pub(crate) fn from_cells(cells: Vec<Cell>, width: u16, height: u16) -> Self {
        debug_assert_eq!(
            cells.len(),
            width as usize * height as usize,
            "Layer buffer must be exactly width*height cells"
        );
        Layer { cells }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DocExtent {
    pub width: u16,
    pub height: u16,
}

/// A document's own opaque black — the default background for a document that predates this
/// field, and the New dialog's starting well value.
fn default_background() -> Rgba {
    Rgba(0, 0, 0, 255)
}

/// One animation frame: a full layer stack (composited exactly like today's single-frame
/// document) plus an optional per-frame playback override.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Frame {
    pub layers: Vec<Layer>,
    /// Overrides `Document::frame_duration_ms` for this frame only. `None` (the common case)
    /// falls back to the document-level default — see `Document::resolved_frame_duration_ms`.
    #[serde(default)]
    pub duration_override: Option<u32>,
}
impl Frame {
    pub fn blank(width: u16, height: u16) -> Self {
        Frame { layers: vec![Layer::blank(width, height)], duration_override: None }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Document {
    pub width: u16,
    pub height: u16,
    /// The document's own background, set once at creation (New dialog only — not an editable
    /// property afterward, so it never appears as an `Edit` variant or touches history). Additive:
    /// `#[serde(default)]` so a pre-existing `.gascii` file without this field loads as opaque
    /// black, matching the app's prior hardcoded canvas surface.
    #[serde(default = "default_background")]
    pub background: Rgba,
    /// Document-level playback default (ms/frame). Meaningful, and only ever serialized to a
    /// `.gascii` file, once `frame_count() > 1` — a single-frame document's fps is never written
    /// to disk (see the format module's v1/v2 save split). Not `Edit`-tracked, mirrors
    /// `background`'s own "set-and-forget document property" precedent.
    #[serde(default = "Document::default_frame_duration_ms")]
    pub frame_duration_ms: u32,
    #[serde(default = "Document::default_loop_playback")]
    pub loop_playback: bool,
    /// `pub(crate)`: every structural change (add/remove/reorder) MUST go through `frame_ops.rs` +
    /// `History`, mirroring the existing "History is the sole choke point for committed document
    /// mutation" discipline. Only `edit.rs` (apply/undo) and `io/gascii_json.rs` (loading, which
    /// builds a fresh Document with no undo to preserve) write this directly.
    pub(crate) frames: Vec<Frame>,
    /// `pub(crate)`, same reasoning. Structural ops route their before/after active_frame through
    /// the `Edit` payload rather than allowing a direct external write.
    pub(crate) active_frame: usize,
}
impl Document {
    pub const DEFAULT_WIDTH: u16 = 120;
    pub const DEFAULT_HEIGHT: u16 = 40;
    /// Sane upper bound on canvas extent, matching the size the app is designed to remain usable
    /// at. Shared by every caller that must validate an untrusted width/height *before*
    /// allocating anything sized by it (currently: the `.gascii` loader) — a single definition so
    /// that bound can never drift out of sync with the value the rest of the app assumes.
    pub const MAX_WIDTH: u16 = 1024;
    pub const MAX_HEIGHT: u16 = 1024;
    /// Sane upper bound on layer count *per frame*, for the same untrusted-input-validation
    /// reason as `MAX_WIDTH`/`MAX_HEIGHT` — generous enough that no real document gets close to it
    /// (today's app never writes more than one layer), tight enough that a file can't force an
    /// unbounded number of full-size blank layers to be allocated before any per-row shape check
    /// runs. See `MAX_TOTAL_CELLS` for the joint budget across every frame's layer count.
    pub const MAX_LAYERS: usize = 256;
    /// Sane upper bound on frame count, mirroring `MAX_LAYERS`'s exact value and reasoning.
    pub const MAX_FRAMES: usize = 256;
    /// Joint width*height*layers budget, derived from the existing per-axis caps rather than a
    /// fresh magic number — deliberately set to match the worst case a single-frame document
    /// already silently permitted before frames existed (`MAX_WIDTH x MAX_HEIGHT x MAX_LAYERS`).
    /// Enforced per-frame (summed across every frame's layer count) now that frame count is a new
    /// multiplicative axis: without this, the same unflagged worst case would grow from ~4GB to
    /// ~1TB once documents can declare many frames.
    pub const MAX_TOTAL_CELLS: usize =
        Self::MAX_WIDTH as usize * Self::MAX_HEIGHT as usize * Self::MAX_LAYERS;
    /// 10fps; a plain, documented starting default for a freshly created document.
    pub const DEFAULT_FRAME_DURATION_MS: u32 = 100;
    /// Sane upper bound on any per-frame playback duration (an hour), covering both
    /// `frame_duration_ms` (the document default) and a `Frame::duration_override` — a `.gascii`
    /// file is untrusted input, and an unbounded value here is one of the few numeric fields the
    /// format lets through with no structural cap of its own. Clamped, never rejected, at v2 load
    /// (see `io/gascii_json.rs::load_v2`) — a stray huge value is a nuisance to fix by hand, not
    /// a reason to refuse the whole file.
    pub const MAX_FRAME_DURATION_MS: u32 = 3_600_000;

    /// `pub(crate)`: also used as the v2 format envelope's serde default (`io/gascii_json.rs`),
    /// not just `Document`'s own struct default.
    pub(crate) fn default_frame_duration_ms() -> u32 {
        Self::DEFAULT_FRAME_DURATION_MS
    }
    pub(crate) fn default_loop_playback() -> bool {
        true
    }

    pub fn new(width: u16, height: u16) -> Self {
        assert!(width > 0 && height > 0, "canvas must be non-empty");
        Document {
            width,
            height,
            background: default_background(),
            frame_duration_ms: Self::DEFAULT_FRAME_DURATION_MS,
            loop_playback: true,
            frames: vec![Frame::blank(width, height)],
            active_frame: 0,
        }
    }
    /// Default new document: 120×40.
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

    /// The active frame's layer stack. `Document`'s own "no active state" precedent is narrowly
    /// broken by `active_frame` alone — `layers`/`layers_mut` are how every existing (pre-frame)
    /// call site keeps addressing "the" layer stack unchanged.
    pub fn layers(&self) -> &[Layer] {
        &self.frames[self.active_frame].layers
    }
    /// `pub`, mirroring the field's own visibility before frames existed (`layers` was a fully public
    /// `Vec<Layer>`) — layer structure was never `History`-tracked, so this preserves exactly the
    /// mutation surface that already existed, just reached through a method instead of a field.
    pub fn layers_mut(&mut self) -> &mut Vec<Layer> {
        &mut self.frames[self.active_frame].layers
    }

    pub fn cell(&self, layer: usize, x: u16, y: u16) -> Option<&Cell> {
        if !self.in_bounds(x, y) {
            return None;
        }
        let i = self.index(x, y);
        self.layers().get(layer).and_then(|l| l.cells.get(i))
    }
    /// Returns false (no-op) if out of bounds or layer missing.
    pub fn set_cell(&mut self, layer: usize, x: u16, y: u16, value: Cell) -> bool {
        if !self.in_bounds(x, y) {
            return false;
        }
        let i = self.index(x, y);
        match self.layers_mut().get_mut(layer).and_then(|l| l.cells.get_mut(i)) {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
    pub fn active_frame(&self) -> usize {
        self.active_frame
    }
    /// `false` (no-op, cursor unchanged) if `idx` is out of bounds.
    pub fn set_active_frame(&mut self, idx: usize) -> bool {
        if idx < self.frames.len() {
            self.active_frame = idx;
            true
        } else {
            false
        }
    }
    pub fn frame(&self, idx: usize) -> Option<&Frame> {
        self.frames.get(idx)
    }
    pub fn frame_layers(&self, frame: usize) -> Option<&[Layer]> {
        self.frames.get(frame).map(|f| f.layers.as_slice())
    }
    /// Explicit-frame read, independent of `active_frame` — for callers that already need
    /// cross-frame awareness (compositing, the format module).
    pub fn cell_at(&self, frame: usize, layer: usize, x: u16, y: u16) -> Option<&Cell> {
        if !self.in_bounds(x, y) {
            return None;
        }
        let i = self.index(x, y);
        self.frames.get(frame)?.layers.get(layer).and_then(|l| l.cells.get(i))
    }
    /// `pub(crate)`: only `History` (via `Edit::Cells`) and the format loader write cells against
    /// an explicit, possibly-non-active frame — every other caller goes through `set_cell`.
    pub(crate) fn set_cell_at(&mut self, frame: usize, layer: usize, x: u16, y: u16, value: Cell) -> bool {
        if !self.in_bounds(x, y) {
            return false;
        }
        let i = self.index(x, y);
        match self
            .frames
            .get_mut(frame)
            .and_then(|f| f.layers.get_mut(layer))
            .and_then(|l| l.cells.get_mut(i))
        {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }
    /// `frame`'s own duration override if set, else the document-level default. `None` only for an
    /// out-of-bounds `frame`.
    pub fn resolved_frame_duration_ms(&self, frame: usize) -> Option<u32> {
        self.frames.get(frame).map(|f| f.duration_override.unwrap_or(self.frame_duration_ms))
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
    fn default_document_is_120x40_blank() {
        let doc = Document::default_document();
        assert_eq!(doc.width, 120);
        assert_eq!(doc.height, 40);
        assert_eq!(doc.layers().len(), 1);
        assert_eq!(doc.layers()[0].cells().len(), 4800);
        assert!(doc.layers()[0].cells().iter().all(Cell::is_blank));
    }

    #[test]
    fn in_bounds_edges() {
        let doc = Document::default_document();
        assert!(doc.in_bounds(119, 39));
        assert!(!doc.in_bounds(120, 39));
        assert!(!doc.in_bounds(119, 40));
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
        assert_eq!(doc.layers()[0].cells()[2 * 10 + 3], value);

        // neighbours untouched
        assert_eq!(doc.cell(0, 2, 2), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 4, 2), Some(&Cell::BLANK));
    }

    /// The `cells.len() == width*height` invariant is asserted at construction (debug builds)
    /// rather than surviving on caller discipline alone — a mis-sized buffer must fail at its
    /// source, not as a distant index panic once a multi-layer feature makes it reachable.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "width*height")]
    fn from_cells_with_a_mis_sized_buffer_panics_in_debug_builds() {
        let _ = Layer::from_cells(vec![Cell::BLANK; 7], 4, 2);
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
        assert!(doc.layers()[0].cells().iter().all(Cell::is_blank));
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
        assert_eq!(doc.layers()[0].cells()[1024 * 1024 - 1], value);

        // one-before-far-corner untouched
        assert_eq!(doc.cell(0, 1022, 1023), Some(&Cell::BLANK));
        assert_eq!(doc.cell(0, 1023, 1022), Some(&Cell::BLANK));
    }

    // --- frame substrate ---

    #[test]
    fn document_new_has_exactly_one_frame() {
        let doc = Document::new(4, 3);
        assert_eq!(doc.frame_count(), 1);
        assert_eq!(doc.active_frame(), 0);
    }

    #[test]
    fn cell_and_set_cell_address_the_active_frame_implicitly() {
        let mut doc = Document::new(4, 4);
        let value = Cell { ch: 'x', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT };
        assert!(doc.set_cell(0, 1, 1, value));
        assert_eq!(doc.cell(0, 1, 1), Some(&value));
        // The same value, addressed explicitly against frame 0, matches.
        assert_eq!(doc.cell_at(0, 0, 1, 1), Some(&value));
    }

    #[test]
    fn cell_at_addresses_an_explicit_frame_independent_of_active_frame() {
        let mut doc = Document::new(4, 4);
        let value = Cell { ch: 'y', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT };
        assert!(doc.set_cell_at(0, 0, 2, 2, value));
        // Reading through the implicit, active-frame-addressed API sees the same write.
        assert_eq!(doc.cell(0, 2, 2), Some(&value));
        assert_eq!(doc.cell_at(0, 0, 2, 2), Some(&value));
    }

    #[test]
    fn set_active_frame_rejects_an_out_of_bounds_index_and_leaves_the_cursor_unchanged() {
        let mut doc = Document::new(4, 4);
        assert!(!doc.set_active_frame(1), "only frame 0 exists");
        assert_eq!(doc.active_frame(), 0, "cursor must be unchanged after a rejected set");
    }

    #[test]
    fn frame_layers_returns_none_for_an_out_of_bounds_frame() {
        let doc = Document::new(4, 4);
        assert!(doc.frame_layers(0).is_some());
        assert!(doc.frame_layers(1).is_none());
    }

    #[test]
    fn resolved_frame_duration_ms_falls_back_to_the_document_default_when_no_override_is_set() {
        let doc = Document::new(4, 4);
        assert_eq!(doc.resolved_frame_duration_ms(0), Some(Document::DEFAULT_FRAME_DURATION_MS));
        assert_eq!(doc.resolved_frame_duration_ms(1), None, "out-of-bounds frame is None");
    }

    #[test]
    fn max_total_cells_equals_max_width_times_max_height_times_max_layers() {
        assert_eq!(
            Document::MAX_TOTAL_CELLS,
            Document::MAX_WIDTH as usize * Document::MAX_HEIGHT as usize * Document::MAX_LAYERS,
            "the derived joint budget must not silently desync from its three source constants"
        );
    }
}
