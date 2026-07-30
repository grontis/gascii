//! Fixed-pixel-size color-block thumbnails, built only for a frame index the strip actually asks
//! for — `timeline::body`'s own visible-rect culling decides which indices that is, this cache has
//! no viewport awareness of its own. Two independent gates keep a repaint cheap:
//!
//! - **Edit-id gate**: each entry remembers the host's `top_edit_id` (`History::top_edit_id`, an
//!   identity that changes on every apply/undo/redo — see `gascii_plugin_api::PluginHost::
//!   top_edit_id`) it was last verified against. A repaint where nothing has changed anywhere in
//!   the document reuses every visible entry's texture with **no** `composite_frame` call and
//!   **no** re-hash — the common case (idle playback, hovering, an unrelated window redraw).
//! - **Content-hash gate**: once the edit id *has* changed, a touched frame is recomposited and
//!   re-hashed (there's no cheaper way to know whether *this* frame specifically changed), but the
//!   `egui::TextureHandle` — the expensive GPU upload — is only rebuilt if the hash actually
//!   differs from what's cached, so an edit to frame 3 doesn't re-upload frames 0-2 and 4-N just
//!   because they were also visible and also got re-verified.
//!
//! 256 frames x up to 1024^2 cells rules out rendering actual glyphs into every thumbnail —
//! illegible at strip scale and expensive regardless — so this samples/averages `composite_frame`'s
//! cells into a small fixed grid instead, uploaded the same `egui::TextureHandle` way
//! `gascii/src/image_bg.rs` already uses for the trace image.

use gascii_core::{Cell, Document};

pub(crate) const THUMB_W: usize = 48;
pub(crate) const THUMB_H: usize = 30;

struct CachedThumb {
    content_hash: u64,
    texture: egui::TextureHandle,
    /// The host's `top_edit_id` this entry was last verified (composited + hashed) against —
    /// `None` covers both "a fresh document with nothing applied yet" and, ambiguously but
    /// harmlessly, "never verified": either way the fast path only fires once composited/hashed at
    /// least once for the id currently in play.
    built_at_edit_id: Option<u64>,
}

/// Resized lazily to `doc.frame_count()` on access — never eagerly for every frame a document
/// might ever have: only a frame the strip's own culling actually asks for ever calls
/// `get_or_build`, so this never holds more live textures than have actually been shown.
/// `get_or_build` also truncates `entries` down to `doc.frame_count()` on every call — a frame
/// deleted out from under this cache frees its texture rather than lingering at a now-meaningless
/// index forever.
#[derive(Default)]
pub(crate) struct ThumbnailCache {
    entries: Vec<Option<CachedThumb>>,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// The number of frame indices this cache has actually built a texture for — exists as a
    /// narrow test seam for `timeline.rs`'s visible-rect culling (proving offscreen frames never
    /// reach `get_or_build` at all), not a production reporting API.
    #[cfg(test)]
    pub(crate) fn built_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }

    /// Returns the (possibly freshly regenerated) texture for `frame`, fixed `THUMB_W x THUMB_H` px
    /// regardless of the document's actual cell dimensions. `None` only for an out-of-bounds
    /// `frame`. Only ever called for a frame the strip's own visible-rect culling decided to show —
    /// this cache has no viewport awareness of its own. `top_edit_id` is `PluginHost::
    /// top_edit_id()`'s value for the current repaint — see the module doc for the two-gate
    /// strategy this drives.
    pub fn get_or_build(&mut self, ctx: &egui::Context, doc: &Document, frame: usize, top_edit_id: Option<u64>) -> Option<egui::TextureHandle> {
        if frame >= doc.frame_count() {
            return None;
        }
        if self.entries.len() > doc.frame_count() {
            self.entries.truncate(doc.frame_count());
        }
        if self.entries.len() <= frame {
            self.entries.resize_with(frame + 1, || None);
        }
        if let Some(cached) = &self.entries[frame] {
            if cached.built_at_edit_id == top_edit_id {
                // Nothing has changed anywhere in the document since this entry was last
                // verified — reuse outright, no composite, no hash.
                return Some(cached.texture.clone());
            }
        }
        let composited = gascii_core::composite_frame(doc, frame)?;
        let hash = content_hash(&composited);
        if let Some(cached) = &mut self.entries[frame] {
            if cached.content_hash == hash {
                // This frame's own content is unchanged even though the edit id moved (the edit
                // touched a different frame, or a document-level property) — skip the texture
                // re-upload, but record the new id so the fast path above fires again next time.
                cached.built_at_edit_id = top_edit_id;
                return Some(cached.texture.clone());
            }
        }
        let pixels = build_pixels(doc, &composited);
        let image = egui::ColorImage::from_rgba_unmultiplied([THUMB_W, THUMB_H], &pixels);
        let texture = ctx.load_texture(format!("gascii_anim_thumb_{frame}"), image, egui::TextureOptions::LINEAR);
        let handle = texture.clone();
        self.entries[frame] = Some(CachedThumb { content_hash: hash, texture, built_at_edit_id: top_edit_id });
        Some(handle)
    }
}

/// A cheap, order-sensitive content hash over every cell's glyph/fg/bg — good enough to detect "did
/// this frame's content change since the last thumbnail build," not a cryptographic property.
/// Deliberately excludes `doc.background`: it's set once at document creation and never edited
/// afterward (see `Document::background`'s own doc comment), so within one document's lifetime it
/// can never be the reason a thumbnail goes stale.
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
        let first = cache.get_or_build(&ctx, &doc, 0, Some(1)).unwrap();
        let second = cache.get_or_build(&ctx, &doc, 0, Some(1)).unwrap();
        assert_eq!(first.id(), second.id(), "unchanged content must reuse the same texture");
    }

    #[test]
    fn get_or_build_regenerates_after_a_cell_edit_changes_the_frames_content() {
        let ctx = egui::Context::default();
        let mut doc = doc_with_cell(4, 4, 'x', Rgba(200, 0, 0, 255));
        let mut cache = ThumbnailCache::new();
        let first = cache.get_or_build(&ctx, &doc, 0, Some(1)).unwrap();
        doc.set_cell(0, 1, 1, Cell { ch: 'y', fg: Rgba::WHITE, bg: Rgba(0, 200, 0, 255) });
        // A real edit always advances `top_edit_id` too — this pins the ordinary "something in the
        // document actually changed" path, distinct from the edit-id-gate-specific tests below.
        let second = cache.get_or_build(&ctx, &doc, 0, Some(2)).unwrap();
        assert_ne!(first.id(), second.id(), "changed content must regenerate the texture");
    }

    /// The edit-id fast path, proven behaviorally rather than via an instrumentation hook: with
    /// `top_edit_id` held constant across two calls, a content change made directly to `doc`
    /// (bypassing `History` — never how a real edit reaches the document, but the only way to make
    /// "content changed, id didn't" observable from outside) must NOT be picked up. If the second
    /// call had recomposited/rehashed at all, it would have noticed the mismatch and rebuilt; it
    /// must not even look.
    #[test]
    fn get_or_build_does_not_notice_a_content_change_when_top_edit_id_is_unchanged() {
        let ctx = egui::Context::default();
        let mut doc = doc_with_cell(4, 4, 'x', Rgba(200, 0, 0, 255));
        let mut cache = ThumbnailCache::new();
        let first = cache.get_or_build(&ctx, &doc, 0, Some(1)).unwrap();
        doc.set_cell(0, 1, 1, Cell { ch: 'y', fg: Rgba::WHITE, bg: Rgba(0, 200, 0, 255) });
        let second = cache.get_or_build(&ctx, &doc, 0, Some(1)).unwrap();
        assert_eq!(first.id(), second.id(), "an unchanged top_edit_id must skip re-composite/re-hash entirely, even if doc content moved underneath it");
    }

    /// The content-hash gate once the edit id *has* moved: an edit elsewhere in the document (the
    /// id changes) but this frame's own content is untouched must reuse the existing texture — no
    /// GPU re-upload — while still updating its recorded edit id so the fast path fires again next.
    #[test]
    fn get_or_build_reuses_the_texture_when_the_edit_id_changes_but_this_frames_content_does_not() {
        let ctx = egui::Context::default();
        let doc = doc_with_cell(4, 4, 'x', Rgba(200, 0, 0, 255));
        let mut cache = ThumbnailCache::new();
        let first = cache.get_or_build(&ctx, &doc, 0, Some(1)).unwrap();
        let second = cache.get_or_build(&ctx, &doc, 0, Some(2)).unwrap();
        assert_eq!(first.id(), second.id(), "an edit id change with identical content must not re-upload the texture");

        // The fast path must now be armed against the new id, not the original one.
        let third = cache.get_or_build(&ctx, &doc, 0, Some(2)).unwrap();
        assert_eq!(second.id(), third.id());
    }

    #[test]
    fn thumbnail_is_a_fixed_pixel_size_regardless_of_document_dimensions() {
        let ctx = egui::Context::default();
        let small = doc_with_cell(80, 25, 'a', Rgba(1, 2, 3, 255));
        let large = doc_with_cell(1024, 1024, 'a', Rgba(1, 2, 3, 255));
        let mut cache = ThumbnailCache::new();
        let small_tex = cache.get_or_build(&ctx, &small, 0, Some(1)).unwrap();
        let large_tex = cache.get_or_build(&ctx, &large, 0, Some(1)).unwrap();
        assert_eq!(small_tex.size(), [THUMB_W, THUMB_H]);
        assert_eq!(large_tex.size(), [THUMB_W, THUMB_H]);
    }

    #[test]
    fn get_or_build_returns_none_for_an_out_of_bounds_frame() {
        let ctx = egui::Context::default();
        let doc = Document::default_document();
        let mut cache = ThumbnailCache::new();
        assert!(cache.get_or_build(&ctx, &doc, 5, Some(1)).is_none());
    }

    fn doc_with_frames(n: usize) -> Document {
        let mut doc = doc_with_cell(4, 4, 'x', Rgba(200, 0, 0, 255));
        let mut history = gascii_core::History::new();
        for i in 1..n {
            let edit = gascii_core::add_frame(&doc, i, gascii_core::Frame::blank(4, 4)).unwrap();
            history.apply(&mut doc, edit);
        }
        doc
    }

    /// L2: a frame removed out from under the cache must have its entry (and texture) freed, not
    /// linger forever at an index the document no longer has.
    #[test]
    fn get_or_build_evicts_entries_past_the_documents_current_frame_count() {
        let ctx = egui::Context::default();
        let doc = doc_with_frames(3);
        let mut cache = ThumbnailCache::new();
        // Touch indices 0..3 so the backing Vec grows to hold them.
        for i in 0..3 {
            let _ = cache.get_or_build(&ctx, &doc, i, Some(1));
        }
        assert_eq!(cache.entries.len(), 3);

        // Simulate the document shrinking to 1 frame (a delete): the next call, for the one
        // remaining index, must truncate the stale entries above it.
        let shrunk = doc_with_frames(1);
        let _ = cache.get_or_build(&ctx, &shrunk, 0, Some(2));
        assert_eq!(cache.entries.len(), 1, "entries past the document's current frame count must be evicted");
    }
}
