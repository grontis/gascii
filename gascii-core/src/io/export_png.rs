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
}
