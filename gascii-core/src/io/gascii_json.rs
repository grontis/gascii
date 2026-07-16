//! `.gascii` file format: a versioned JSON envelope holding structure-of-arrays layers, with
//! colors run-length-encoded within each row (never across row boundaries, so a single row's
//! edit never ripples the encoding of unrelated rows). Cell access always goes through
//! `Document`'s public API — this module never reaches into `Layer`'s private cell storage.

use serde::{Deserialize, Serialize};

use crate::model::{Cell, Document, Layer, Rgba};
use crate::palette::{validate_width, WidthReject};

pub const CURRENT_VERSION: u32 = 1;

/// Matches `Document`'s own default so a pre-existing file without this field loads identically to
/// how the app already rendered it (a hardcoded opaque black canvas surface).
fn default_background() -> Rgba {
    Rgba(0, 0, 0, 255)
}

#[derive(Serialize, Deserialize)]
struct FileEnvelope {
    version: u32,
    width: u16,
    height: u16,
    layers: Vec<FileLayer>,
    /// Additive: absent in every file saved before this field existed, so `#[serde(default)]`
    /// keeps those files loading unchanged rather than becoming a rejected/unsupported version.
    #[serde(default = "default_background")]
    background: Rgba,
}

#[derive(Serialize, Deserialize)]
struct FileLayer {
    glyphs: Vec<String>,
    fg: Vec<Vec<(u16, Rgba)>>,
    bg: Vec<Vec<(u16, Rgba)>>,
}

/// Why loading a `.gascii` file failed. Never a panic, even on adversarial/malformed input.
#[derive(Debug)]
pub enum LoadError {
    Json(serde_json::Error),
    UnsupportedVersion { found: u32, max_supported: u32 },
    EmptyExtent,
    /// `width`/`height` exceed `Document::MAX_WIDTH`/`MAX_HEIGHT`. Checked before any allocation
    /// sized by these fields happens — a file's raw, unvalidated `u16` extent could otherwise
    /// drive a single multi-gigabyte `Document::new` allocation.
    ExtentTooLarge { width: u16, height: u16, max_width: u16, max_height: u16 },
    /// `layers.len()` exceeds `Document::MAX_LAYERS`. Checked before any per-layer allocation, for
    /// the same reason as `ExtentTooLarge` — an unbounded declared layer count is an independent
    /// amplification vector even at a modest width/height.
    TooManyLayers { found: usize, max: usize },
    /// The layer's `glyphs`/`fg`/`bg` arrays don't all have `expected` (the document's declared
    /// `height`) rows. Carries each array's actual row count so the message doesn't have to guess
    /// which one is wrong (or misleadingly imply row 0 specifically is at fault).
    LayerRowCountMismatch { layer: usize, expected: usize, glyph_rows: usize, fg_rows: usize, bg_rows: usize },
    ShapeMismatch { layer: usize, row: usize },
    MalformedRuns { layer: usize, row: usize, expected: u16, got: usize },
    InvalidGlyph { layer: usize, row: usize, col: u16, reason: WidthReject },
    NoLayers,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Json(e) => write!(f, "malformed file: {e}"),
            LoadError::UnsupportedVersion { found, max_supported } => write!(
                f,
                "file format version {found} is newer than the supported version {max_supported}"
            ),
            LoadError::EmptyExtent => write!(f, "document width and height must both be non-zero"),
            LoadError::ExtentTooLarge { width, height, max_width, max_height } => write!(
                f,
                "document {width}x{height} exceeds the maximum supported size {max_width}x{max_height}"
            ),
            LoadError::TooManyLayers { found, max } => {
                write!(f, "document has {found} layers, exceeding the maximum supported {max}")
            }
            LoadError::LayerRowCountMismatch { layer, expected, glyph_rows, fg_rows, bg_rows } => write!(
                f,
                "layer {layer}: expected {expected} rows, found glyphs={glyph_rows}, fg={fg_rows}, bg={bg_rows}"
            ),
            LoadError::ShapeMismatch { layer, row } => {
                write!(f, "layer {layer}, row {row} does not match the document's declared width")
            }
            LoadError::MalformedRuns { layer, row, expected, got } => {
                write!(f, "layer {layer}, row {row}: color runs total {got} cells, expected {expected}")
            }
            LoadError::InvalidGlyph { layer, row, col, reason } => {
                let why = match reason {
                    WidthReject::Control => "a control character",
                    WidthReject::ZeroWidth => "a zero-width or combining character",
                    WidthReject::DoubleWidth => "a double-width character",
                };
                write!(f, "layer {layer}, row {row}, column {col}: glyph is {why}, not single-width")
            }
            LoadError::NoLayers => write!(f, "document has no layers"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Serializes `doc` to a compact JSON string. Never fails: every `Document` is representable.
pub fn save_string(doc: &Document) -> String {
    let layers = (0..doc.layers.len()).map(|i| encode_layer(doc, i)).collect();
    let envelope = FileEnvelope {
        version: CURRENT_VERSION,
        width: doc.width,
        height: doc.height,
        layers,
        background: doc.background,
    };
    serde_json::to_string(&envelope).expect("Document -> FileEnvelope is always serializable")
}

/// Parses a `.gascii` JSON string into a `Document`, rejecting malformed or unsupported-version
/// input with a specific `LoadError` rather than panicking.
pub fn load_str(s: &str) -> Result<Document, LoadError> {
    let envelope: FileEnvelope = serde_json::from_str(s).map_err(LoadError::Json)?;
    if envelope.version > CURRENT_VERSION {
        return Err(LoadError::UnsupportedVersion {
            found: envelope.version,
            max_supported: CURRENT_VERSION,
        });
    }
    // Must reject before Document::new, which asserts on zero extent.
    if envelope.width == 0 || envelope.height == 0 {
        return Err(LoadError::EmptyExtent);
    }
    // Must reject before Document::new, which eagerly allocates width*height cells: an untrusted
    // u16 width/height pair (up to 65535 each) would otherwise drive a ~51 GB allocation attempt
    // from a file well under 200 bytes, before any per-row shape validation ever runs.
    if envelope.width > Document::MAX_WIDTH || envelope.height > Document::MAX_HEIGHT {
        return Err(LoadError::ExtentTooLarge {
            width: envelope.width,
            height: envelope.height,
            max_width: Document::MAX_WIDTH,
            max_height: Document::MAX_HEIGHT,
        });
    }
    if envelope.layers.is_empty() {
        return Err(LoadError::NoLayers);
    }
    // Must reject before the per-layer Layer::blank(width, height) loop below: an unbounded
    // declared layer count is an independent amplification vector even at a capped width/height.
    if envelope.layers.len() > Document::MAX_LAYERS {
        return Err(LoadError::TooManyLayers { found: envelope.layers.len(), max: Document::MAX_LAYERS });
    }

    let mut doc = Document::new(envelope.width, envelope.height);
    doc.layers.clear();
    for file_layer in &envelope.layers {
        let idx = doc.layers.len();
        doc.layers.push(Layer::blank(envelope.width, envelope.height));
        decode_layer_into(&mut doc, idx, file_layer)?;
    }
    doc.background = envelope.background;
    Ok(doc)
}

/// Extends the last run if `color` matches and its count hasn't hit `u16::MAX`, else starts a
/// new run.
fn push_run(runs: &mut Vec<(u16, Rgba)>, color: Rgba) {
    if let Some(last) = runs.last_mut() {
        if last.1 == color && last.0 < u16::MAX {
            last.0 += 1;
            return;
        }
    }
    runs.push((1, color));
}

fn encode_layer(doc: &Document, layer: usize) -> FileLayer {
    let mut glyphs = Vec::with_capacity(doc.height as usize);
    let mut fg = Vec::with_capacity(doc.height as usize);
    let mut bg = Vec::with_capacity(doc.height as usize);
    for y in 0..doc.height {
        let mut row_glyphs = String::with_capacity(doc.width as usize);
        let mut row_fg = Vec::new();
        let mut row_bg = Vec::new();
        for x in 0..doc.width {
            let cell = doc.cell(layer, x, y).copied().unwrap_or(Cell::BLANK);
            row_glyphs.push(cell.ch);
            push_run(&mut row_fg, cell.fg);
            push_run(&mut row_bg, cell.bg);
        }
        glyphs.push(row_glyphs);
        fg.push(row_fg);
        bg.push(row_bg);
    }
    FileLayer { glyphs, fg, bg }
}

/// Expands `runs` into one color per cell. Errors if the runs' total length doesn't exactly
/// equal `width` — an adversarial or corrupted file, never silently truncated or padded.
///
/// Validates the running total against `width` *before* pushing each run's expansion, not after
/// materializing the full `Vec`: `runs` comes straight from untrusted JSON with no cap on either
/// the array's length or any individual run's declared `len` (up to `u16::MAX`), so
/// allocate-then-check would let a tiny file (a handful of runs claiming `u16::MAX` cells each)
/// force a multi-gigabyte allocation before the mismatch is ever noticed. Bailing out the moment
/// the accumulated length would exceed `width` caps the allocation at `width` cells, regardless
/// of how large or numerous the declared runs are.
fn expand_runs(runs: &[(u16, Rgba)], width: u16, layer: usize, row: usize) -> Result<Vec<Rgba>, LoadError> {
    let width = width as usize;
    let mut out = Vec::with_capacity(width);
    let mut total: usize = 0;
    for &(len, color) in runs {
        total = total.saturating_add(len as usize);
        if total > width {
            return Err(LoadError::MalformedRuns { layer, row, expected: width as u16, got: total });
        }
        out.resize(total, color);
    }
    if out.len() != width {
        return Err(LoadError::MalformedRuns { layer, row, expected: width as u16, got: out.len() });
    }
    Ok(out)
}

/// Decodes one file layer's glyphs/colors into `doc`'s layer `layer`. Every loaded glyph is
/// routed through `validate_width` — the same choke point every other character-entry path
/// (`TextTool`, pencil's page-constrained glyphs) already goes through — so a well-formed-shaped
/// but adversarial file can't inject a double-width/zero-width/control character that the app's
/// own renderer assumes never happens (one glyph per fixed-size cell). Width is a structural
/// invariant, enforced on every loaded glyph.
fn decode_layer_into(doc: &mut Document, layer: usize, file_layer: &FileLayer) -> Result<(), LoadError> {
    let (width, height) = (doc.width, doc.height);
    if file_layer.glyphs.len() != height as usize
        || file_layer.fg.len() != height as usize
        || file_layer.bg.len() != height as usize
    {
        return Err(LoadError::LayerRowCountMismatch {
            layer,
            expected: height as usize,
            glyph_rows: file_layer.glyphs.len(),
            fg_rows: file_layer.fg.len(),
            bg_rows: file_layer.bg.len(),
        });
    }
    for y in 0..height {
        let row = y as usize;
        let glyph_row: Vec<char> = file_layer.glyphs[row].chars().collect();
        if glyph_row.len() != width as usize {
            return Err(LoadError::ShapeMismatch { layer, row });
        }
        for (x, &ch) in glyph_row.iter().enumerate() {
            if let Err(reason) = validate_width(ch) {
                return Err(LoadError::InvalidGlyph { layer, row, col: x as u16, reason });
            }
        }
        let fg_row = expand_runs(&file_layer.fg[row], width, layer, row)?;
        let bg_row = expand_runs(&file_layer.bg[row], width, layer, row)?;
        for x in 0..width {
            let xi = x as usize;
            let cell = Cell { ch: glyph_row[xi], fg: fg_row[xi], bg: bg_row[xi] };
            doc.set_cell(layer, x, y, cell);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char, fg: Rgba, bg: Rgba) -> Cell {
        Cell { ch, fg, bg }
    }

    #[test]
    fn round_trips_a_default_blank_document() {
        let doc = Document::default_document();
        let json = save_string(&doc);
        let back = load_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn round_trips_a_1x1_document() {
        let mut doc = Document::new(1, 1);
        doc.set_cell(0, 0, 0, cell('x', Rgba::WHITE, Rgba(1, 2, 3, 255)));
        let back = load_str(&save_string(&doc)).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn round_trips_a_document_with_a_full_row_single_color_run() {
        let mut doc = Document::new(20, 5);
        for x in 0..20u16 {
            doc.set_cell(0, x, 2, cell('#', Rgba(10, 20, 30, 255), Rgba(40, 50, 60, 255)));
        }
        let back = load_str(&save_string(&doc)).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn round_trips_a_maximally_noisy_document() {
        let mut doc = Document::new(16, 16);
        for y in 0..16u16 {
            for x in 0..16u16 {
                let ch = char::from_u32('a' as u32 + ((x as u32 + y as u32 * 16) % 26)).unwrap();
                let c = ((x + y) % 255) as u8;
                doc.set_cell(0, x, y, cell(ch, Rgba(c, c.wrapping_add(1), c.wrapping_add(2), 255), Rgba(c, c, c, 128)));
            }
        }
        let back = load_str(&save_string(&doc)).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn round_trips_at_1024x1024_scale() {
        let mut doc = Document::new(1024, 1024);
        doc.set_cell(0, 0, 0, cell('A', Rgba::WHITE, Rgba::TRANSPARENT));
        doc.set_cell(0, 1023, 1023, cell('Z', Rgba(1, 2, 3, 255), Rgba(4, 5, 6, 255)));
        let back = load_str(&save_string(&doc)).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn round_trips_a_manually_constructed_multi_layer_document() {
        let mut doc = Document::new(4, 4);
        doc.layers.push(Layer::blank(4, 4));
        doc.set_cell(0, 1, 1, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));
        doc.set_cell(1, 2, 2, cell('b', Rgba(9, 9, 9, 255), Rgba(8, 8, 8, 255)));
        let back = load_str(&save_string(&doc)).unwrap();
        assert_eq!(doc, back);
        assert_eq!(back.layers.len(), 2);
    }


    #[test]
    fn version_too_new_is_rejected() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["version"] = serde_json::json!(9999);
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::UnsupportedVersion { found, max_supported }) => {
                assert_eq!(found, 9999);
                assert_eq!(max_supported, CURRENT_VERSION);
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn a_non_black_background_round_trips() {
        let mut doc = Document::new(2, 2);
        doc.background = Rgba(10, 20, 30, 255);
        let back = load_str(&save_string(&doc)).unwrap();
        assert_eq!(back.background, Rgba(10, 20, 30, 255));
    }

    /// A file saved before `background` existed (no such key at all) must load as opaque black —
    /// the same value the app hardcoded as its canvas surface before this field existed.
    #[test]
    fn a_legacy_file_with_no_background_field_loads_as_opaque_black() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value.as_object_mut().unwrap().remove("background");
        let json = serde_json::to_string(&value).unwrap();
        let back = load_str(&json).unwrap();
        assert_eq!(back.background, Rgba(0, 0, 0, 255));
    }

    #[test]
    fn additive_unknown_top_level_field_is_tolerated() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["future_field"] = serde_json::json!("unused");
        let json = serde_json::to_string(&value).unwrap();
        assert!(load_str(&json).is_ok());
    }

    #[test]
    fn zero_width_is_rejected_cleanly_not_a_panic() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["width"] = serde_json::json!(0);
        let json = serde_json::to_string(&value).unwrap();
        assert!(matches!(load_str(&json), Err(LoadError::EmptyExtent)));
    }

    #[test]
    fn zero_height_is_rejected_cleanly_not_a_panic() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["height"] = serde_json::json!(0);
        let json = serde_json::to_string(&value).unwrap();
        assert!(matches!(load_str(&json), Err(LoadError::EmptyExtent)));
    }

    #[test]
    fn malformed_runs_that_undershoot_width_are_rejected() {
        let doc = Document::new(3, 1);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        // One run of length 1 (should be 3) for row 0's fg.
        value["layers"][0]["fg"][0] = serde_json::json!([[1, "#FFFFFFFF"]]);
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::MalformedRuns { layer, row, expected, got }) => {
                assert_eq!(layer, 0);
                assert_eq!(row, 0);
                assert_eq!(expected, 3);
                assert_eq!(got, 1);
            }
            other => panic!("expected MalformedRuns, got {other:?}"),
        }
    }

    /// A single run declaring `u16::MAX` (65,535) cells against a tiny declared `width` must be
    /// rejected immediately, without ever allocating anywhere near 65,535 elements — expanding
    /// runs before validating their total would let a small file (many such runs) force a
    /// multi-gigabyte allocation. The input here stays tiny (`width: 3`, one run of `u16::MAX`)
    /// so the test is fast either way; the meaningful assertion is the prompt `Err`.
    #[test]
    fn adversarially_large_declared_run_length_is_rejected_without_expanding() {
        let doc = Document::new(3, 1);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["layers"][0]["fg"][0] = serde_json::json!([[u16::MAX, "#FFFFFFFF"]]);
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::MalformedRuns { layer, row, expected, got }) => {
                assert_eq!(layer, 0);
                assert_eq!(row, 0);
                assert_eq!(expected, 3);
                assert_eq!(got, u16::MAX as usize);
            }
            other => panic!("expected MalformedRuns, got {other:?}"),
        }
    }

    /// A compact multi-run version of the same attack: many small-looking runs whose declared
    /// lengths sum far past `width`. Must reject on the run that first pushes the total over
    /// `width`, not after expanding everything.
    #[test]
    fn many_runs_summing_far_past_width_are_rejected_promptly() {
        let doc = Document::new(4, 1);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        let runs: Vec<_> = (0..50).map(|_| serde_json::json!([u16::MAX, "#00000000"])).collect();
        value["layers"][0]["bg"][0] = serde_json::json!(runs);
        let json = serde_json::to_string(&value).unwrap();
        assert!(matches!(
            load_str(&json),
            Err(LoadError::MalformedRuns { layer: 0, row: 0, expected: 4, .. })
        ));
    }

    #[test]
    fn malformed_runs_that_overshoot_width_are_rejected() {
        let doc = Document::new(3, 1);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["layers"][0]["bg"][0] = serde_json::json!([[5, "#00000000"]]);
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::MalformedRuns { layer, row, expected, got }) => {
                assert_eq!(layer, 0);
                assert_eq!(row, 0);
                assert_eq!(expected, 3);
                assert_eq!(got, 5);
            }
            other => panic!("expected MalformedRuns, got {other:?}"),
        }
    }

    /// Row-level shape mismatch: the layer's arrays have the right *count* of rows (so it's not
    /// `LayerRowCountMismatch`), but one row's glyph string has the wrong character count.
    #[test]
    fn wrong_glyph_row_length_is_rejected_as_shape_mismatch() {
        let doc = Document::new(3, 3);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["layers"][0]["glyphs"][1] = serde_json::json!("xx"); // row 1: 2 chars, want 3
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::ShapeMismatch { layer, row }) => {
                assert_eq!(layer, 0);
                assert_eq!(row, 1);
            }
            other => panic!("expected ShapeMismatch, got {other:?}"),
        }
    }

    /// A well-shaped file (correct row/run counts, valid hex colors) containing a double-width
    /// glyph in `glyphs` must be rejected, not silently accepted — every other character-entry
    /// path in the app funnels through `validate_width`, and the loader must too, since a
    /// double-width glyph rendered at a fixed-size cell would visually overlap its neighbor.
    #[test]
    fn wide_glyph_in_loaded_file_is_rejected_as_invalid_glyph() {
        let doc = Document::new(3, 1);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["layers"][0]["glyphs"][0] = serde_json::json!("a你b"); // '你' is double-width
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::InvalidGlyph { layer, row, col, reason }) => {
                assert_eq!(layer, 0);
                assert_eq!(row, 0);
                assert_eq!(col, 1);
                assert_eq!(reason, crate::palette::WidthReject::DoubleWidth);
            }
            other => panic!("expected InvalidGlyph, got {other:?}"),
        }
    }

    /// Same as above but for a control character, the other structural rejection `validate_width`
    /// enforces.
    #[test]
    fn control_char_glyph_in_loaded_file_is_rejected_as_invalid_glyph() {
        let doc = Document::new(3, 1);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["layers"][0]["glyphs"][0] = serde_json::json!("a\tb");
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::InvalidGlyph { reason, .. }) => {
                assert_eq!(reason, crate::palette::WidthReject::Control);
            }
            other => panic!("expected InvalidGlyph, got {other:?}"),
        }
    }

    #[test]
    fn wrong_glyph_row_count_is_rejected_as_layer_row_count_mismatch() {
        let doc = Document::new(3, 3);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        let glyphs = value["layers"][0]["glyphs"].as_array_mut().unwrap();
        glyphs.pop();
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::LayerRowCountMismatch { layer, expected, glyph_rows, fg_rows, bg_rows }) => {
                assert_eq!(layer, 0);
                assert_eq!(expected, 3);
                assert_eq!(glyph_rows, 2);
                assert_eq!(fg_rows, 3);
                assert_eq!(bg_rows, 3);
            }
            other => panic!("expected LayerRowCountMismatch, got {other:?}"),
        }
    }

    /// Regression for the pre-allocation decompression bomb: a file declaring `width`/`height` at
    /// `u16::MAX` with a minimally-shaped single layer (well under 200 bytes) must be rejected
    /// with `ExtentTooLarge` before `Document::new` is ever called, not attempt the ~51 GB
    /// allocation that constructing a `65535x65535` document would require.
    #[test]
    fn huge_declared_extent_is_rejected_before_any_allocation() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["width"] = serde_json::json!(u16::MAX);
        value["height"] = serde_json::json!(u16::MAX);
        // Shape doesn't matter — this must be rejected long before shape validation is reached.
        value["layers"][0]["glyphs"] = serde_json::json!([]);
        value["layers"][0]["fg"] = serde_json::json!([]);
        value["layers"][0]["bg"] = serde_json::json!([]);
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::ExtentTooLarge { width, height, max_width, max_height }) => {
                assert_eq!(width, u16::MAX);
                assert_eq!(height, u16::MAX);
                assert_eq!(max_width, Document::MAX_WIDTH);
                assert_eq!(max_height, Document::MAX_HEIGHT);
            }
            other => panic!("expected ExtentTooLarge, got {other:?}"),
        }
    }

    /// A width or height just one past the cap must also be rejected — the cap itself is
    /// inclusive (a document exactly at `MAX_WIDTH`/`MAX_HEIGHT` is fine; see
    /// `round_trips_at_1024x1024_scale`).
    #[test]
    fn extent_one_past_the_cap_is_rejected() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["width"] = serde_json::json!(Document::MAX_WIDTH + 1);
        value["layers"][0]["glyphs"] = serde_json::json!([]);
        value["layers"][0]["fg"] = serde_json::json!([]);
        value["layers"][0]["bg"] = serde_json::json!([]);
        let json = serde_json::to_string(&value).unwrap();
        assert!(matches!(load_str(&json), Err(LoadError::ExtentTooLarge { .. })));
    }

    /// Independent amplification vector: a sane width/height with an excessive declared layer
    /// count must also be rejected before the per-layer `Layer::blank` allocation loop runs.
    #[test]
    fn excessive_layer_count_is_rejected_before_allocating_layers() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        let one_layer = value["layers"][0].clone();
        let layers: Vec<_> = (0..=Document::MAX_LAYERS).map(|_| one_layer.clone()).collect();
        value["layers"] = serde_json::json!(layers);
        let json = serde_json::to_string(&value).unwrap();
        match load_str(&json) {
            Err(LoadError::TooManyLayers { found, max }) => {
                assert_eq!(found, Document::MAX_LAYERS + 1);
                assert_eq!(max, Document::MAX_LAYERS);
            }
            other => panic!("expected TooManyLayers, got {other:?}"),
        }
    }

    #[test]
    fn no_layers_is_rejected_cleanly() {
        let doc = Document::new(2, 2);
        let mut value: serde_json::Value = serde_json::from_str(&save_string(&doc)).unwrap();
        value["layers"] = serde_json::json!([]);
        let json = serde_json::to_string(&value).unwrap();
        assert!(matches!(load_str(&json), Err(LoadError::NoLayers)));
    }

    #[test]
    fn garbage_json_is_a_clean_json_error_not_a_panic() {
        assert!(matches!(load_str("not json"), Err(LoadError::Json(_))));
    }

    /// Deeply nested JSON in a field this format doesn't even care about (an unknown top-level
    /// key, tolerated per `additive_unknown_top_level_field_is_tolerated`) must not stack-overflow
    /// the process while `serde_json` skips it — safe at depths far beyond the one used here,
    /// which is kept moderate so the test stays fast. The resulting error is `NoLayers` (from the
    /// empty `layers` array elsewhere in the fixture), confirming the deeply nested value was
    /// skipped cleanly rather than the parse aborting partway through.
    #[test]
    fn deeply_nested_json_in_an_unknown_field_does_not_crash_the_parser() {
        let depth = 50_000;
        let mut json = String::from(
            r#"{"version":1,"width":2,"height":2,"settings":{"strict_ascii":false},"layers":[],"extra":"#,
        );
        for _ in 0..depth {
            json.push('[');
        }
        for _ in 0..depth {
            json.push(']');
        }
        json.push('}');
        assert!(matches!(load_str(&json), Err(LoadError::NoLayers)));
    }

    /// A literal, hand-written JSON string (not built via `serde_json::Value`, which would already
    /// dedupe keys before this code ever sees it) with a duplicated top-level key must not panic —
    /// `serde_json`'s struct visitor either takes the last occurrence or errors, and either
    /// outcome is acceptable; only a panic or hang would not be.
    #[test]
    fn duplicate_json_keys_do_not_panic() {
        let json = r#"{"version":1,"width":2,"width":2,"height":2,"settings":{"strict_ascii":false},"layers":[]}"#;
        // The derive-generated `Deserialize` for `FileEnvelope` rejects a duplicated struct field
        // outright ("duplicate field") rather than silently taking the last value — stricter than
        // the minimum "just don't panic" bar this test exists to enforce, but worth pinning as the
        // actual observed behavior of this project's pinned serde_json version.
        assert!(matches!(load_str(json), Err(LoadError::Json(_))));
    }

    /// A bare empty string is the simplest possible malformed input — must be a clean `Json` error,
    /// not a panic, distinct from `"not json"` (`garbage_json_is_a_clean_json_error_not_a_panic`)
    /// in that there isn't even a single token to fail on.
    #[test]
    fn empty_string_input_is_rejected_cleanly_not_a_panic() {
        assert!(matches!(load_str(""), Err(LoadError::Json(_))));
    }

    /// A syntactically valid but structurally empty JSON object is missing every required field —
    /// must be a clean `Json` error (missing field), not a panic.
    #[test]
    fn empty_json_object_is_rejected_cleanly_not_a_panic() {
        assert!(matches!(load_str("{}"), Err(LoadError::Json(_))));
    }
}
