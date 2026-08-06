use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, FontId, Galley, Painter, Pos2, Rect, Shape};

/// Collects one layer's cell painting into two shape runs — backgrounds, then glyphs — each
/// submitted with a single `Painter::extend`. `Painter::text` per cell costs a `String`
/// allocation, a layout-job hash, and two `Context` write-locks per call; at canvas scale
/// (thousands of cells per frame) that per-call overhead dominates paint time on low-power
/// hardware. Batching pays the text layout once per distinct `(glyph, color)` pair and the
/// painter lock once per run instead.
///
/// Cells within one layer occupy disjoint rects, so splitting a layer into a background pass and
/// a glyph pass preserves what per-cell interleaved painting produced: a cell's own glyph still
/// lands above its own background, and no other cell's shapes overlap either. Layer stacking
/// stays exact as long as the caller flushes between layers.
pub struct CellBatch {
    font: FontId,
    galleys: HashMap<(char, Color32), Arc<Galley>>,
    bgs: Vec<Shape>,
    glyphs: Vec<Shape>,
}

impl CellBatch {
    pub fn new(font: FontId) -> Self {
        Self {
            font,
            galleys: HashMap::new(),
            bgs: Vec::new(),
            glyphs: Vec::new(),
        }
    }

    /// Queues a cell's background fill.
    pub fn bg(&mut self, rect: Rect, color: Color32) {
        self.bgs.push(Shape::rect_filled(rect, 0.0, color));
    }

    /// Queues a cell's glyph, anchored top-left at `pos` (matching
    /// `Painter::text(..., Align2::LEFT_TOP, ...)`). The galley is laid out once per distinct
    /// `(ch, color)` pair per batch lifetime and shared by `Arc` afterward.
    pub fn glyph(&mut self, painter: &Painter, pos: Pos2, ch: char, color: Color32) {
        let galley = self
            .galleys
            .entry((ch, color))
            .or_insert_with(|| painter.layout_no_wrap(ch.to_string(), self.font.clone(), color))
            .clone();
        self.glyphs.push(Shape::galley(pos, galley, color));
    }

    /// Submits everything queued since the last flush — backgrounds first, then glyphs — and
    /// resets for the next layer. The galley memo survives flushes: glyphs repeat across layers,
    /// and the memo is keyed on nothing layer-specific.
    pub fn flush_layer(&mut self, painter: &Painter) {
        if !self.bgs.is_empty() {
            painter.extend(self.bgs.drain(..));
        }
        if !self.glyphs.is_empty() {
            painter.extend(self.glyphs.drain(..));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Align2, Vec2};

    fn run_shapes(paint: impl Fn(&Painter)) -> Vec<Shape> {
        let ctx = egui::Context::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            paint(&painter);
        });
        out.shapes.into_iter().map(|cs| cs.shape).collect()
    }

    fn font() -> FontId {
        FontId::monospace(16.0)
    }

    /// Within one flush, every background must precede every glyph — the invariant that lets a
    /// batched layer replace per-cell interleaved painting without a glyph ever being covered by a
    /// same-layer background.
    #[test]
    fn flush_emits_all_backgrounds_before_any_glyph() {
        let shapes = run_shapes(|painter| {
            let mut batch = CellBatch::new(font());
            batch.glyph(painter, Pos2::ZERO, 'a', Color32::WHITE);
            batch.bg(
                Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)),
                Color32::RED,
            );
            batch.glyph(painter, Pos2::new(10.0, 0.0), 'b', Color32::WHITE);
            batch.bg(
                Rect::from_min_size(Pos2::new(10.0, 0.0), Vec2::splat(10.0)),
                Color32::BLUE,
            );
            batch.flush_layer(painter);
        });
        let first_glyph = shapes
            .iter()
            .position(|s| matches!(s, Shape::Text(_)))
            .expect("glyphs were queued");
        let last_bg = shapes
            .iter()
            .rposition(|s| matches!(s, Shape::Rect(_)))
            .expect("backgrounds were queued");
        assert!(
            last_bg < first_glyph,
            "backgrounds must all precede glyphs within a flush: last bg at {last_bg}, first glyph at {first_glyph}"
        );
    }

    /// Two flushes must keep their submission order — layer N's glyphs before layer N+1's
    /// backgrounds — or acetate stacking breaks.
    #[test]
    fn consecutive_flushes_preserve_layer_stacking_order() {
        let shapes = run_shapes(|painter| {
            let mut batch = CellBatch::new(font());
            batch.glyph(painter, Pos2::ZERO, 'x', Color32::WHITE);
            batch.flush_layer(painter);
            batch.bg(
                Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)),
                Color32::RED,
            );
            batch.flush_layer(painter);
        });
        let glyph_at = shapes
            .iter()
            .position(|s| matches!(s, Shape::Text(_)))
            .expect("layer-0 glyph present");
        let bg_at = shapes
            .iter()
            .position(|s| matches!(s, Shape::Rect(_)))
            .expect("layer-1 background present");
        assert!(
            glyph_at < bg_at,
            "an earlier flush's glyphs must precede a later flush's backgrounds"
        );
    }

    /// A batched glyph must land exactly where `Painter::text(..., Align2::LEFT_TOP, ...)` puts
    /// it — same position, same glyph, same color — so swapping a renderer over to the batch is
    /// pixel-identical.
    #[test]
    fn batched_glyph_matches_painter_text_left_top() {
        let pos = Pos2::new(7.0, 3.0);
        let via_batch = run_shapes(|painter| {
            let mut batch = CellBatch::new(font());
            batch.glyph(painter, pos, 'Q', Color32::LIGHT_BLUE);
            batch.flush_layer(painter);
        });
        let via_text = run_shapes(|painter| {
            painter.text(pos, Align2::LEFT_TOP, 'Q', font(), Color32::LIGHT_BLUE);
        });
        let batch_text = via_batch
            .iter()
            .find_map(|s| match s {
                Shape::Text(t) => Some(t),
                _ => None,
            })
            .expect("batch produced a text shape");
        let direct_text = via_text
            .iter()
            .find_map(|s| match s {
                Shape::Text(t) => Some(t),
                _ => None,
            })
            .expect("painter.text produced a text shape");
        assert_eq!(batch_text.pos, direct_text.pos);
        assert_eq!(batch_text.galley.job.text, direct_text.galley.job.text);
    }

    /// The galley memo must actually dedupe: N cells of the same `(char, color)` lay out once.
    /// Checked structurally via `Arc` identity across the emitted shapes.
    #[test]
    fn repeated_glyphs_share_one_galley() {
        let shapes = run_shapes(|painter| {
            let mut batch = CellBatch::new(font());
            for i in 0..3 {
                batch.glyph(
                    painter,
                    Pos2::new(i as f32 * 10.0, 0.0),
                    '#',
                    Color32::WHITE,
                );
            }
            batch.flush_layer(painter);
        });
        let galleys: Vec<&Arc<Galley>> = shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Text(t) => Some(&t.galley),
                _ => None,
            })
            .collect();
        assert_eq!(galleys.len(), 3);
        assert!(
            Arc::ptr_eq(galleys[0], galleys[1]) && Arc::ptr_eq(galleys[1], galleys[2]),
            "identical (char, color) glyphs must share one laid-out galley"
        );
    }
}
