//! Fixed-pixel-size color-block thumbnails, lazily built per visible frame index only, dirty-hashed
//! so an unchanged frame's texture is reused across frames. 256 frames x up to 1024^2 cells rules
//! out rendering actual glyphs into every thumbnail — illegible at strip scale and expensive
//! regardless — so this samples/averages `composite_frame`'s cells into a small fixed grid instead,
//! uploaded the same `egui::TextureHandle` way `gascii/src/image_bg.rs` already uses for the trace
//! image.

use gascii_core::{Cell, Document};

pub(crate) const THUMB_W: usize = 48;
pub(crate) const THUMB_H: usize = 30;

struct CachedThumb {
    content_hash: u64,
    texture: egui::TextureHandle,
}

/// Resized lazily to `doc.frame_count()` on access — never eagerly for every frame a document
/// might ever have: only a frame actually scrolled into view ever calls `get_or_build`, so this
/// never holds more live textures than the strip has shown.
#[derive(Default)]
pub(crate) struct ThumbnailCache {
    entries: Vec<Option<CachedThumb>>,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Returns the (possibly freshly regenerated) texture for `frame`, fixed `THUMB_W x THUMB_H` px
    /// regardless of the document's actual cell dimensions. `None` only for an out-of-bounds
    /// `frame`. Only ever called for frames currently scrolled into view — the strip's own layout
    /// decides visibility, not this cache.
    pub fn get_or_build(&mut self, ctx: &egui::Context, doc: &Document, frame: usize) -> Option<egui::TextureHandle> {
        let composited = gascii_core::composite_frame(doc, frame)?;
        if self.entries.len() <= frame {
            self.entries.resize_with(frame + 1, || None);
        }
        let hash = content_hash(&composited);
        if let Some(cached) = &self.entries[frame] {
            if cached.content_hash == hash {
                return Some(cached.texture.clone());
            }
        }
        let pixels = build_pixels(doc, &composited);
        let image = egui::ColorImage::from_rgba_unmultiplied([THUMB_W, THUMB_H], &pixels);
        let texture = ctx.load_texture(format!("gascii_anim_thumb_{frame}"), image, egui::TextureOptions::LINEAR);
        let handle = texture.clone();
        self.entries[frame] = Some(CachedThumb { content_hash: hash, texture });
        Some(handle)
    }
}

/// A cheap, order-sensitive content hash over every cell's glyph/fg/bg — good enough to detect "did
/// this frame's content change since the last thumbnail build," not a cryptographic property.
fn content_hash(rows: &[Vec<Cell>]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for row in rows {
        for c in row {
            c.ch.hash(&mut h);
            (c.fg.0, c.fg.1, c.fg.2, c.fg.3).hash(&mut h);
            (c.bg.0, c.bg.1, c.bg.2, c.bg.3).hash(&mut h);
        }
    }
    h.finish()
}

/// `cell.bg` alpha-composited over `doc.background` — the thumbnail ignores glyphs entirely (a
/// fixed-size color block, not a rendered sheet) so only the background channel matters.
fn effective_rgb(doc: &Document, cell: &Cell) -> [f32; 3] {
    let a = cell.bg.3 as f32 / 255.0;
    let bg = doc.background;
    [
        cell.bg.0 as f32 * a + bg.0 as f32 * (1.0 - a),
        cell.bg.1 as f32 * a + bg.1 as f32 * (1.0 - a),
        cell.bg.2 as f32 * a + bg.2 as f32 * (1.0 - a),
    ]
}

/// Block-averages `composited` (the document's real cell dimensions) down into a fixed
/// `THUMB_W x THUMB_H` RGBA buffer.
fn build_pixels(doc: &Document, composited: &[Vec<Cell>]) -> Vec<u8> {
    let (src_w, src_h) = (doc.width as usize, doc.height as usize);
    let mut out = vec![0u8; THUMB_W * THUMB_H * 4];
    for ty in 0..THUMB_H {
        let y0 = ty * src_h / THUMB_H;
        let y1 = ((ty + 1) * src_h / THUMB_H).max(y0 + 1).min(src_h);
        for tx in 0..THUMB_W {
            let x0 = tx * src_w / THUMB_W;
            let x1 = ((tx + 1) * src_w / THUMB_W).max(x0 + 1).min(src_w);
            let mut sum = [0.0f32; 3];
            let mut count = 0.0f32;
            for row in composited.iter().take(y1).skip(y0) {
                for cell in row.iter().take(x1).skip(x0) {
                    let rgb = effective_rgb(doc, cell);
                    sum[0] += rgb[0];
                    sum[1] += rgb[1];
                    sum[2] += rgb[2];
                    count += 1.0;
                }
            }
            let idx = (ty * THUMB_W + tx) * 4;
            if count > 0.0 {
                out[idx] = (sum[0] / count).round() as u8;
                out[idx + 1] = (sum[1] / count).round() as u8;
                out[idx + 2] = (sum[2] / count).round() as u8;
            } else {
                out[idx] = doc.background.0;
                out[idx + 1] = doc.background.1;
                out[idx + 2] = doc.background.2;
            }
            out[idx + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::Rgba;

    fn doc_with_cell(w: u16, h: u16, ch: char, bg: Rgba) -> Document {
        let mut doc = Document::new(w, h);
        doc.set_cell(0, 0, 0, Cell { ch, fg: Rgba::WHITE, bg });
        doc
    }

    #[test]
    fn get_or_build_returns_the_same_texture_id_when_content_is_unchanged() {
        let ctx = egui::Context::default();
        let doc = doc_with_cell(4, 4, 'x', Rgba(200, 0, 0, 255));
        let mut cache = ThumbnailCache::new();
        let first = cache.get_or_build(&ctx, &doc, 0).unwrap();
        let second = cache.get_or_build(&ctx, &doc, 0).unwrap();
        assert_eq!(first.id(), second.id(), "unchanged content must reuse the same texture");
    }

    #[test]
    fn get_or_build_regenerates_after_a_cell_edit_changes_the_frames_content() {
        let ctx = egui::Context::default();
        let mut doc = doc_with_cell(4, 4, 'x', Rgba(200, 0, 0, 255));
        let mut cache = ThumbnailCache::new();
        let first = cache.get_or_build(&ctx, &doc, 0).unwrap();
        doc.set_cell(0, 1, 1, Cell { ch: 'y', fg: Rgba::WHITE, bg: Rgba(0, 200, 0, 255) });
        let second = cache.get_or_build(&ctx, &doc, 0).unwrap();
        assert_ne!(first.id(), second.id(), "changed content must regenerate the texture");
    }

    #[test]
    fn thumbnail_is_a_fixed_pixel_size_regardless_of_document_dimensions() {
        let ctx = egui::Context::default();
        let small = doc_with_cell(80, 25, 'a', Rgba(1, 2, 3, 255));
        let large = doc_with_cell(1024, 1024, 'a', Rgba(1, 2, 3, 255));
        let mut cache = ThumbnailCache::new();
        let small_tex = cache.get_or_build(&ctx, &small, 0).unwrap();
        let large_tex = cache.get_or_build(&ctx, &large, 0).unwrap();
        assert_eq!(small_tex.size(), [THUMB_W, THUMB_H]);
        assert_eq!(large_tex.size(), [THUMB_W, THUMB_H]);
    }

    #[test]
    fn get_or_build_returns_none_for_an_out_of_bounds_frame() {
        let ctx = egui::Context::default();
        let doc = Document::default_document();
        let mut cache = ThumbnailCache::new();
        assert!(cache.get_or_build(&ctx, &doc, 5).is_none());
    }
}
