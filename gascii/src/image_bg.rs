//! Image background: a loaded reference image plus its trace/export display settings. Exactly one
//! lives at a time, in-memory only. `decode_image`, `fit_contain`, and `fit_cover` are the pure,
//! render-free halves — the file pick, decode, and texture upload are `GasciiApp` methods in
//! `app.rs` (they need `&mut self`/`ctx`), the live paint is in `canvas.rs`, and the export
//! composite is in `png_export.rs`.
//!
//! Distinct from `Document::background` — the document's own solid backdrop colour. The image
//! background sits above that solid fill and below the cells, in both the live trace overlay and
//! the export composite.

use eframe::egui;

/// A loaded image background: decoded source pixels, an optionally-uploaded live texture, and the
/// trace/export display settings. `texture: None` (not yet uploaded, or headless) is a no-op
/// everywhere it's read.
pub(crate) struct ImageBackground {
    pub pixels: image::RgbaImage,
    pub texture: Option<egui::TextureHandle>,
    /// 0.0..=1.0 — the live trace overlay's strength. Independent of `export_opacity` so a faint
    /// tracing guide never washes out the exported background.
    pub trace_opacity: f32,
    /// 0.0..=1.0 — the PNG export composite's strength. Independent of `trace_opacity` so a faint
    /// tracing guide never washes out the exported background.
    pub export_opacity: f32,
    /// Trace overlay visibility; hides without dropping the loaded image.
    pub show_as_trace: bool,
    /// Gates whether the export composite includes this image.
    pub use_in_export: bool,
    /// Source path, kept for display only — not persisted (in-memory-only for v1). Dormant until
    /// something displays it.
    #[allow(dead_code)]
    pub path: Option<std::path::PathBuf>,
}

impl ImageBackground {
    /// Defaults: a faint (50%) trace, shown immediately; a full-strength (100%) export composite,
    /// gated off until the user opts in.
    pub fn new(
        pixels: image::RgbaImage,
        texture: Option<egui::TextureHandle>,
        path: Option<std::path::PathBuf>,
    ) -> Self {
        ImageBackground {
            pixels,
            texture,
            trace_opacity: 0.5,
            export_opacity: 1.0,
            show_as_trace: true,
            use_in_export: false,
            path,
        }
    }
}

/// Decodes arbitrary image bytes (png/jpeg — whatever `image`'s enabled decoders support) into
/// straight-alpha RGBA8. Failure (corrupt bytes, unsupported format) is a plain `Err`, never a
/// panic — the caller treats it as non-fatal (sets `last_error`, leaves the current image alone).
pub(crate) fn decode_image(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    image::load_from_memory(bytes).map(|d| d.to_rgba8()).map_err(|e| e.to_string())
}

/// Aspect-preserving, centered fit of an `img_w`×`img_h` image *inside* an `avail_w`×`avail_h` box
/// (Contain — the trace overlay's fit): the whole image stays visible, undistorted, letterboxed on
/// whichever axis doesn't fill the box. Returns `(offset_x, offset_y, width, height)`; both offsets
/// are `>= 0`. `None` for any non-positive dimension (nothing to fit).
pub(crate) fn fit_contain(img_w: u32, img_h: u32, avail_w: f32, avail_h: f32) -> Option<(f32, f32, f32, f32)> {
    if img_w == 0 || img_h == 0 || avail_w <= 0.0 || avail_h <= 0.0 {
        return None;
    }
    let scale = (avail_w / img_w as f32).min(avail_h / img_h as f32);
    let (w, h) = (img_w as f32 * scale, img_h as f32 * scale);
    let (ox, oy) = ((avail_w - w) / 2.0, (avail_h - h) / 2.0);
    Some((ox, oy, w, h))
}

/// Aspect-preserving, centered fit of an `img_w`×`img_h` image to *cover* an `avail_w`×`avail_h`
/// box (Cover — the export background's fit): the box is filled entirely, no letterbox, and the
/// axis that overflows is cropped (its offset goes negative). Returns `(offset_x, offset_y, width,
/// height)`. Differs from `fit_contain` only in using the larger of the two scale candidates
/// (`max` instead of `min`). `None` for any non-positive dimension (nothing to fit).
pub(crate) fn fit_cover(img_w: u32, img_h: u32, avail_w: f32, avail_h: f32) -> Option<(f32, f32, f32, f32)> {
    if img_w == 0 || img_h == 0 || avail_w <= 0.0 || avail_h <= 0.0 {
        return None;
    }
    let scale = (avail_w / img_w as f32).max(avail_h / img_h as f32);
    let (w, h) = (img_w as f32 * scale, img_h as f32 * scale);
    let (ox, oy) = ((avail_w - w) / 2.0, (avail_h - h) / 2.0);
    Some((ox, oy, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-3;

    /// A 1:1 image fit inside a 2:1 box: the box's height is the constraining axis (Contain uses
    /// the smaller scale), so the fitted image is `avail_h` square, centered horizontally with a
    /// letterbox gap on either side and no vertical gap.
    #[test]
    fn fit_contain_square_into_wide_letterboxes_horizontally() {
        let (ox, oy, w, h) = fit_contain(100, 100, 200.0, 100.0).unwrap();
        assert!((w - 100.0).abs() < EPS && (h - 100.0).abs() < EPS, "fits the full square inside the height");
        assert!(oy.abs() < EPS, "no vertical letterbox");
        assert!(ox > EPS, "horizontal letterbox on both sides");
        assert!(ox + w <= 200.0 + EPS, "must fit inside the box, not overflow it");
    }

    /// A 2:1 image fit inside a 1:1 box: the box's width is now the constraining axis, so the
    /// letterbox flips to vertical.
    #[test]
    fn fit_contain_wide_into_square_letterboxes_vertically() {
        let (ox, oy, w, h) = fit_contain(200, 100, 100.0, 100.0).unwrap();
        assert!((w - 100.0).abs() < EPS && (h - 50.0).abs() < EPS);
        assert!(ox.abs() < EPS, "no horizontal letterbox");
        assert!(oy > EPS, "vertical letterbox on both sides");
        assert!(oy + h <= 100.0 + EPS, "must fit inside the box, not overflow it");
    }

    /// A same-aspect image fits the box exactly: no letterbox on either axis, offsets both ~0.
    #[test]
    fn fit_contain_exact_aspect_match_is_an_identity_fit() {
        let (ox, oy, w, h) = fit_contain(50, 50, 200.0, 200.0).unwrap();
        assert!(ox.abs() < EPS && oy.abs() < EPS);
        assert!((w - 200.0).abs() < EPS && (h - 200.0).abs() < EPS);
    }

    /// Every non-positive dimension — a zero-size image or a zero-size box — has nothing to fit
    /// into and must return `None` rather than dividing by zero or producing a degenerate rect.
    #[test]
    fn fit_contain_returns_none_for_any_zero_dimension() {
        assert_eq!(fit_contain(0, 100, 200.0, 100.0), None, "zero image width");
        assert_eq!(fit_contain(100, 0, 200.0, 100.0), None, "zero image height");
        assert_eq!(fit_contain(100, 100, 0.0, 100.0), None, "zero box width");
        assert_eq!(fit_contain(100, 100, 200.0, 0.0), None, "zero box height");
    }

    /// Round-trip: encode a tiny synthetic image to PNG bytes in memory, then confirm
    /// `decode_image` recovers the exact dimensions — exercises the real decode path headlessly,
    /// with no file system or `rfd` involved.
    #[test]
    fn decode_image_round_trips_a_small_in_memory_png() {
        let mut img = image::RgbaImage::new(3, 2);
        for px in img.pixels_mut() {
            px.0 = [10, 20, 30, 255];
        }
        let mut bytes = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();

        let decoded = decode_image(&bytes).expect("a valid in-memory PNG must decode");
        assert_eq!((decoded.width(), decoded.height()), (3, 2));
        assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30, 255]);
    }

    /// Malformed bytes must surface as a plain `Err`, never a panic — the non-fatal load-failure
    /// path `load_trace_image` relies on.
    #[test]
    fn decode_image_returns_an_error_for_malformed_bytes_without_panicking() {
        assert!(decode_image(b"not an image").is_err());
    }

    /// The `jpeg` Cargo feature is new in this change (`Cargo.toml`'s `image` dep gained it
    /// alongside `png`) — this proves the decoder is actually wired into the binary, not just
    /// declared in the manifest. Encodes a tiny synthetic image to JPEG bytes in memory (lossy, so
    /// pixel values are not asserted exactly — only that decoding succeeds and recovers the right
    /// dimensions), then confirms `decode_image` round-trips it, the same path `load_trace_image`
    /// exercises for a `.jpg`/`.jpeg` pick.
    #[test]
    fn decode_image_round_trips_a_small_in_memory_jpeg() {
        let mut img = image::RgbaImage::new(4, 3);
        for px in img.pixels_mut() {
            px.0 = [200, 50, 10, 255];
        }
        // JPEG has no alpha channel, so the encoder only accepts RGB8 — drop the (opaque) alpha
        // before encoding, matching how a real-world .jpg (never RGBA) would arrive.
        let rgb = image::DynamicImage::from(img).into_rgb8();
        let mut bytes = Vec::new();
        rgb.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .expect("the jpeg feature must actually encode, not just be declared in Cargo.toml");

        let decoded = decode_image(&bytes).expect("a valid in-memory JPEG must decode");
        assert_eq!((decoded.width(), decoded.height()), (4, 3));
    }

    /// A degenerate 1x1 source is the smallest possible image `fit_contain` can be asked to place —
    /// it must still scale up cleanly to fill the smaller box dimension rather than hitting a
    /// divide-by-zero or a NaN from the `img_w as f32`/`img_h as f32` division.
    #[test]
    fn fit_contain_of_a_1x1_pixel_source_scales_up_to_fill_the_smaller_box_dimension() {
        let (ox, oy, w, h) = fit_contain(1, 1, 200.0, 100.0).unwrap();
        assert!((w - 100.0).abs() < EPS && (h - 100.0).abs() < EPS, "a 1x1 source is square, so it scales to the smaller box axis");
        assert!(oy.abs() < EPS, "no vertical letterbox: the box's height is the constraining axis");
        assert!(ox > EPS && ox + w <= 200.0 + EPS, "centered horizontally, still fully inside the box");
    }

    /// An extreme aspect-ratio source (1000:1) fit into a square box exercises the far end of the
    /// `min(...)` scale — the fitted height collapses to a sliver but must still be positive and
    /// stay fully inside the box on both axes, not overflow from a rounding slip.
    #[test]
    fn fit_contain_of_an_extreme_aspect_ratio_source_still_fits_inside_the_box_on_both_axes() {
        let (ox, oy, w, h) = fit_contain(1000, 1, 100.0, 100.0).unwrap();
        assert!((w - 100.0).abs() < EPS, "width fills the box: the box's width is the constraining axis");
        assert!(h > 0.0 && h < 1.0, "height collapses to a sliver but must stay positive");
        assert!(ox.abs() < EPS, "no horizontal letterbox");
        assert!(oy > EPS && oy + h <= 100.0 + EPS, "vertical letterbox centers the sliver, still inside the box");
    }

    /// A 1:1 image covering a 2:1 box: Cover uses the *larger* scale candidate, so the box's width
    /// is what the image fills exactly, and the (now taller-than-the-box) height overflows and is
    /// cropped — the vertical offset goes negative. Contrast with `fit_contain`'s min-scale
    /// horizontal-letterbox result for the same inputs.
    #[test]
    fn fit_cover_square_into_wide_fills_width_and_crops_height() {
        let (ox, oy, w, h) = fit_cover(100, 100, 200.0, 100.0).unwrap();
        assert!((w - 200.0).abs() < EPS, "width fills the box exactly");
        assert!(h > 100.0 + EPS, "height overflows the box — this is the cropped axis");
        assert!(ox.abs() < EPS, "no horizontal crop: width fit the box exactly");
        assert!(oy < -EPS, "vertical offset goes negative: the overflow is centered and cropped");

        // Contrast with fit_contain on the identical inputs: min-scale letterboxes horizontally
        // instead, the opposite axis.
        let (cox, coy, cw, ch) = fit_contain(100, 100, 200.0, 100.0).unwrap();
        assert!(cw < w, "contain's fitted size must be smaller than cover's on the scaled axis");
        assert!(ch < h);
        assert!(coy.abs() < EPS && cox > EPS, "contain letterboxes horizontally instead of cropping vertically");
    }

    /// A 2:1 image covering a 1:1 box: the box's height is now what the image fills exactly, and
    /// the (now wider-than-the-box) width overflows and is cropped — the horizontal offset goes
    /// negative. Mirror image of the square-into-wide case above.
    #[test]
    fn fit_cover_wide_into_square_fills_height_and_crops_width() {
        let (ox, oy, w, h) = fit_cover(200, 100, 100.0, 100.0).unwrap();
        assert!((h - 100.0).abs() < EPS, "height fills the box exactly");
        assert!(w > 100.0 + EPS, "width overflows the box — this is the cropped axis");
        assert!(oy.abs() < EPS, "no vertical crop: height fit the box exactly");
        assert!(ox < -EPS, "horizontal offset goes negative: the overflow is centered and cropped");
    }

    /// A same-aspect image covers the box exactly: no crop on either axis, offsets both ~0 — an
    /// identity fit, same as `fit_contain` would produce for the same inputs (the two fits only
    /// diverge when the aspect ratios mismatch).
    #[test]
    fn fit_cover_exact_aspect_match_is_an_identity_fit() {
        let (ox, oy, w, h) = fit_cover(50, 50, 200.0, 200.0).unwrap();
        assert!(ox.abs() < EPS && oy.abs() < EPS);
        assert!((w - 200.0).abs() < EPS && (h - 200.0).abs() < EPS);
    }

    /// Every non-positive dimension has nothing to fit into — same guard as `fit_contain`.
    #[test]
    fn fit_cover_returns_none_for_any_zero_dimension() {
        assert_eq!(fit_cover(0, 100, 200.0, 100.0), None, "zero image width");
        assert_eq!(fit_cover(100, 0, 200.0, 100.0), None, "zero image height");
        assert_eq!(fit_cover(100, 100, 0.0, 100.0), None, "zero box width");
        assert_eq!(fit_cover(100, 100, 200.0, 0.0), None, "zero box height");
    }
}
