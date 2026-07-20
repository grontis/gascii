//! PNG export: composites a `Document` (via `gascii_core::composite`) and rasterizes each cell's
//! glyph through `fontdue`, encoding the result via `image`. `gascii-core` stays headless — the
//! only thing it contributes is `validate_png_dimensions`, which this module treats as the sole
//! authority on whether a pixel buffer may be allocated at all.

use gascii_core::{composite, validate_png_dimensions, Document, Rgba};

#[derive(Debug)]
pub enum PngExportAppError {
    Dimensions(gascii_core::PngExportError),
    Font(String),
    Encode(String),
}

impl std::fmt::Display for PngExportAppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PngExportAppError::Dimensions(e) => write!(f, "invalid export dimensions: {e:?}"),
            PngExportAppError::Font(e) => write!(f, "font rasterization failed: {e}"),
            PngExportAppError::Encode(e) => write!(f, "PNG encode failed: {e}"),
        }
    }
}

/// Standard "over" alpha compositing of a straight-alpha `src` onto a straight-alpha `dst` pixel
/// (`image::Rgba<u8>`'s `.0` array), returning the resulting straight-alpha pixel.
///
/// PNG stores straight (non-premultiplied) alpha, so the color channels must be un-premultiplied
/// by dividing through by the result's own alpha: `out_c = (src_c*src_a + dst_c*dst_a*(1-src_a)) /
/// out_a`. Skipping that division (storing `src_c*src_a + dst_c*(1-src_a)` directly) only happens
/// to be correct at the `src_a == 1` or `dst_a == 1` boundaries — every anti-aliased glyph edge
/// composited over a non-opaque cell background is `src_a < 1` and `dst_a < 1`, so the division is
/// required. Guards `out_a == 0` (both source and destination fully transparent) to avoid a
/// divide-by-zero; the result is fully transparent black in that case, which is unobservable in
/// the final PNG regardless of which RGB triple is chosen.
fn composite_over(src: Rgba, dst: [u8; 4]) -> [u8; 4] {
    let src_a = src.3 as f32 / 255.0;
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return [0, 0, 0, 0];
    }
    let mix = |sc: u8, dc: u8| -> u8 {
        let sc_f = sc as f32 / 255.0;
        let dc_f = dc as f32 / 255.0;
        let out_c = (sc_f * src_a + dc_f * dst_a * (1.0 - src_a)) / out_a;
        (out_c * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [mix(src.0, dst[0]), mix(src.1, dst[1]), mix(src.2, dst[2]), (out_a * 255.0).round() as u8]
}

/// Standard "over" alpha compositing of `color` (straight alpha) onto `img`'s pixel at `(x,y)`.
/// A no-op for a fully transparent `color` (also the correct result of `composite_over` in that
/// case, since `src_a == 0` leaves `dst` unchanged by the formula — this is purely a fast path).
fn blend_pixel(img: &mut image::RgbaImage, x: u32, y: u32, color: Rgba) {
    if color.3 == 0 {
        return;
    }
    let px = img.get_pixel_mut(x, y);
    px.0 = composite_over(color, px.0);
}

/// Scales `src`'s RGB by its own per-pixel alpha, returning a premultiplied copy. `image`'s own
/// `imageops::resize` assumes premultiplied alpha for any image whose alpha isn't constant across
/// pixels — resizing straight alpha directly lets a transparent pixel's (otherwise invisible) RGB
/// bleed into its opaque neighbors near a soft or hard alpha edge. A fully-opaque source (`a == 255`
/// everywhere) is unchanged by this step, so the common "backdrop photo" case is unaffected.
fn premultiply(src: &image::RgbaImage) -> image::RgbaImage {
    let mut out = src.clone();
    for px in out.pixels_mut() {
        let a = px.0[3] as f32 / 255.0;
        px.0[0] = (px.0[0] as f32 * a).round() as u8;
        px.0[1] = (px.0[1] as f32 * a).round() as u8;
        px.0[2] = (px.0[2] as f32 * a).round() as u8;
    }
    out
}

/// Reverses `premultiply` on a single resized pixel, returning straight alpha for the blend loop
/// below (which, via `blend_pixel`/`composite_over`, expects straight alpha throughout). `a == 0`
/// has no recoverable color (the premultiplied RGB is `0` regardless of what it originally was) and
/// is returned as fully transparent black, matching `composite_over`'s own `out_a == 0` guard.
fn unpremultiply(p: [u8; 4]) -> [u8; 4] {
    let a = p[3];
    if a == 0 {
        return [0, 0, 0, 0];
    }
    let un = |c: u8| -> u8 { (c as f32 * 255.0 / a as f32).round().clamp(0.0, 255.0) as u8 };
    [un(p[0]), un(p[1]), un(p[2]), a]
}

/// Rasterizes `doc`'s composited cells at `cell_px` pixels per cell into a straight-alpha RGBA8
/// pixel buffer (row-major, `4 * width * height` bytes) plus its `(width, height)`. `opaque_bg`
/// pre-fills every pixel with that color before compositing cell content over it (`None` keeps the
/// buffer transparent, so a cell's own transparent bg stays transparent in the result). `bg_image`
/// — `(source, opacity)` — is composited next, beneath the cells and above `opaque_bg`: the source
/// is resized to *cover* (fill, crop the overflow, see `image_bg::fit_cover`) the `px_w`×`px_h`
/// frame and blended in at `opacity`. The resize itself runs in premultiplied-alpha space (see
/// `premultiply`/`unpremultiply`) so a translucent source's soft edges don't fringe. `None` skips it
/// entirely (today's export, unchanged).
///
/// The pure pixel-math half of PNG export, split out from encoding so the export dialog's live
/// preview can upload these bytes straight into an egui texture without a PNG encode/decode round
/// trip.
pub fn rasterize_rgba8(
    doc: &Document,
    cell_px: u32,
    opaque_bg: Option<Rgba>,
    bg_image: Option<(&image::RgbaImage, f32)>,
) -> Result<(u32, u32, Vec<u8>), PngExportAppError> {
    let (px_w, px_h) = validate_png_dimensions(doc.width, doc.height, cell_px)
        .map_err(PngExportAppError::Dimensions)?;
    let composited = composite(doc);
    let font = fontdue::Font::from_bytes(crate::fonts::CANVAS_FONT_BYTES, fontdue::FontSettings::default())
        .map_err(|e| PngExportAppError::Font(e.to_string()))?;
    let mut img = image::RgbaImage::new(px_w, px_h);
    if let Some(bg) = opaque_bg {
        for px in img.pixels_mut() {
            px.0 = [bg.0, bg.1, bg.2, bg.3];
        }
    }
    if let Some((src, opacity)) = bg_image {
        // Cover (see `image_bg::fit_cover`): the fitted rect fills px_w×px_h and overflows on one
        // axis with a negative offset; the bounds check below crops that overflow, so no
        // transparent letterbox gap remains — every export pixel gets a source sample.
        if let Some((ox, oy, w, h)) = crate::image_bg::fit_cover(src.width(), src.height(), px_w as f32, px_h as f32) {
            let (fw, fh) = ((w.round() as u32).max(1), (h.round() as u32).max(1));
            let premultiplied = premultiply(src);
            let resized = image::imageops::resize(&premultiplied, fw, fh, image::imageops::FilterType::Triangle);
            let (ox, oy) = (ox.round() as i64, oy.round() as i64);
            for ry in 0..resized.height() as i64 {
                for rx in 0..resized.width() as i64 {
                    let (dx, dy) = (ox + rx, oy + ry);
                    if dx < 0 || dy < 0 || dx as u32 >= px_w || dy as u32 >= px_h {
                        continue;
                    }
                    let p = unpremultiply(resized.get_pixel(rx as u32, ry as u32).0);
                    let a = (p[3] as f32 * opacity).round().clamp(0.0, 255.0) as u8;
                    blend_pixel(&mut img, dx as u32, dy as u32, Rgba(p[0], p[1], p[2], a));
                }
            }
        }
    }
    let ascent = font
        .horizontal_line_metrics(cell_px as f32)
        .map(|m| m.ascent)
        .unwrap_or(cell_px as f32 * 0.8);

    for y in 0..doc.height {
        for x in 0..doc.width {
            let cell = composited[y as usize][x as usize];
            let cell_x0 = x as i64 * cell_px as i64;
            let cell_y0 = y as i64 * cell_px as i64;

            if cell.bg.3 > 0 {
                for py in 0..cell_px as i64 {
                    for pxo in 0..cell_px as i64 {
                        let (px, py2) = (cell_x0 + pxo, cell_y0 + py);
                        if px >= 0 && py2 >= 0 && (px as u32) < px_w && (py2 as u32) < px_h {
                            blend_pixel(&mut img, px as u32, py2 as u32, cell.bg);
                        }
                    }
                }
            }

            if cell.ch != ' ' {
                let (metrics, bitmap) = font.rasterize(cell.ch, cell_px as f32);
                let origin_x = cell_x0 + metrics.xmin as i64;
                let origin_y = cell_y0 + ascent.round() as i64 - metrics.height as i64 - metrics.ymin as i64;
                for gy in 0..metrics.height {
                    for gx in 0..metrics.width {
                        let coverage = bitmap[gy * metrics.width + gx];
                        if coverage == 0 {
                            continue;
                        }
                        let px = origin_x + gx as i64;
                        let py = origin_y + gy as i64;
                        if px < 0 || py < 0 || px as u32 >= px_w || py as u32 >= px_h {
                            continue;
                        }
                        // Combine the glyph's per-pixel coverage with the cell's own fg alpha, so
                        // a translucent fg color still attenuates the glyph correctly.
                        let alpha = (coverage as f32 / 255.0) * (cell.fg.3 as f32 / 255.0);
                        let a_byte = (alpha * 255.0).round() as u8;
                        if a_byte == 0 {
                            continue;
                        }
                        blend_pixel(&mut img, px as u32, py as u32, Rgba(cell.fg.0, cell.fg.1, cell.fg.2, a_byte));
                    }
                }
            }
        }
    }

    Ok((px_w, px_h, img.into_raw()))
}

/// Rasterizes `doc`'s composited cells at `cell_px` pixels per cell into PNG bytes. Blank cells
/// (and any cell whose bg is fully transparent) leave the output transparent at that pixel when
/// `opaque_bg` is `None` — the PNG carries no baked-in editor chrome background unless the caller
/// asks for one (the "transparent background" checkbox unchecked, which passes `Some(doc.background)`).
/// `bg_image`, see `rasterize_rgba8`.
pub fn export_png(
    doc: &Document,
    cell_px: u32,
    opaque_bg: Option<Rgba>,
    bg_image: Option<(&image::RgbaImage, f32)>,
) -> Result<Vec<u8>, PngExportAppError> {
    let (px_w, px_h, pixels) = rasterize_rgba8(doc, cell_px, opaque_bg, bg_image)?;
    let img = image::RgbaImage::from_raw(px_w, px_h, pixels)
        .expect("rasterize_rgba8 returns a buffer sized exactly px_w * px_h * 4");
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| PngExportAppError::Encode(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::Cell;

    /// A fully-covered glyph pixel (the interior of a solid block character, `src_a == 255`)
    /// composited over a fully transparent cell background must reproduce the cell's own fg color
    /// exactly — `composite_over`'s un-premultiply divides by `out_a == src_a`, which cancels
    /// exactly at full coverage, so this is a deterministic "known-fg pixel" check reachable
    /// headlessly (unlike an anti-aliased glyph edge, whose coverage — and thus exact color — is a
    /// font-rasterizer implementation detail this test must not depend on).
    #[test]
    fn a_fully_covered_glyph_pixel_reproduces_the_cells_exact_fg_color_over_a_transparent_background() {
        let mut doc = doc_with(1, 1);
        let fg = Rgba(10, 20, 30, 255);
        doc.set_cell(0, 0, 0, Cell { ch: '█', fg, bg: Rgba::TRANSPARENT });
        let bytes = export_png(&doc, 32, None, None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert!(
            decoded.pixels().any(|p| p.0 == [fg.0, fg.1, fg.2, fg.3]),
            "a full block glyph must rasterize at least one fully-opaque pixel matching the cell's exact fg color"
        );
    }

    /// A known-bg pixel, at a corner far from where any glyph coverage lands, must be the cell's
    /// exact opaque bg color — locks the bg fill loop's own color output (not just "some non-
    /// transparent pixel exists", which `opaque_background_fills_the_entire_cell` above already
    /// covers for the whole-cell case) alongside the glyph-color check above.
    #[test]
    fn a_corner_pixel_of_an_opaque_background_cell_matches_the_exact_bg_color() {
        let mut doc = doc_with(1, 1);
        let bg = Rgba(10, 20, 30, 255);
        doc.set_cell(0, 0, 0, Cell { ch: ' ', fg: Rgba::WHITE, bg });
        let bytes = export_png(&doc, 16, None, None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [bg.0, bg.1, bg.2, bg.3]);
    }

    fn doc_with(w: u16, h: u16) -> Document {
        Document::new(w, h)
    }

    #[test]
    fn exported_png_dimensions_match_validate_png_dimensions() {
        let doc = doc_with(10, 4);
        let bytes = export_png(&doc, 16, None, None).expect("export must succeed for a small document");
        let decoded = image::load_from_memory(&bytes).expect("must decode as a valid image");
        let (expected_w, expected_h) = validate_png_dimensions(doc.width, doc.height, 16).unwrap();
        assert_eq!(decoded.width(), expected_w);
        assert_eq!(decoded.height(), expected_h);
    }

    #[test]
    fn all_blank_document_exports_a_fully_transparent_image_at_the_requested_size() {
        let doc = doc_with(4, 4);
        let bytes = export_png(&doc, 8, None, None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert!(decoded.pixels().all(|p| p.0[3] == 0), "an all-blank document must export fully transparent");
    }

    /// `opaque_bg` pre-fills every pixel before compositing — a blank document with a non-
    /// transparent `opaque_bg` must export fully opaque at that exact color (the "Transparent
    /// background" checkbox unchecked path), not the fully-transparent result `None` produces.
    #[test]
    fn opaque_bg_pre_fills_a_blank_document_instead_of_leaving_it_transparent() {
        let doc = doc_with(3, 3);
        let bg = Rgba(10, 20, 30, 255);
        let bytes = export_png(&doc, 8, Some(bg), None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert!(decoded.pixels().all(|p| p.0 == [bg.0, bg.1, bg.2, bg.3]));
    }

    /// `rasterize_rgba8`'s dimensions and pixel count must agree with `validate_png_dimensions` and
    /// its own declared buffer length — the export dialog's preview builds an `egui::ColorImage`
    /// straight from these bytes with no further validation.
    #[test]
    fn rasterize_rgba8_returns_a_buffer_sized_exactly_width_times_height_times_4() {
        let doc = doc_with(5, 3);
        let (w, h, pixels) = rasterize_rgba8(&doc, 4, None, None).unwrap();
        assert_eq!((w, h), (20, 12));
        assert_eq!(pixels.len(), (w * h * 4) as usize);
    }

    #[test]
    fn oversized_request_surfaces_the_dimension_error_without_allocating() {
        let doc = doc_with(1024, 1024);
        let err = export_png(&doc, 1000, None, None).unwrap_err();
        assert!(matches!(err, PngExportAppError::Dimensions(_)));
    }

    #[test]
    fn a_painted_cell_produces_a_visibly_non_transparent_region() {
        let mut doc = doc_with(1, 1);
        doc.set_cell(0, 0, 0, Cell { ch: '#', fg: Rgba(255, 255, 255, 255), bg: Rgba::TRANSPARENT });
        let bytes = export_png(&doc, 16, None, None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert!(decoded.pixels().any(|p| p.0[3] > 0), "a drawn glyph must rasterize to at least one visible pixel");
    }

    #[test]
    fn opaque_background_fills_the_entire_cell() {
        let mut doc = doc_with(1, 1);
        doc.set_cell(0, 0, 0, Cell { ch: ' ', fg: Rgba::WHITE, bg: Rgba(10, 20, 30, 255) });
        let bytes = export_png(&doc, 8, None, None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert!(decoded.pixels().all(|p| p.0 == [10, 20, 30, 255]));
    }

    // `composite_over` regression tests: hand-computed straight-alpha "over" results, kept as
    // fixed expected values rather than re-derived at test time, so a regression to the old
    // premultiplied-but-stored-straight bug is caught.

    #[test]
    fn partial_alpha_source_over_a_fully_transparent_dest_reproduces_the_sources_own_straight_color() {
        // src_a = 128/255 ≈ 0.502. Un-premultiplying by out_a (== src_a, since dst_a == 0) cancels
        // out exactly, so the stored color must equal the source's own straight RGB — not the
        // source scaled down by its own alpha (the old bug's result would have been [100,50,25,128]).
        let result = composite_over(Rgba(200, 100, 50, 128), [0, 0, 0, 0]);
        assert_eq!(result, [200, 100, 50, 128]);
    }

    #[test]
    fn partial_alpha_source_over_a_partial_alpha_dest_un_premultiplies_correctly() {
        // src red @ a=0.4 (102/255) over dst green @ a=0.6 (153/255).
        let result = composite_over(Rgba(255, 0, 0, 102), [0, 255, 0, 153]);
        assert_eq!(result, [134, 121, 0, 194]);
    }

    #[test]
    fn partial_alpha_source_over_an_opaque_dest_matches_the_simple_boundary_case() {
        // dst_a == 1 is the one case the old (buggy) formula got right by coincidence; confirms
        // the corrected formula still agrees there.
        let result = composite_over(Rgba(255, 0, 0, 102), [0, 0, 255, 255]);
        assert_eq!(result, [102, 0, 153, 255]);
    }

    #[test]
    fn fully_transparent_source_is_a_no_op_through_blend_pixel() {
        let mut img = image::RgbaImage::new(1, 1);
        img.get_pixel_mut(0, 0).0 = [10, 20, 30, 200];
        blend_pixel(&mut img, 0, 0, Rgba(255, 255, 255, 0));
        assert_eq!(img.get_pixel(0, 0).0, [10, 20, 30, 200]);
    }

    #[test]
    fn fully_transparent_source_over_fully_transparent_dest_guards_the_divide_by_zero() {
        // Both src_a and dst_a are 0, so out_a == 0 — `composite_over` must guard the division
        // rather than producing NaN/panic, and return a fully transparent pixel.
        let result = composite_over(Rgba(0, 0, 0, 0), [0, 0, 0, 0]);
        assert_eq!(result, [0, 0, 0, 0]);
    }

    /// Cross-feature: a document's own custom `background`, carried through a real (anchored, not
    /// just top-left) `resize_document` grow, must show up exactly at the newly created cells when
    /// exported opaque — those cells are `Cell::BLANK` (transparent) after the resize, not a
    /// literal copy of `doc.background`, so this pins that the app's own `opaque_bg` convention
    /// (`(!transparent).then_some(doc.background)`, the exact expression `run_export` and
    /// `refresh_export_preview` both use) is what makes "new cells fill with background" true at
    /// the pixel level, not just at the cell-storage level. The "Transparent background" checkbox
    /// checked (`None`) must leave that same newly grown region genuinely transparent instead.
    #[test]
    fn a_custom_background_grown_into_by_an_anchored_resize_fills_the_new_cells_when_exported_opaque() {
        use gascii_core::{AxisAnchor, ResizeAnchor};

        let mut doc = doc_with(2, 2);
        doc.background = Rgba(30, 60, 90, 255);
        doc.set_cell(0, 0, 0, Cell { ch: 'a', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 1, 1, Cell { ch: 'z', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });

        // Center/Center grow to 6x6: old content lands at (2,2)-(3,3); every other cell is a
        // newly created Blank cell this resize introduced.
        let anchor = ResizeAnchor { h: AxisAnchor::Center, v: AxisAnchor::Center };
        let edit = gascii_core::resize_document(&doc, 6, 6, anchor).unwrap().unwrap();
        let mut history = gascii_core::History::new();
        history.apply(&mut doc, edit);
        assert_eq!(doc.cell(0, 0, 0), Some(&Cell::BLANK), "sanity: (0,0) is a newly created cell, not old content");

        // Opaque export ("Transparent background" unchecked): the app's own convention.
        let opaque_bg = Some(doc.background);
        let opaque_bytes = export_png(&doc, 8, opaque_bg, None).unwrap();
        let opaque = image::load_from_memory(&opaque_bytes).unwrap().to_rgba8();
        let (px, py) = (2, 2); // inside the (0,0) cell's 8x8 pixel block
        assert_eq!(
            opaque.get_pixel(px, py).0,
            [30, 60, 90, 255],
            "a newly-grown Blank cell must render as the document's own background when exported opaque"
        );

        // Transparent export ("Transparent background" checked): the same newly-grown region must
        // stay genuinely transparent, not silently pick up the background anyway.
        let transparent_bytes = export_png(&doc, 8, None, None).unwrap();
        let transparent = image::load_from_memory(&transparent_bytes).unwrap().to_rgba8();
        assert_eq!(transparent.get_pixel(px, py).0[3], 0, "the same cell must be transparent when opaque_bg is None");
    }

    // Export background (Cover) blend tests. Every source used here is a single uniform color, so
    // bilinear resize (`FilterType::Triangle`) cannot introduce interpolation drift into the
    // assertions — any exported pixel touched by the source must equal that exact color.

    fn uniform_source(w: u32, h: u32, color: [u8; 4]) -> image::RgbaImage {
        image::RgbaImage::from_pixel(w, h, image::Rgba(color))
    }

    /// A wide (4x1) uniform source composited at full opacity over a blank, square-ish document
    /// must fill *every* exported pixel — including all four corners — with the source color at
    /// full alpha. The source is deliberately non-square over a square canvas so Cover is forced to
    /// crop the wide axis; a Contain regression here would leave transparent letterbox bars at the
    /// top/bottom corners and fail this assertion.
    #[test]
    fn a_wide_uniform_source_at_full_opacity_covers_every_pixel_including_every_corner() {
        let doc = doc_with(4, 4); // cell_px 8 -> 32x32 px, square
        let src = uniform_source(4, 1, [255, 0, 0, 255]);
        let bytes = export_png(&doc, 8, None, Some((&src, 1.0))).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        let (w, h) = (decoded.width(), decoded.height());
        let corners = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)];
        for (x, y) in corners {
            assert_eq!(
                decoded.get_pixel(x, y).0,
                [255, 0, 0, 255],
                "corner ({x},{y}) must be fully covered by the source, not left as a transparent letterbox gap"
            );
        }
        assert_eq!(decoded.get_pixel(w / 2, h / 2).0, [255, 0, 0, 255], "center pixel must also be the source color");
    }

    /// Opacity 0.0 must blend nothing in — the export is byte-identical to the same document
    /// exported with no background image at all.
    #[test]
    fn zero_opacity_background_image_is_byte_identical_to_the_no_image_export() {
        let doc = doc_with(4, 4);
        let src = uniform_source(4, 1, [255, 0, 0, 255]);
        let with_zero_opacity = export_png(&doc, 8, None, Some((&src, 0.0))).unwrap();
        let without_image = export_png(&doc, 8, None, None).unwrap();
        assert_eq!(with_zero_opacity, without_image, "opacity 0.0 must produce byte-identical output to no bg_image at all");
    }

    /// A fully-opaque cell painted over the covering background image must win at its own pixels —
    /// cells composite last, on top of the image, matching the live trace overlay's z-order.
    #[test]
    fn a_fully_opaque_cell_wins_over_the_background_image_beneath_it() {
        let mut doc = doc_with(4, 4);
        let cell_color = Rgba(10, 20, 30, 255);
        doc.set_cell(0, 0, 0, Cell { ch: ' ', fg: Rgba::WHITE, bg: cell_color });
        let src = uniform_source(4, 1, [255, 0, 0, 255]);
        let bytes = export_png(&doc, 8, None, Some((&src, 1.0))).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        // Center of the (0,0) cell's 8x8 pixel block.
        assert_eq!(
            decoded.get_pixel(4, 4).0,
            [cell_color.0, cell_color.1, cell_color.2, cell_color.3],
            "the opaque cell must fully cover the background image at its own pixels"
        );
    }

    /// A degenerate zero-height source must not panic and must fall back to the no-image result —
    /// `fit_cover` guards this case and returns `None`, so the blend loop is skipped entirely.
    #[test]
    fn a_zero_height_source_does_not_panic_and_produces_the_no_image_result() {
        let doc = doc_with(4, 4);
        let degenerate = image::RgbaImage::new(4, 0);
        let bytes = export_png(&doc, 8, None, Some((&degenerate, 1.0))).unwrap();
        let without_image = export_png(&doc, 8, None, None).unwrap();
        assert_eq!(bytes, without_image, "a degenerate source must be skipped, not panic or partially blend");
    }

    // `premultiply`/`unpremultiply` regression tests: the two-step's own math, isolated from the
    // resize call so a rounding regression is caught directly rather than only through a full export.

    #[test]
    fn premultiply_scales_rgb_by_the_pixels_own_alpha() {
        let mut img = image::RgbaImage::new(1, 1);
        img.get_pixel_mut(0, 0).0 = [200, 100, 50, 128];
        // Same source values as `composite_over`'s own partial-alpha regression test above —
        // 128/255 ≈ 0.502, so this pins the identical rounding behavior in the premultiply step.
        assert_eq!(premultiply(&img).get_pixel(0, 0).0, [100, 50, 25, 128]);
    }

    #[test]
    fn premultiply_then_unpremultiply_round_trips_a_fully_opaque_pixel_exactly() {
        let mut img = image::RgbaImage::new(1, 1);
        img.get_pixel_mut(0, 0).0 = [12, 34, 56, 255];
        let premultiplied = premultiply(&img);
        assert_eq!(
            unpremultiply(premultiplied.get_pixel(0, 0).0),
            [12, 34, 56, 255],
            "a == 255 makes premultiply a no-op, so un-premultiplying must recover the exact source pixel"
        );
    }

    #[test]
    fn unpremultiply_of_a_fully_transparent_pixel_is_fully_transparent_black_not_a_divide_by_zero() {
        assert_eq!(unpremultiply([10, 20, 30, 0]), [0, 0, 0, 0]);
    }

    /// A source with a hard alpha edge — half opaque green, half fully-transparent "hidden red" (its
    /// alpha is 0, so the color is never meant to be seen) — composited opaque over a contrasting
    /// blue backdrop. Neither the opaque half's green nor the blue backdrop carries any red channel,
    /// so a straight-alpha resize's fringe (the old bug: the hidden red leaking into the Triangle
    /// filter's blend of the two source halves near the boundary) is the *only* way any exported
    /// pixel can end up with a non-zero red channel — a regression to un-premultiplied resize fails
    /// the whole-image scan below. The interior samples pin the flat regions; the red-channel scan
    /// covers wherever the filter's blend zone actually falls, without hard-coding the exact
    /// boundary pixel (robust to bilinear edge softening).
    #[test]
    fn a_hard_alpha_edge_source_does_not_bleed_the_hidden_colour_of_its_transparent_half() {
        let doc = doc_with(8, 8); // cell_px 8 -> 64x64 px, square
        let mut src = image::RgbaImage::new(8, 8); // square, matches the doc's aspect: identity fit, no crop
        for (x, _y, px) in src.enumerate_pixels_mut() {
            px.0 = if x < 4 { [0, 255, 0, 255] } else { [255, 0, 0, 0] };
        }
        let opaque_bg = Rgba(0, 0, 255, 255);
        let bytes = export_png(&doc, 8, Some(opaque_bg), Some((&src, 1.0))).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();

        assert_eq!(
            decoded.get_pixel(8, 32).0,
            [0, 255, 0, 255],
            "deep in the opaque half (far from the boundary), the exported pixel is the source's own green"
        );
        assert_eq!(
            decoded.get_pixel(56, 32).0,
            [opaque_bg.0, opaque_bg.1, opaque_bg.2, opaque_bg.3],
            "deep in the transparent half, no source alpha survives — the opaque backdrop shows through"
        );
        assert!(
            decoded.pixels().all(|p| p.0[0] == 0),
            "neither the opaque green half nor the blue backdrop carries any red channel — any non-zero \
             red pixel is the transparent half's hidden colour bleeding through a straight-alpha resize"
        );
    }

    /// The same hard-alpha-edge/hidden-red trap as the test above, but this time the source's aspect
    /// (8x2, wide) forces `fit_cover` to produce a genuine *negative offset* against a square doc —
    /// unlike the identity-fit case above (a square source over a square doc, `ox == oy == 0`, no
    /// crop), here both edges of the resized buffer are actually clipped by the blit loop's bounds
    /// check (`ox = -32` at 16x upscale: resized rx 0..32 and 96..128 are dropped, only rx 32..96
    /// survives into the destination). This exercises the premultiply/resize/un-premultiply fix
    /// together with real cropping, not just a straight pass-through resize — the two code paths
    /// (Cover's negative-offset crop arithmetic and the premultiplied-alpha resize) interact here in
    /// a way the identity-fit test above cannot catch on its own.
    #[test]
    fn a_hard_alpha_edge_source_with_a_real_cover_crop_does_not_bleed_the_hidden_colour_either() {
        let doc = doc_with(8, 4); // cell_px 8 -> 64x32 px
        let mut src = image::RgbaImage::new(8, 2); // wide source: fit_cover(8,2,64,32) crops horizontally
        for (x, _y, px) in src.enumerate_pixels_mut() {
            px.0 = if x < 4 { [0, 255, 0, 255] } else { [255, 0, 0, 0] };
        }
        let opaque_bg = Rgba(0, 0, 255, 255);
        let bytes = export_png(&doc, 8, Some(opaque_bg), Some((&src, 1.0))).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();

        // Sanity: fit_cover really does crop here (not an identity fit like the test above).
        let (_ox, _oy, w, h) = crate::image_bg::fit_cover(8, 2, 64.0, 32.0).unwrap();
        assert!(w > 64.0, "sanity: the fitted width must overflow the doc, forcing a real horizontal crop");
        assert!((h - 32.0).abs() < 1e-3, "sanity: height fits exactly, only width is cropped");

        assert_eq!(
            decoded.get_pixel(8, 16).0,
            [0, 255, 0, 255],
            "deep in the visible opaque region (post-crop), the exported pixel is the source's own green"
        );
        assert_eq!(
            decoded.get_pixel(48, 16).0,
            [opaque_bg.0, opaque_bg.1, opaque_bg.2, opaque_bg.3],
            "deep in the visible transparent region (post-crop), no source alpha survives — the backdrop shows through"
        );
        assert!(
            decoded.pixels().all(|p| p.0[0] == 0),
            "the crop must not expose the hidden red either — neither the visible source region nor the \
             backdrop carries any red channel, so any non-zero red pixel is a bleed surviving the crop"
        );
    }
}
