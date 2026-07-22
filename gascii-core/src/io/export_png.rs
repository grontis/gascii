//! PNG export dimension math. Pure arithmetic, no image/font dependency — `gascii-core` stays
//! headless. The app crate (`gascii/src/png_export.rs`) owns rasterization and encoding, calling
//! `validate_png_dimensions` first and never allocating a pixel buffer it hasn't authorized.

/// Sane upper bound on total output pixels (~100MP, ~400MB as an RGBA8 buffer) — keeps a
/// user-chosen cell scale from driving an unbounded allocation attempt, the same untrusted-size
/// class as the `.gascii` loader's extent cap and paste's dimension clamp.
pub const MAX_PNG_PIXELS: u64 = 100_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PngExportError {
    ZeroScale,
    TooLarge { width_px: u64, height_px: u64, max_pixels: u64 },
    TooManyFrames { frame_px_w: u64, frame_px_h: u64, frame_count: usize, max_pixels: u64 },
}

/// Computes target pixel dimensions for a `width x height` cell document at `cell_px` pixels per
/// cell, rejecting before any pixel buffer is described as OK to allocate. `width`/`height` are
/// trusted document dimensions (already capped at `Document::MAX_WIDTH`/`MAX_HEIGHT`); `cell_px`
/// is the untrusted piece — a user-chosen scale that, multiplied against a max-size document, can
/// overflow or demand an enormous allocation. All multiplication happens in `u64` to stay
/// overflow-safe regardless of input.
pub fn validate_png_dimensions(width: u16, height: u16, cell_px: u32) -> Result<(u32, u32), PngExportError> {
    if cell_px == 0 {
        return Err(PngExportError::ZeroScale);
    }
    let w = width as u64 * cell_px as u64;
    let h = height as u64 * cell_px as u64;
    if w > u32::MAX as u64 || h > u32::MAX as u64 || w.saturating_mul(h) > MAX_PNG_PIXELS {
        return Err(PngExportError::TooLarge { width_px: w, height_px: h, max_pixels: MAX_PNG_PIXELS });
    }
    Ok((w as u32, h as u32))
}

/// Validates one GIF frame's pixel dimensions (via `validate_png_dimensions`, unchanged) AND the
/// joint per-frame-times-frame-count pixel budget a streamed multi-frame GIF encode represents.
/// Streaming (only one frame's RGBA8 buffer resident at a time — see `gascii::anim_export::
/// export_gif`) already makes per-frame memory use safe without a new per-frame cap; this second
/// check instead bounds worst-case blocking-encode *latency* to roughly one `MAX_PNG_PIXELS`-sized
/// PNG export's worth of pixel work, by reusing that exact budget against the new frame-count axis
/// rather than letting it multiply unbounded (mirrors `Document::MAX_TOTAL_CELLS`'s own precedent
/// of bounding a new multiplicative axis against an existing single-instance budget).
pub fn validate_gif_dimensions(
    width: u16,
    height: u16,
    cell_px: u32,
    frame_count: usize,
) -> Result<(u32, u32), PngExportError> {
    let (w, h) = validate_png_dimensions(width, height, cell_px)?;
    let total = (w as u64).saturating_mul(h as u64).saturating_mul(frame_count as u64);
    if total > MAX_PNG_PIXELS {
        return Err(PngExportError::TooManyFrames {
            frame_px_w: w as u64,
            frame_px_h: h as u64,
            frame_count,
            max_pixels: MAX_PNG_PIXELS,
        });
    }
    Ok((w, h))
}

/// Validates a spritesheet's final tiled-canvas dimensions — `frame_px_w`/`frame_px_h` (one
/// frame's already-validated size, from `validate_png_dimensions`) times a `cols`x`rows` grid —
/// before the tiled canvas is allocated. Reuses `TooLarge` (not `TooManyFrames`): a spritesheet's
/// rejection genuinely is "this rectangle is too large," the same shape `validate_png_dimensions`
/// already reports.
pub fn validate_spritesheet_dimensions(
    frame_px_w: u32,
    frame_px_h: u32,
    cols: u32,
    rows: u32,
) -> Result<(u32, u32), PngExportError> {
    let w = frame_px_w as u64 * cols as u64;
    let h = frame_px_h as u64 * rows as u64;
    if w > u32::MAX as u64 || h > u32::MAX as u64 || w.saturating_mul(h) > MAX_PNG_PIXELS {
        return Err(PngExportError::TooLarge { width_px: w, height_px: h, max_pixels: MAX_PNG_PIXELS });
    }
    Ok((w as u32, h as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_scale_is_rejected() {
        assert_eq!(validate_png_dimensions(80, 25, 0), Err(PngExportError::ZeroScale));
    }

    #[test]
    fn a_max_size_document_at_a_sane_scale_is_accepted() {
        // 1024x1024 at 16px/cell would be a huge image (16384x16384 = ~268MP) — exceeds the cap,
        // so use a smaller, still-representative scale that stays under it for this assertion.
        let (w, h) = validate_png_dimensions(1024, 1024, 8).unwrap();
        assert_eq!(w, 1024 * 8);
        assert_eq!(h, 1024 * 8);
    }

    #[test]
    fn a_typical_document_at_a_typical_scale_is_accepted() {
        let (w, h) = validate_png_dimensions(80, 25, 16).unwrap();
        assert_eq!(w, 80 * 16);
        assert_eq!(h, 25 * 16);
    }

    #[test]
    fn a_max_size_document_at_an_absurd_scale_is_rejected() {
        let result = validate_png_dimensions(1024, 1024, 1000);
        assert!(matches!(result, Err(PngExportError::TooLarge { .. })));
    }

    #[test]
    fn overflow_safe_multiplication_never_panics_on_extreme_inputs() {
        // u16::MAX dims x a large cell_px must not panic — only ever accept or cleanly reject.
        let result = std::panic::catch_unwind(|| validate_png_dimensions(u16::MAX, u16::MAX, u32::MAX));
        assert!(result.is_ok(), "must not panic");
        assert!(matches!(result.unwrap(), Err(PngExportError::TooLarge { .. })));
    }

    #[test]
    fn boundary_at_the_pixel_cap_accepts_at_or_under_and_rejects_over() {
        // At cell_px=1, width_px/height_px equal width/height directly: 10000 x 10000 =
        // 100,000,000 exactly == MAX_PNG_PIXELS: accepted.
        assert!(validate_png_dimensions(10000, 10000, 1).is_ok());
        // One row over the cap: rejected.
        let result = validate_png_dimensions(10000, 10001, 1);
        assert!(matches!(result, Err(PngExportError::TooLarge { .. })));
    }

    #[test]
    fn width_or_height_exceeding_u32_max_pixels_is_rejected_without_panicking() {
        let result = std::panic::catch_unwind(|| validate_png_dimensions(u16::MAX, 1, u32::MAX));
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), Err(PngExportError::TooLarge { .. })));
    }

    /// A max-size document at the largest offered UI preset (48px/cell, `PNG_SCALE_PRESETS` in
    /// `gascii/src/app.rs`) — the concrete, real-world-reachable overflow case, not just the
    /// synthetic `u32::MAX` inputs the tests above already prove don't panic.
    #[test]
    fn a_max_size_document_at_the_largest_ui_preset_scale_is_rejected_cleanly() {
        let result = validate_png_dimensions(1024, 1024, 48);
        assert!(matches!(result, Err(PngExportError::TooLarge { .. })));
    }

    /// A zero-width or zero-height request (unreachable through the shipped app, since
    /// `Document::new` itself panics on either dimension being 0 — but `validate_png_dimensions`
    /// is a public `gascii-core` function any caller could call directly) must not panic and must
    /// not authorize a pixel buffer for a nonsensical degenerate request.
    #[test]
    fn zero_width_or_zero_height_does_not_panic_and_reports_a_zero_sized_result_rather_than_erroring() {
        // Not rejected as an error today (only cell_px==0 and the pixel cap are checked) — this
        // test locks in that documented current behavior (0 pixels is trivially under the cap) so
        // a future change to add a width/height==0 check is a deliberate, visible decision, not an
        // accidental behavior change caught only by a rasterizer crash downstream.
        assert_eq!(validate_png_dimensions(0, 25, 16), Ok((0, 400)));
        assert_eq!(validate_png_dimensions(80, 0, 16), Ok((1280, 0)));
    }

    // `validate_gif_dimensions` tests.

    /// Per-frame px = 80*16 x 25*16 = 1280x400 = 512,000. 512,000*195 = 99,840,000 <=
    /// MAX_PNG_PIXELS (100,000,000): accepted. 512,000*196 = 100,352,000 > cap: rejected. Exact
    /// boundary values, not just a `matches!` shape check.
    #[test]
    fn boundary_at_the_joint_frame_budget_accepts_at_or_under_and_rejects_over() {
        assert!(validate_gif_dimensions(80, 25, 16, 195).is_ok());
        let result = validate_gif_dimensions(80, 25, 16, 196);
        assert!(matches!(result, Err(PngExportError::TooManyFrames { .. })));
    }

    /// `frame_count == 0` is trivially under budget — the pure function must not special-case it,
    /// even though `Document` always has at least one frame in practice.
    #[test]
    fn zero_frame_count_is_accepted_as_trivially_under_budget() {
        assert!(validate_gif_dimensions(80, 25, 16, 0).is_ok());
    }

    #[test]
    fn validate_gif_dimensions_overflow_safe_on_extreme_inputs() {
        let result =
            std::panic::catch_unwind(|| validate_gif_dimensions(u16::MAX, u16::MAX, u32::MAX, usize::MAX));
        assert!(result.is_ok(), "must not panic");
        assert!(matches!(result.unwrap(), Err(PngExportError::TooLarge { .. } | PngExportError::TooManyFrames { .. })));
    }

    #[test]
    fn a_zero_scale_gif_request_is_rejected_the_same_as_a_zero_scale_png_request() {
        assert_eq!(validate_gif_dimensions(80, 25, 0, 10), Err(PngExportError::ZeroScale));
    }

    // `validate_spritesheet_dimensions` tests — mirrors `validate_png_dimensions`'s own shapes.

    #[test]
    fn a_typical_spritesheet_grid_is_accepted() {
        let (w, h) = validate_spritesheet_dimensions(80, 40, 3, 2).unwrap();
        assert_eq!((w, h), (240, 80));
    }

    #[test]
    fn boundary_at_the_spritesheet_pixel_cap_accepts_at_or_under_and_rejects_over() {
        // 10000 x 10000 = 100,000,000 exactly == MAX_PNG_PIXELS: accepted, via a 100x100 grid of
        // 100x100px frames.
        assert!(validate_spritesheet_dimensions(100, 100, 100, 100).is_ok());
        // One frame-row over: rejected.
        let result = validate_spritesheet_dimensions(100, 100, 100, 101);
        assert!(matches!(result, Err(PngExportError::TooLarge { .. })));
    }

    #[test]
    fn validate_spritesheet_dimensions_overflow_safe_on_extreme_inputs() {
        let result =
            std::panic::catch_unwind(|| validate_spritesheet_dimensions(u32::MAX, u32::MAX, u32::MAX, u32::MAX));
        assert!(result.is_ok(), "must not panic");
        assert!(matches!(result.unwrap(), Err(PngExportError::TooLarge { .. })));
    }
}
