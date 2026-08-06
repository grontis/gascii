use eframe::egui::{self, Pos2, Rect, Vec2};

use gascii_core::DocExtent;

pub const ZOOM_SCALES: [f32; 6] = [0.5, 1.0, 1.5, 2.0, 3.0, 4.0];

pub struct Viewport {
    pub zoom_step: usize,  // index into ZOOM_SCALES
    pub pan: Vec2,         // screen-space pixel offset of cell (0,0)
    pub base_font_px: f32, // unscaled glyph px (e.g. 16.0)
    /// A continuous scale from `fit_to_window`, overriding the preset ladder until the next
    /// manual zoom snaps back onto it. Fit is the one operation whose ideal scale is almost
    /// never a preset — snapping it down a whole step can waste a third of the window.
    fit_scale: Option<f32>,
    /// `(scale().to_bits(), pixels_per_point.to_bits(), cell_size)` from the last measurement, so
    /// steady-state frames skip the font query. DPI is part of the key because nothing in
    /// `row_height`'s contract promises DPI-independence, even though epaint 0.35 happens to
    /// compute it that way. Bit-equality is safe: both keys are copied config values, not ones
    /// accumulating float error.
    cached_cell: Option<(u32, u32, Vec2)>,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            zoom_step: 1, // ZOOM_SCALES[1] == 1.0
            pan: Vec2::ZERO,
            base_font_px: 16.0,
            fit_scale: None,
            cached_cell: None,
        }
    }
}

impl Viewport {
    pub fn scale(&self) -> f32 {
        debug_assert!(
            self.zoom_step < ZOOM_SCALES.len(),
            "zoom_step out of range for ZOOM_SCALES"
        );
        self.fit_scale.unwrap_or(ZOOM_SCALES[self.zoom_step])
    }

    pub fn font_px(&self) -> f32 {
        self.base_font_px * self.scale()
    }

    /// Advance width + row height at the current font size. Monospace ⇒ uniform advance.
    /// Re-queries egui's font metrics only when the zoom step or DPI scale factor changed.
    pub fn cell_size(&mut self, ctx: &egui::Context) -> Vec2 {
        let ppp_bits = ctx.pixels_per_point().to_bits();
        let scale_bits = self.scale().to_bits();
        if let Some((s_bits, bits, cell)) = self.cached_cell {
            if s_bits == scale_bits && bits == ppp_bits {
                return cell;
            }
        }
        let fid = crate::fonts::canvas_font_id(self.font_px());
        let cell = ctx.fonts_mut(|f| Vec2::new(f.glyph_width(&fid, 'M'), f.row_height(&fid)));
        self.cached_cell = Some((scale_bits, ppp_bits, cell));
        cell
    }

    pub fn cell_to_screen(&self, x: u16, y: u16, cell: Vec2, origin: Pos2) -> Pos2 {
        origin + self.pan + Vec2::new(x as f32 * cell.x, y as f32 * cell.y)
    }

    /// `None` if the position is left/above the origin, or resolves to a cell outside
    /// `doc_extent` (matches `Document::in_bounds`).
    pub fn screen_to_cell(
        &self,
        p: Pos2,
        cell: Vec2,
        origin: Pos2,
        doc_extent: DocExtent,
    ) -> Option<(u16, u16)> {
        let rel = p - (origin + self.pan);
        if rel.x < 0.0 || rel.y < 0.0 || cell.x <= 0.0 || cell.y <= 0.0 {
            return None;
        }
        let x = (rel.x / cell.x).floor();
        let y = (rel.y / cell.y).floor();
        if x > u16::MAX as f32 || y > u16::MAX as f32 {
            return None;
        }
        let (x, y) = (x as u16, y as u16);
        if x >= doc_extent.width || y >= doc_extent.height {
            return None;
        }
        Some((x, y))
    }

    /// Like `screen_to_cell`, but clamps to `[0, w-1] x [0, h-1]` instead of returning `None` when
    /// the point falls outside the doc's screen-space rect. Used only for active-stroke targeting
    /// so a drag off the canvas keeps drawing to the edge instead of stalling.
    pub fn screen_to_cell_clamped(
        &self,
        p: Pos2,
        cell: Vec2,
        origin: Pos2,
        doc_extent: DocExtent,
    ) -> (u16, u16) {
        if doc_extent.width == 0 || doc_extent.height == 0 || cell.x <= 0.0 || cell.y <= 0.0 {
            return (0, 0);
        }
        let rel = p - (origin + self.pan);
        let x = (rel.x / cell.x).floor();
        let y = (rel.y / cell.y).floor();

        let max_x = (doc_extent.width - 1) as f32;
        let max_y = (doc_extent.height - 1) as f32;
        let x = x.clamp(0.0, max_x) as u16;
        let y = y.clamp(0.0, max_y) as u16;
        (x, y)
    }

    /// Clamp the clip rect to doc bounds for culling.
    pub fn visible_cell_rect(
        &self,
        clip: Rect,
        cell: Vec2,
        origin: Pos2,
        doc_extent: DocExtent,
    ) -> (u16, u16, u16, u16) {
        if cell.x <= 0.0 || cell.y <= 0.0 {
            return (0, 0, 0, 0);
        }
        let rel_min = clip.min - (origin + self.pan);
        let rel_max = clip.max - (origin + self.pan);

        let x0 = (rel_min.x / cell.x).floor().max(0.0) as u16;
        let y0 = (rel_min.y / cell.y).floor().max(0.0) as u16;
        let x1_raw = (rel_max.x / cell.x).ceil();
        let y1_raw = (rel_max.y / cell.y).ceil();

        let x1 = if x1_raw < 0.0 {
            0
        } else {
            (x1_raw as u32).min(doc_extent.width as u32) as u16
        };
        let y1 = if y1_raw < 0.0 {
            0
        } else {
            (y1_raw as u32).min(doc_extent.height as u32) as u16
        };

        let x0 = x0.min(doc_extent.width);
        let y0 = y0.min(doc_extent.height);
        let x1 = x1.max(x0);
        let y1 = y1.max(y0);

        (x0, y0, x1, y1)
    }

    /// Step zoom keeping the cell under the cursor fixed (adjust `pan`).
    ///
    /// `cell` is the cell size measured *before* the zoom step. Since `font_px` scales linearly
    /// with `scale()`, the post-zoom cell size is approximated as `cell * (new_scale/old_scale)`;
    /// the caller's next-frame `cell_size(ctx)` re-measurement settles any font-hinting rounding.
    pub fn zoom_at(&mut self, cursor: Pos2, dir: i32, cell: Vec2, origin: Pos2) {
        let before = self.screen_to_cell_f(cursor, cell, origin);
        let old_scale = self.scale();

        // A continuous fit scale sits between rungs of the preset ladder; a manual step snaps to
        // the nearest preset strictly in the step's direction. The tolerance absorbs the fit's
        // float error, so a fit that landed on a preset behaves as that preset.
        const SNAP: f32 = 1e-3;
        let new_step = match (self.fit_scale, dir.signum()) {
            (Some(fit), 1) => ZOOM_SCALES
                .iter()
                .position(|&s| s > fit + SNAP)
                .unwrap_or(ZOOM_SCALES.len() - 1),
            (Some(fit), -1) => ZOOM_SCALES
                .iter()
                .rposition(|&s| s < fit - SNAP)
                .unwrap_or(0),
            (None, d) => {
                (self.zoom_step as i32 + d).clamp(0, ZOOM_SCALES.len() as i32 - 1) as usize
            }
            _ => return,
        };
        let left_fit = self.fit_scale.take().is_some();
        if new_step == self.zoom_step && !left_fit {
            return;
        }
        self.zoom_step = new_step;

        let ratio = self.scale() / old_scale;
        let new_cell = cell * ratio;
        self.reanchor(cursor, before, new_cell, origin);
    }

    fn screen_to_cell_f(&self, p: Pos2, cell: Vec2, origin: Pos2) -> Vec2 {
        let rel = p - (origin + self.pan);
        Vec2::new(rel.x / cell.x, rel.y / cell.y)
    }

    /// Adjust pan so that fractional cell coordinate `target_cell` renders at screen point `p`;
    /// the next frame's `cell_size` re-measurement settles any residual drift.
    ///
    /// `desired` is an *absolute* screen position, so the new pan must be *set* to `p - desired`,
    /// never accumulated — `+=` would re-add the pan already baked into `desired`, losing
    /// cursor-anchoring after any prior pan or a second consecutive zoom.
    fn reanchor(&mut self, p: Pos2, target_cell: Vec2, cell: Vec2, origin: Pos2) {
        let desired = origin + Vec2::new(target_cell.x * cell.x, target_cell.y * cell.y);
        self.pan = p - desired;
    }

    /// Fit the full doc extent to `available` inset by `margin` on every side, then center via
    /// `pan`.
    ///
    /// The scale is *continuous* — clamped to the preset ladder's ends but otherwise free —
    /// because the largest preset that fits can waste a third of the window on a wide document.
    /// Cell metrics aren't exactly linear in font px (hinting), so the guess from the base
    /// metrics gets one measured correction, then a shave loop as a backstop; the correction aims
    /// 0.5% under the room so float residue can never push the card out of bounds. A later
    /// manual zoom snaps off the fitted scale onto the ladder (see `zoom_at`).
    ///
    /// `margin` is applied to the fit test but NOT to the centering: the document is centered in the
    /// whole canvas area, and the margin only guarantees the desk keeps showing around it rather
    /// than the card butting up against the panel edges. Shrinking `available` for both would push
    /// the document off-centre by half the margin.
    pub fn fit_to_window(
        &mut self,
        available: Vec2,
        margin: f32,
        doc_extent: DocExtent,
        ctx: &egui::Context,
    ) {
        let room = Vec2::new(
            (available.x - margin * 2.0).max(1.0),
            (available.y - margin * 2.0).max(1.0),
        );
        let (w, h) = (
            doc_extent.width.max(1) as f32,
            doc_extent.height.max(1) as f32,
        );
        let (min_scale, max_scale) = (ZOOM_SCALES[0], ZOOM_SCALES[ZOOM_SCALES.len() - 1]);

        let measure = |scale: f32| {
            let fid = crate::fonts::canvas_font_id(self.base_font_px * scale);
            ctx.fonts_mut(|f| Vec2::new(f.glyph_width(&fid, 'M'), f.row_height(&fid)))
        };
        let refit = |cell: Vec2, at: f32| {
            ((room.x / (w * cell.x)).min(room.y / (h * cell.y)) * at * 0.995)
                .clamp(min_scale, max_scale)
        };

        let mut scale = refit(measure(1.0), 1.0);
        scale = refit(measure(scale), scale);
        let mut cell = measure(scale);
        for _ in 0..8 {
            if (w * cell.x <= room.x && h * cell.y <= room.y) || scale <= min_scale {
                break;
            }
            scale = (scale * 0.99).max(min_scale);
            cell = measure(scale);
        }

        self.fit_scale = Some(scale);
        // The ladder position a later manual step zooms from: the largest preset not above the
        // fit, so `-` always shrinks and `+` always grows from what's on screen.
        self.zoom_step = ZOOM_SCALES
            .iter()
            .rposition(|&z| z <= scale + 1e-3)
            .unwrap_or(0);
        self.cached_cell = Some((
            self.scale().to_bits(),
            ctx.pixels_per_point().to_bits(),
            cell,
        ));

        let doc_w = w * cell.x;
        let doc_h = h * cell.y;
        self.pan = Vec2::new((available.x - doc_w) / 2.0, (available.y - doc_h) / 2.0);
    }
}

/// Thin delegation to `Viewport`'s own inherent methods — the two facts a plugin's `CanvasRenderer`
/// needs, without handing it the rest of `Viewport`'s app-only surface.
impl gascii_plugin_api::CellGrid for Viewport {
    fn cell_to_screen(&self, x: u16, y: u16, cell: Vec2, origin: Pos2) -> Pos2 {
        Viewport::cell_to_screen(self, x, y, cell, origin)
    }

    fn font_px(&self) -> f32 {
        Viewport::font_px(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> Vec2 {
        Vec2::new(10.0, 20.0)
    }
    fn origin() -> Pos2 {
        Pos2::new(0.0, 0.0)
    }
    /// A doc extent generous enough not to constrain tests that aren't exercising bounds-clamping.
    fn big_doc() -> DocExtent {
        DocExtent {
            width: 1000,
            height: 1000,
        }
    }

    #[test]
    fn scale_and_font_px() {
        let vp = Viewport::default();
        assert_eq!(vp.scale(), 1.0);
        assert_eq!(vp.font_px(), 16.0);
    }

    /// The `CellGrid` impl must be pure delegation — a plugin's renderer calling through the trait
    /// object must see exactly what `gascii`'s own code sees calling the inherent methods directly.
    #[test]
    fn viewport_cell_grid_impl_matches_its_inherent_methods() {
        use gascii_plugin_api::CellGrid;

        let vp = Viewport {
            zoom_step: 2,
            pan: Vec2::new(12.0, -7.0),
            ..Viewport::default()
        };
        let grid: &dyn CellGrid = &vp;

        assert_eq!(grid.font_px(), vp.font_px());
        assert_eq!(
            grid.cell_to_screen(5, 9, cell(), origin()),
            vp.cell_to_screen(5, 9, cell(), origin())
        );
    }

    #[test]
    fn cell_to_screen_round_trip() {
        let vp = Viewport::default();
        let p = vp.cell_to_screen(3, 4, cell(), origin());
        let back = vp.screen_to_cell(p, cell(), origin(), big_doc()).unwrap();
        assert_eq!(back, (3, 4));
    }

    #[test]
    fn cell_to_screen_round_trip_with_pan() {
        let vp = Viewport {
            pan: Vec2::new(15.0, -7.0),
            ..Viewport::default()
        };
        for (x, y) in [(0u16, 0u16), (5, 9), (79, 24)] {
            let p = vp.cell_to_screen(x, y, cell(), origin());
            let back = vp.screen_to_cell(p, cell(), origin(), big_doc()).unwrap();
            assert_eq!(back, (x, y));
        }
    }

    #[test]
    fn screen_to_cell_negative_is_none() {
        let vp = Viewport::default();
        assert_eq!(
            vp.screen_to_cell(Pos2::new(-1.0, 5.0), cell(), origin(), big_doc()),
            None
        );
        assert_eq!(
            vp.screen_to_cell(Pos2::new(5.0, -1.0), cell(), origin(), big_doc()),
            None
        );
    }

    #[test]
    fn screen_to_cell_out_of_doc_bounds_is_none() {
        let vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 80,
            height: 25,
        };

        // Just inside the last in-bounds cell (79, 24) at 10x20 cell size.
        let inside = vp.cell_to_screen(79, 24, cell(), origin()) + Vec2::new(1.0, 1.0);
        assert_eq!(
            vp.screen_to_cell(inside, cell(), origin(), doc_extent),
            Some((79, 24))
        );

        // One cell past the right edge and one cell past the bottom edge — both out of bounds
        // even though they resolve to valid, in-range-looking floor()'d coordinates.
        let past_right = vp.cell_to_screen(80, 0, cell(), origin()) + Vec2::new(1.0, 1.0);
        assert_eq!(
            vp.screen_to_cell(past_right, cell(), origin(), doc_extent),
            None
        );

        let past_bottom = vp.cell_to_screen(0, 25, cell(), origin()) + Vec2::new(1.0, 1.0);
        assert_eq!(
            vp.screen_to_cell(past_bottom, cell(), origin(), doc_extent),
            None
        );
    }

    #[test]
    fn visible_cell_rect_clamps_to_doc_bounds() {
        let vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 5,
            height: 3,
        };
        // clip much larger than doc
        let clip = Rect::from_min_max(Pos2::new(-50.0, -50.0), Pos2::new(500.0, 500.0));
        let (x0, y0, x1, y1) = vp.visible_cell_rect(clip, cell(), origin(), doc_extent);
        assert_eq!((x0, y0, x1, y1), (0, 0, 5, 3));
    }

    #[test]
    fn visible_cell_rect_partial_clip() {
        let vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 100,
            height: 100,
        };
        // clip covers roughly cells [1,3) x [0,2)
        let clip = Rect::from_min_max(Pos2::new(10.0, 0.0), Pos2::new(29.0, 39.0));
        let (x0, y0, x1, y1) = vp.visible_cell_rect(clip, cell(), origin(), doc_extent);
        assert_eq!(x0, 1);
        assert_eq!(y0, 0);
        assert_eq!(x1, 3);
        assert_eq!(y1, 2);
    }

    #[test]
    fn screen_to_cell_clamped_matches_screen_to_cell_inside_bounds() {
        let vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 80,
            height: 25,
        };
        let p = vp.cell_to_screen(10, 5, cell(), origin()) + Vec2::new(1.0, 1.0);
        let unclamped = vp.screen_to_cell(p, cell(), origin(), doc_extent);
        let clamped = vp.screen_to_cell_clamped(p, cell(), origin(), doc_extent);
        assert_eq!(unclamped, Some(clamped));
    }

    #[test]
    fn screen_to_cell_clamped_left_or_above_origin_clamps_to_zero() {
        let vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 80,
            height: 25,
        };
        assert_eq!(
            vp.screen_to_cell_clamped(Pos2::new(-100.0, 5.0), cell(), origin(), doc_extent),
            (0, 0)
        );
        assert_eq!(
            vp.screen_to_cell_clamped(Pos2::new(5.0, -100.0), cell(), origin(), doc_extent),
            (0, 0)
        );
    }

    #[test]
    fn screen_to_cell_clamped_past_right_or_bottom_clamps_to_max() {
        let vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 80,
            height: 25,
        };
        assert_eq!(
            vp.screen_to_cell_clamped(Pos2::new(100_000.0, 5.0), cell(), origin(), doc_extent),
            (79, 0)
        );
        assert_eq!(
            vp.screen_to_cell_clamped(Pos2::new(5.0, 100_000.0), cell(), origin(), doc_extent),
            (0, 24)
        );
        assert_eq!(
            vp.screen_to_cell_clamped(
                Pos2::new(100_000.0, 100_000.0),
                cell(),
                origin(),
                doc_extent
            ),
            (79, 24)
        );
    }

    #[test]
    fn screen_to_cell_clamped_consistent_across_zoom_steps() {
        let doc_extent = DocExtent {
            width: 40,
            height: 20,
        };
        for zoom_step in 0..ZOOM_SCALES.len() {
            let vp = Viewport {
                zoom_step,
                ..Viewport::default()
            };
            let cell = Vec2::new(10.0 * vp.scale(), 20.0 * vp.scale());
            let p = vp.cell_to_screen(5, 5, cell, origin()) + Vec2::new(1.0, 1.0);
            assert_eq!(
                vp.screen_to_cell_clamped(p, cell, origin(), doc_extent),
                (5, 5)
            );
            // off-canvas stays clamped to the last valid cell at every zoom step
            assert_eq!(
                vp.screen_to_cell_clamped(
                    Pos2::new(1_000_000.0, 1_000_000.0),
                    cell,
                    origin(),
                    doc_extent
                ),
                (39, 19)
            );
        }
    }

    #[test]
    fn zoom_at_clamps_at_top() {
        let mut vp = Viewport {
            zoom_step: ZOOM_SCALES.len() - 1,
            ..Viewport::default()
        };
        vp.zoom_at(Pos2::new(5.0, 5.0), 1, cell(), origin());
        assert_eq!(vp.zoom_step, ZOOM_SCALES.len() - 1);
    }

    #[test]
    fn zoom_at_clamps_at_bottom() {
        let mut vp = Viewport {
            zoom_step: 0,
            ..Viewport::default()
        };
        vp.zoom_at(Pos2::new(5.0, 5.0), -1, cell(), origin());
        assert_eq!(vp.zoom_step, 0);
    }

    /// Headless `egui::Context`: `set_fonts` only stages definitions, and `fonts`/`fonts_mut`
    /// panic until the first pass has run — so we drive one empty `run_ui` pass first to apply
    /// the staged Iosevka Fixed registration before querying any glyph metrics.
    fn headless_ctx_with_canvas_font() -> egui::Context {
        let ctx = egui::Context::default();
        crate::fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |_ui| {});
        ctx
    }

    #[test]
    fn cell_size_is_cached_across_calls_at_same_zoom_step() {
        let ctx = headless_ctx_with_canvas_font();
        let mut vp = Viewport::default();
        let first = vp.cell_size(&ctx);
        let second = vp.cell_size(&ctx);
        assert_eq!(first, second);
        assert_eq!(
            vp.cached_cell,
            Some((
                vp.scale().to_bits(),
                ctx.pixels_per_point().to_bits(),
                first
            ))
        );
    }

    #[test]
    fn cell_size_cache_key_tracks_pixels_per_point() {
        // Verifies the cache re-keys on a DPI change. Deliberately does NOT assert the returned
        // `Vec2` differs: epaint 0.35 computes `row_height` independently of `pixels_per_point`
        // (it only feeds glyph rasterization), so the values are identical today — but that's an
        // implementation detail, not a contract, hence the key still tracks DPI.
        let ctx = headless_ctx_with_canvas_font();
        let mut vp = Viewport::default();
        let _ = vp.cell_size(&ctx);
        let (scale_before, ppp_bits_before, _) =
            vp.cached_cell.expect("cache populated after first call");

        let new_ppp = ctx.pixels_per_point() * 2.0;
        ctx.set_pixels_per_point(new_ppp);
        // `set_pixels_per_point` only takes effect at the start of the next pass.
        let _ = ctx.run_ui(egui::RawInput::default(), |_ui| {});
        assert_eq!(
            ctx.pixels_per_point(),
            new_ppp,
            "DPI change should be active after a pass"
        );
        assert_ne!(
            new_ppp.to_bits(),
            ppp_bits_before,
            "sanity: ppp actually changed"
        );

        let _ = vp.cell_size(&ctx);
        let (scale_after, ppp_bits_after, _) =
            vp.cached_cell.expect("cache populated after second call");

        assert_eq!(
            scale_after, scale_before,
            "the scale is unchanged in this scenario"
        );
        assert_eq!(
            ppp_bits_after,
            new_ppp.to_bits(),
            "cache entry must be keyed on the *current* pixels_per_point"
        );
        assert_ne!(
            ppp_bits_after, ppp_bits_before,
            "cache must not silently keep serving the pre-DPI-change key"
        );
    }

    #[test]
    fn fit_to_window_picks_expected_step_and_centers() {
        let ctx = headless_ctx_with_canvas_font();
        let mut vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 80,
            height: 25,
        };

        let cell_at_default = vp.cell_size(&ctx); // real Iosevka Fixed metrics at scale 1.0
                                                  // Shrink the window to 60% of what's needed at the default zoom (index 2, scale 1.0).
                                                  // At scale 0.75 (index 1) that's still 75% of full width/height needed — doesn't fit.
                                                  // At scale 0.5 (index 0) that's 50% needed — fits. So step 0 is the expected winner.
        let available = Vec2::new(
            cell_at_default.x * doc_extent.width as f32 * 0.6,
            cell_at_default.y * doc_extent.height as f32 * 0.6,
        );

        vp.fit_to_window(available, 0.0, doc_extent, &ctx);

        assert_eq!(
            vp.zoom_step, 0,
            "expected the smallest zoom step to fit the shrunk window"
        );

        let cell = vp.cell_size(&ctx);
        let doc_w = doc_extent.width as f32 * cell.x;
        let doc_h = doc_extent.height as f32 * cell.y;
        assert!(
            doc_w <= available.x + 0.5,
            "fitted width should not exceed available width"
        );
        assert!(
            doc_h <= available.y + 0.5,
            "fitted height should not exceed available height"
        );

        let expected_pan = Vec2::new((available.x - doc_w) / 2.0, (available.y - doc_h) / 2.0);
        assert!(
            (vp.pan - expected_pan).length() < 0.01,
            "pan should center the fitted doc: got {:?}, expected {:?}",
            vp.pan,
            expected_pan
        );
    }

    #[test]
    fn zoom_at_keeps_anchor_after_manual_pan() {
        // A nonzero pan before any zoom is the state where accumulating instead of setting pan
        // in `reanchor` would drift. Mixed-sign, and small enough that the cursor stays right of
        // and below the origin (otherwise `screen_to_cell` correctly returns `None`).
        let mut vp = Viewport::default();
        vp.pan += Vec2::new(23.0, -17.0);
        let old_step = vp.zoom_step;

        let cursor = Pos2::new(45.0, 65.0);
        let before = vp
            .screen_to_cell(cursor, cell(), origin(), big_doc())
            .unwrap();

        vp.zoom_at(cursor, 1, cell(), origin());
        let ratio = ZOOM_SCALES[vp.zoom_step] / ZOOM_SCALES[old_step];
        let new_cell = Vec2::new(cell().x * ratio, cell().y * ratio);
        let after = vp
            .screen_to_cell(cursor, new_cell, origin(), big_doc())
            .unwrap();

        assert_eq!(
            after, before,
            "anchor should survive a single zoom after a manual pan"
        );
    }

    #[test]
    fn zoom_at_keeps_anchor_close_across_full_zoom_range() {
        // Walks the anchor-under-cursor invariant through every zoom step, bottom to top and
        // back, emulating the next-frame cell-size re-measurement at each step.
        let mut vp = Viewport {
            zoom_step: 0,
            ..Viewport::default()
        };
        let cursor = Pos2::new(45.0, 65.0);
        let mut current_cell = cell();
        let before = vp
            .screen_to_cell(cursor, current_cell, origin(), big_doc())
            .unwrap();

        for _ in 0..(ZOOM_SCALES.len() - 1) {
            let old_scale = vp.scale();
            vp.zoom_at(cursor, 1, current_cell, origin());
            let ratio = vp.scale() / old_scale;
            current_cell = Vec2::new(current_cell.x * ratio, current_cell.y * ratio);
            let after = vp
                .screen_to_cell(cursor, current_cell, origin(), big_doc())
                .unwrap();
            assert_eq!(
                after, before,
                "anchor drifted zooming IN at zoom_step {}",
                vp.zoom_step
            );
        }
        assert_eq!(vp.zoom_step, ZOOM_SCALES.len() - 1);

        for _ in 0..(ZOOM_SCALES.len() - 1) {
            let old_scale = vp.scale();
            vp.zoom_at(cursor, -1, current_cell, origin());
            let ratio = vp.scale() / old_scale;
            current_cell = Vec2::new(current_cell.x * ratio, current_cell.y * ratio);
            let after = vp
                .screen_to_cell(cursor, current_cell, origin(), big_doc())
                .unwrap();
            assert_eq!(
                after, before,
                "anchor drifted zooming OUT at zoom_step {}",
                vp.zoom_step
            );
        }
        assert_eq!(vp.zoom_step, 0);
    }

    #[test]
    fn fit_to_window_contains_full_doc_when_a_fit_exists() {
        let ctx = headless_ctx_with_canvas_font();
        let mut probe = Viewport {
            zoom_step: 0,
            ..Viewport::default()
        };
        let min_cell = probe.cell_size(&ctx);

        let doc_extents = [
            DocExtent {
                width: 80,
                height: 25,
            },
            DocExtent {
                width: 200,
                height: 100,
            },
            DocExtent {
                width: 1024,
                height: 1024,
            },
            DocExtent {
                width: 1,
                height: 1,
            },
        ];
        for doc_extent in doc_extents {
            // Generous margin above the step-0 lower bound so a fit is unambiguously possible.
            let available = Vec2::new(
                doc_extent.width as f32 * min_cell.x * 1.5 + 10.0,
                doc_extent.height as f32 * min_cell.y * 1.5 + 10.0,
            );
            let mut vp = Viewport::default();
            vp.fit_to_window(available, 0.0, doc_extent, &ctx);
            let cell = vp.cell_size(&ctx);
            let doc_w = doc_extent.width as f32 * cell.x;
            let doc_h = doc_extent.height as f32 * cell.y;
            assert!(
                doc_w <= available.x + 0.5 && doc_h <= available.y + 0.5,
                "fit_to_window must contain the full doc when a fit exists: \
                 doc_extent={doc_extent:?} picked step={} cell={cell:?} \
                 doc_size=({doc_w},{doc_h}) available={available:?}",
                vp.zoom_step
            );
            // Since the doc fits entirely within `available`, the centering pan must not push the
            // doc's top-left corner off-screen (negative pan).
            assert!(
                vp.pan.x >= -0.01 && vp.pan.y >= -0.01,
                "pan should not push a fully-contained doc off-screen: pan={:?}",
                vp.pan
            );
        }
    }

    /// The desk margin's two halves, which pull in opposite directions and are easy to conflate:
    /// it must shrink the room the fit test uses (so the card never butts against the panels), but
    /// must NOT shrink the box the document is centred in (or the card sits off-centre by half the
    /// margin). Applying it to both — the obvious implementation — silently does the second.
    #[test]
    fn fit_margin_insets_the_fit_but_leaves_the_document_centered() {
        let ctx = headless_ctx_with_canvas_font();
        let doc_extent = DocExtent {
            width: 80,
            height: 25,
        };
        let available = Vec2::new(1000.0, 700.0);
        const MARGIN: f32 = 28.0;

        let mut vp = Viewport::default();
        vp.fit_to_window(available, MARGIN, doc_extent, &ctx);
        let cell = vp.cell_size(&ctx);
        let (doc_w, doc_h) = (
            doc_extent.width as f32 * cell.x,
            doc_extent.height as f32 * cell.y,
        );

        // Still centred in the FULL area, not in the inset one.
        assert!(
            (vp.pan.x - (available.x - doc_w) / 2.0).abs() < 0.5,
            "the margin pushed the document off-centre horizontally"
        );
        assert!(
            (vp.pan.y - (available.y - doc_h) / 2.0).abs() < 0.5,
            "the margin pushed the document off-centre vertically"
        );
        // And the margin is real: the desk still shows on every side.
        assert!(
            vp.pan.x >= MARGIN && vp.pan.y >= MARGIN,
            "the card butts against the panel edge"
        );

        // A margin large enough to matter must be able to force a smaller step than no margin does.
        let mut tight = Viewport::default();
        tight.fit_to_window(available, 0.0, doc_extent, &ctx);
        assert!(
            vp.zoom_step <= tight.zoom_step,
            "the margin must never pick a LARGER step"
        );
    }

    /// The reason fit is continuous: a window 1.3× the doc's size at 100% must fit near 130%,
    /// not drop to the largest preset that fits (100%) and waste a third of the screen. This is
    /// the kiosk auto-fit's contract for non-default doc sizes like 120×40.
    #[test]
    fn fit_to_window_fills_the_window_beyond_the_preset_ladder() {
        let ctx = headless_ctx_with_canvas_font();
        let mut vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 120,
            height: 40,
        };

        let cell_at_100 = vp.cell_size(&ctx);
        let available = Vec2::new(
            doc_extent.width as f32 * cell_at_100.x * 1.3,
            doc_extent.height as f32 * cell_at_100.y * 1.3,
        );
        vp.fit_to_window(available, 0.0, doc_extent, &ctx);

        assert!(
            vp.scale() > 1.0 + 0.1,
            "fit stayed on the preset ladder: {}",
            vp.scale()
        );

        let cell = vp.cell_size(&ctx);
        let doc_w = doc_extent.width as f32 * cell.x;
        let doc_h = doc_extent.height as f32 * cell.y;
        assert!(
            doc_w <= available.x + 0.5 && doc_h <= available.y + 0.5,
            "fitted doc overflows: ({doc_w},{doc_h}) in {available:?}"
        );
        let fill = (doc_w / available.x).max(doc_h / available.y);
        assert!(fill >= 0.95, "fit wastes window space: fill={fill:.3}");
    }

    /// A fitted (continuous) scale must hand control back to the preset ladder on the first
    /// manual step: `+` goes to the nearest preset above the fit, `-` to the nearest below.
    #[test]
    fn manual_zoom_from_a_fitted_scale_snaps_to_the_preset_ladder() {
        let ctx = headless_ctx_with_canvas_font();
        let mut vp = Viewport::default();
        let doc_extent = DocExtent {
            width: 80,
            height: 25,
        };

        let cell_at_100 = vp.cell_size(&ctx);
        let available = Vec2::new(
            doc_extent.width as f32 * cell_at_100.x * 1.3,
            doc_extent.height as f32 * cell_at_100.y * 1.3,
        );

        vp.fit_to_window(available, 0.0, doc_extent, &ctx);
        let fitted = vp.scale();
        assert!(
            fitted > 1.0 && fitted < 1.5,
            "test setup: fit should land between presets, got {fitted}"
        );
        assert_eq!(vp.zoom_step, 1, "the ladder position under the fit is 100%");

        let cell = vp.cell_size(&ctx);
        vp.zoom_at(Pos2::new(50.0, 50.0), 1, cell, origin());
        assert_eq!(
            vp.scale(),
            1.5,
            "stepping up from the fit snaps to the next preset above"
        );

        vp.fit_to_window(available, 0.0, doc_extent, &ctx);
        let cell = vp.cell_size(&ctx);
        vp.zoom_at(Pos2::new(50.0, 50.0), -1, cell, origin());
        assert_eq!(
            vp.scale(),
            1.0,
            "stepping down from the fit snaps to the next preset below"
        );
    }

    #[test]
    fn fit_to_window_when_no_step_fits_falls_back_to_smallest_step_without_panicking() {
        // Documents current behavior rather than asserting a chosen design: when the window is
        // smaller than the doc at every zoom step, `fit_to_window` degrades to the smallest step
        // and the doc overflows the window (negative pan) — containment is not guaranteed here.
        let ctx = headless_ctx_with_canvas_font();
        let doc_extent = DocExtent {
            width: 1024,
            height: 1024,
        };
        let available = Vec2::new(10.0, 10.0); // far smaller than the doc even at the smallest step
        let mut vp = Viewport::default();
        vp.fit_to_window(available, 0.0, doc_extent, &ctx);

        assert_eq!(
            vp.zoom_step, 0,
            "falls back to the smallest zoom step when nothing fits"
        );
        let cell = vp.cell_size(&ctx);
        let doc_w = doc_extent.width as f32 * cell.x;
        let doc_h = doc_extent.height as f32 * cell.y;
        assert!(
            doc_w > available.x && doc_h > available.y,
            "sanity check: this scenario is genuinely a non-fit case"
        );
        assert!(
            vp.pan.x < 0.0 && vp.pan.y < 0.0,
            "documents current behavior: pan goes negative (doc overflows the window) when no \
             zoom step fits — containment is NOT guaranteed in this fallback case"
        );
    }

    #[test]
    fn boundary_cell_round_trip_across_zoom_steps_and_pan_offsets() {
        let doc_extent = DocExtent {
            width: 80,
            height: 25,
        };
        let boundary_cells = [(0u16, 0u16), (79, 0), (0, 24), (79, 24)];
        let pans = [
            Vec2::ZERO,
            Vec2::new(37.5, -12.25),
            Vec2::new(-500.0, 500.0),
        ];

        for zoom_step in 0..ZOOM_SCALES.len() {
            for &pan in &pans {
                let vp = Viewport {
                    zoom_step,
                    pan,
                    ..Viewport::default()
                };
                let cell = Vec2::new(10.0 * vp.scale(), 20.0 * vp.scale());
                for &(x, y) in &boundary_cells {
                    let p = vp.cell_to_screen(x, y, cell, origin());
                    // Nudge into the cell's interior so floating-point edge rounding can't
                    // spuriously land on a neighbouring cell.
                    let nudged = p + Vec2::new(cell.x * 0.4, cell.y * 0.4);
                    let back = vp.screen_to_cell(nudged, cell, origin(), doc_extent);
                    assert_eq!(
                        back,
                        Some((x, y)),
                        "round trip failed at zoom_step={zoom_step} pan={pan:?} cell=({x},{y})"
                    );
                }
            }
        }
    }
}
