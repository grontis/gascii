//! Multi-frame raster export: animated GIF and a PNG spritesheet. Both stream through
//! `png_export::rasterize_frame_rgba8` one frame at a time — a GIF encode never holds more than
//! one frame's own RGBA8 buffer resident, and a spritesheet blits each frame's buffer into the
//! tiled canvas as it's produced rather than collecting them all first.

use std::time::Duration;

use gascii_core::{validate_gif_dimensions, validate_png_dimensions, validate_spritesheet_dimensions, Document, Rgba};
use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame};

use crate::png_export::{rasterize_frame_rgba8, PngExportAppError};

/// Rounds a millisecond duration to the nearest 10ms, floored at 10ms (never 0) — matches GIF's
/// own on-disk delay unit (centiseconds) and the timeline UI's existing 10ms step floor
/// (`timeline.rs::step_duration`), so a duration that already came from the UI round-trips exactly
/// through the encode and only an untrusted `.gascii`-file-sourced value needs the rounding at all.
fn round_delay_ms(dur_ms: u32) -> u64 {
    (((dur_ms as f64 / 10.0).round().max(1.0)) as u64) * 10
}

/// Encodes `doc` as an animated GIF at `cell_px` pixels per cell, honoring `doc.loop_playback` and
/// each frame's `resolved_frame_duration_ms`.
///
/// Loop mapping: `doc.loop_playback == true` calls `set_repeat(Repeat::Infinite)`, writing the
/// NETSCAPE2.0 loop extension; `false` skips the call entirely, so no extension is written — the
/// standard "play once" GIF encoding. Viewers key off the extension block's presence in the raw
/// bytes; `image`'s `AnimationDecoder::loop_count()` decodes both "no extension" and
/// `Repeat::Finite(0)` as `LoopCount::Infinite`, so the tests assert on the raw bytes, not the
/// decoder.
pub fn export_gif(
    doc: &Document,
    cell_px: u32,
    opaque_bg: Option<Rgba>,
    bg_image: Option<(&image::RgbaImage, f32)>,
) -> Result<Vec<u8>, PngExportAppError> {
    let (px_w, px_h) =
        validate_gif_dimensions(doc.width, doc.height, cell_px, doc.frame_count()).map_err(PngExportAppError::Dimensions)?;
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        if doc.loop_playback {
            encoder.set_repeat(Repeat::Infinite).map_err(|e| PngExportAppError::Encode(e.to_string()))?;
        }
        for i in 0..doc.frame_count() {
            let (_, _, pixels) = rasterize_frame_rgba8(doc, i, cell_px, opaque_bg, bg_image)?;
            let img = image::RgbaImage::from_raw(px_w, px_h, pixels)
                .expect("rasterize_frame_rgba8 returns a buffer sized exactly px_w * px_h * 4");
            let dur_ms = doc.resolved_frame_duration_ms(i).expect("i is always in 0..doc.frame_count()");
            let delay = Delay::from_saturating_duration(Duration::from_millis(round_delay_ms(dur_ms)));
            encoder
                .encode_frame(Frame::from_parts(img, 0, 0, delay))
                .map_err(|e| PngExportAppError::Encode(e.to_string()))?;
        }
    } // `encoder` (and its `&mut bytes` borrow) dropped here, before `bytes` is returned.
    Ok(bytes)
}

/// Tiles every frame of `doc`, rasterized at `cell_px` pixels per cell, into one PNG spritesheet
/// on a roughly-square auto grid (`cols = ceil(sqrt(frame_count))`, `rows` however many that
/// takes). A single-frame document degenerates to a 1x1 grid — a plain single-frame PNG.
pub fn export_spritesheet(
    doc: &Document,
    cell_px: u32,
    opaque_bg: Option<Rgba>,
    bg_image: Option<(&image::RgbaImage, f32)>,
) -> Result<Vec<u8>, PngExportAppError> {
    let n = doc.frame_count();
    let cols = (n as f64).sqrt().ceil().max(1.0) as u32;
    let rows = (n as u32).div_ceil(cols);
    let (frame_px_w, frame_px_h) =
        validate_png_dimensions(doc.width, doc.height, cell_px).map_err(PngExportAppError::Dimensions)?;
    let (sheet_w, sheet_h) =
        validate_spritesheet_dimensions(frame_px_w, frame_px_h, cols, rows).map_err(PngExportAppError::Dimensions)?;
    let mut sheet = image::RgbaImage::new(sheet_w, sheet_h);
    for i in 0..n {
        let (_, _, pixels) = rasterize_frame_rgba8(doc, i, cell_px, opaque_bg, bg_image)?;
        let tile = image::RgbaImage::from_raw(frame_px_w, frame_px_h, pixels)
            .expect("rasterize_frame_rgba8 returns a buffer sized exactly frame_px_w * frame_px_h * 4");
        let (col, row) = (i as u32 % cols, i as u32 / cols);
        image::imageops::overlay(&mut sheet, &tile, (col * frame_px_w) as i64, (row * frame_px_h) as i64);
    }
    let mut out = Vec::new();
    sheet
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| PngExportAppError::Encode(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::{add_frame, Cell, Frame as DocFrame, History};
    use image::AnimationDecoder;

    /// Builds an `n`-frame document, one cell (0,0) per frame set to a distinct solid opaque color
    /// from `colors` (also fills the whole cell's glyph as `'#'` at full bg so the rasterized tile
    /// is dominated by that color, tolerant of GIF's lossy quantization).
    fn doc_with_colored_frames(colors: &[Rgba]) -> Document {
        let mut doc = Document::new(2, 2);
        let mut history = History::new();
        for _ in 1..colors.len() {
            let edit = add_frame(&doc, doc.frame_count(), DocFrame::blank(2, 2)).unwrap();
            history.apply(&mut doc, edit);
        }
        for (i, &color) in colors.iter().enumerate() {
            doc.set_active_frame(i);
            for y in 0..2u16 {
                for x in 0..2u16 {
                    doc.set_cell(0, x, y, Cell { ch: '#', fg: color, bg: color });
                }
            }
        }
        doc.set_active_frame(0);
        doc
    }

    #[test]
    fn export_gif_surfaces_the_dimension_error_without_allocating_a_frame_buffer() {
        // The locked-in (80,25,16,196) case: a valid per-frame size, rejected on the joint
        // frame-count budget alone.
        let mut doc = Document::new(80, 25);
        let mut history = History::new();
        for _ in 1..196 {
            let edit = add_frame(&doc, doc.frame_count(), DocFrame::blank(80, 25)).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.frame_count(), 196);
        let err = export_gif(&doc, 16, None, None).unwrap_err();
        assert!(matches!(err, PngExportAppError::Dimensions(gascii_core::PngExportError::TooManyFrames { .. })));
    }

    /// A small 3-frame, 3-distinct-color document round-trips through `image`'s own GIF decoder:
    /// the decoded frame count matches, and each decoded frame contains at least one pixel
    /// reasonably close to its source frame's color (GIF quantizes — not asserted byte-exact).
    #[test]
    fn a_small_multi_color_document_round_trips_through_the_real_gif_decoder() {
        let colors = [Rgba(255, 0, 0, 255), Rgba(0, 255, 0, 255), Rgba(0, 0, 255, 255)];
        let doc = doc_with_colored_frames(&colors);
        let bytes = export_gif(&doc, 8, None, None).unwrap();

        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), colors.len());

        for (frame, &color) in frames.iter().zip(colors.iter()) {
            let close = |a: u8, b: u8| (a as i16 - b as i16).abs() <= 16;
            assert!(
                frame.buffer().pixels().any(|p| close(p.0[0], color.0) && close(p.0[1], color.1) && close(p.0[2], color.2)),
                "decoded frame must contain a pixel close to its source color {color:?}"
            );
        }
    }

    /// The loop mapping, verified against the raw encoded bytes rather than `image`'s own
    /// `loop_count()` (which cannot distinguish these two cases — see `export_gif`'s doc comment):
    /// `loop_playback == true` writes a NETSCAPE2.0 application-extension block; `false` writes
    /// none at all.
    #[test]
    fn loop_playback_true_writes_the_netscape_loop_extension_and_false_omits_it() {
        let mut looping = Document::new(2, 2);
        looping.loop_playback = true;
        let looping_bytes = export_gif(&looping, 4, None, None).unwrap();
        assert!(
            looping_bytes.windows(11).any(|w| w == b"NETSCAPE2.0"),
            "loop_playback == true must write a NETSCAPE2.0 loop extension"
        );

        let mut once = Document::new(2, 2);
        once.loop_playback = false;
        let once_bytes = export_gif(&once, 4, None, None).unwrap();
        assert!(
            !once_bytes.windows(11).any(|w| w == b"NETSCAPE2.0"),
            "loop_playback == false must write no loop extension at all"
        );
    }

    /// A `duration_override` that isn't a multiple of 10ms rounds to the nearest 10ms on encode,
    /// and one below the UI's 10ms floor (reachable only via a hand-built `Document`, since the
    /// timeline UI itself already enforces the floor) rounds up to 10ms, never down to 0ms.
    #[test]
    fn duration_rounds_to_the_nearest_10ms_and_floors_at_10ms_never_0ms() {
        assert_eq!(round_delay_ms(37), 40);
        assert_eq!(round_delay_ms(34), 30);
        assert_eq!(round_delay_ms(4), 10);
        assert_eq!(round_delay_ms(0), 10);

        let mut doc = Document::new(2, 2);
        let mut history = History::new();
        let edit = add_frame(&doc, 1, DocFrame::blank(2, 2)).unwrap();
        history.apply(&mut doc, edit);
        let edit = gascii_core::set_frame_duration(&doc, 1, Some(37)).unwrap().unwrap();
        history.apply(&mut doc, edit);

        let bytes = export_gif(&doc, 4, None, None).unwrap();
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        let frames = decoder.into_frames().collect_frames().unwrap();
        let (numer, denom) = frames[1].delay().numer_denom_ms();
        assert_eq!(numer / denom, 40, "37ms must round to the nearest 10ms (40)");
    }

    #[test]
    fn export_spritesheet_surfaces_the_dimension_error_without_allocating_a_tile_buffer() {
        let doc = Document::new(1024, 1024);
        let err = export_spritesheet(&doc, 48, None, None).unwrap_err();
        assert!(matches!(err, PngExportAppError::Dimensions(_)));
    }

    /// A degenerate single-frame document's spritesheet must still behave correctly, called
    /// directly (unreachable through the dialog, which gates the format on `frame_count() > 1`) —
    /// a 1x1 grid, degenerating to a plain single-frame PNG.
    #[test]
    fn a_single_frame_documents_spritesheet_is_a_1x1_grid() {
        let mut doc = Document::new(2, 2);
        doc.set_cell(0, 0, 0, Cell { ch: '#', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT });
        let bytes = export_spritesheet(&doc, 8, None, None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    /// A 3-frame document's spritesheet grid (`cols = ceil(sqrt(3)) = 2`, `rows = 2`) places frame
    /// index 1 at grid position (1, 0) — a known pixel inside that tile must match frame 1's known
    /// color, proving the actual blit math rather than just "some PNG came out."
    #[test]
    fn a_known_pixel_inside_frame_1s_tile_matches_that_frames_color() {
        let colors = [Rgba(255, 0, 0, 255), Rgba(0, 255, 0, 255), Rgba(0, 0, 255, 255)];
        let doc = doc_with_colored_frames(&colors);
        let bytes = export_spritesheet(&doc, 8, None, None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();

        let (frame_px_w, frame_px_h) = validate_png_dimensions(doc.width, doc.height, 8).unwrap();
        let (sheet_w, sheet_h) = validate_spritesheet_dimensions(frame_px_w, frame_px_h, 2, 2).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (sheet_w, sheet_h));

        // Frame 1 lands at grid (col=1, row=0): pixel deep inside that tile.
        let (px, py) = (frame_px_w + 4, 4);
        let c = colors[1];
        assert_eq!(decoded.get_pixel(px, py).0, [c.0, c.1, c.2, c.3]);
    }

    /// A `0ms` override floors to the 10ms minimum (never 0, which no real GIF viewer could play
    /// at all), and a large override (well above any UI-reachable value, but a hand-built
    /// `Document` can carry one) rounds to the nearest 10ms exactly like a small one does. Both
    /// verified through a real encode/decode round-trip, not just `round_delay_ms`'s own pure
    /// unit values (already covered by `duration_rounds_to_the_nearest_10ms_and_floors_at_10ms_
    /// never_0ms` above).
    #[test]
    fn a_zero_and_a_large_duration_override_round_trip_through_real_encode_decode() {
        let mut doc = Document::new(2, 2);
        let mut history = History::new();
        for _ in 1..3 {
            let edit = add_frame(&doc, doc.frame_count(), DocFrame::blank(2, 2)).unwrap();
            history.apply(&mut doc, edit);
        }
        let edit = gascii_core::set_frame_duration(&doc, 0, Some(0)).unwrap().unwrap();
        history.apply(&mut doc, edit);
        let edit = gascii_core::set_frame_duration(&doc, 2, Some(9999)).unwrap().unwrap();
        history.apply(&mut doc, edit);

        let bytes = export_gif(&doc, 4, None, None).unwrap();
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), 3);
        let ms = |f: &image::Frame| {
            let (numer, denom) = f.delay().numer_denom_ms();
            numer / denom
        };
        assert_eq!(ms(&frames[0]), 10, "0ms floors up to the 10ms minimum, never encodes as 0");
        assert_eq!(ms(&frames[2]), 10000, "9999ms rounds to the nearest 10ms (10000)");
    }

    /// Documents the known `image`-crate-level limitation `export_gif`'s own doc comment describes:
    /// `AnimationDecoder::loop_count()` cannot distinguish "no NETSCAPE2.0 extension written" from
    /// "an extension requesting 0 repetitions" -- both decode as `LoopCount::Infinite`, regardless
    /// of `doc.loop_playback`. The raw-byte scan
    /// (`loop_playback_true_writes_the_netscape_loop_extension_and_false_omits_it`) is the only way
    /// to observe the real difference; this test locks in that the high-level decode-level API
    /// genuinely cannot, so a future `image` upgrade that changes this doesn't silently go unnoticed.
    #[test]
    fn both_loop_states_decode_identically_as_infinite_via_the_high_level_loop_count_api() {
        use image::metadata::LoopCount;

        let mut looping = Document::new(2, 2);
        looping.loop_playback = true;
        let looping_bytes = export_gif(&looping, 4, None, None).unwrap();
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(looping_bytes)).unwrap();
        assert!(matches!(decoder.loop_count(), LoopCount::Infinite));

        let mut once = Document::new(2, 2);
        once.loop_playback = false;
        let once_bytes = export_gif(&once, 4, None, None).unwrap();
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(once_bytes)).unwrap();
        assert!(matches!(decoder.loop_count(), LoopCount::Infinite));
    }

    /// GIF's palette is capped at 256 colors per frame; a document using more than that many
    /// distinct colors in one frame must not crash or produce a corrupt file -- the encoder
    /// quantizes automatically, and this test only asserts the encode/decode pipeline survives
    /// that intact (dimensions, frame count), not byte-exact color fidelity (an accepted,
    /// documented GIF limitation, not a GASCII bug -- see the architect plan's D-gif note).
    #[test]
    fn a_document_with_more_than_256_distinct_colors_in_one_frame_exports_a_valid_gif_without_crashing() {
        let (w, h) = (17u16, 16u16);
        let n = w as usize * h as usize; // 272 cells, each given its own color below.
        // `r` alone distinguishes every index within a 256-wide band; `g` distinguishes which band
        // (`i / 256`) an index falls in -- together the `(r, g)` pair is unique for every `i` in
        // `0..272`, so all 272 colors are guaranteed distinct (a naive `i % 256`-only scheme would
        // alias indices 256 places apart onto the same color, silently falling short of >256).
        let colors: Vec<Rgba> =
            (0..n).map(|i| Rgba((i % 256) as u8, (i / 256) as u8 * 200, ((i * 13) % 256) as u8, 255)).collect();
        let distinct: std::collections::HashSet<_> = colors.iter().map(|c| (c.0, c.1, c.2)).collect();
        assert!(distinct.len() > 256, "sanity: the test fixture itself must exceed GIF's 256-color palette");

        let mut doc = Document::new(w, h);
        for (i, &color) in colors.iter().enumerate() {
            let (x, y) = ((i % w as usize) as u16, (i / w as usize) as u16);
            doc.set_cell(0, x, y, Cell { ch: '#', fg: color, bg: color });
        }

        let bytes = export_gif(&doc, 2, None, None).unwrap();
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), 1);
        let (expected_w, expected_h) = validate_png_dimensions(w, h, 2).unwrap();
        assert_eq!((frames[0].buffer().width(), frames[0].buffer().height()), (expected_w, expected_h));
    }

    /// Spritesheet grid math (`cols = ceil(sqrt(n))`, `rows = n.div_ceil(cols)`) for every frame
    /// count from 2 through 5 -- not just the 1-frame (degenerate) and 3-frame cases already
    /// covered above. Each frame's known color must land at its own computed grid tile, proving
    /// the blit math for the remainder case (5 frames: a 3x2 grid with one empty tile) too.
    #[test]
    fn spritesheet_grid_layout_is_correct_for_2_3_4_and_5_frame_documents() {
        const POOL: [Rgba; 5] =
            [Rgba(255, 0, 0, 255), Rgba(0, 255, 0, 255), Rgba(0, 0, 255, 255), Rgba(255, 255, 0, 255), Rgba(0, 255, 255, 255)];
        for (n, expected_cols, expected_rows) in [(2usize, 2u32, 1u32), (3, 2, 2), (4, 2, 2), (5, 3, 2)] {
            let colors = &POOL[..n];
            let doc = doc_with_colored_frames(colors);
            let bytes = export_spritesheet(&doc, 6, None, None).unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();

            let (frame_px_w, frame_px_h) = validate_png_dimensions(doc.width, doc.height, 6).unwrap();
            let (sheet_w, sheet_h) = validate_spritesheet_dimensions(frame_px_w, frame_px_h, expected_cols, expected_rows).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (sheet_w, sheet_h), "{n}-frame grid dimensions");

            for (i, &color) in colors.iter().enumerate() {
                let (col, row) = (i as u32 % expected_cols, i as u32 / expected_cols);
                let (px, py) = (col * frame_px_w + 4, row * frame_px_h + 4);
                assert_eq!(
                    decoded.get_pixel(px, py).0,
                    [color.0, color.1, color.2, color.3],
                    "{n}-frame doc: frame {i} must land at grid ({col},{row})"
                );
            }
        }
    }
}
