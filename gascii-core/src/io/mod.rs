//! Layer-general compositing and file I/O. Two consumption models share the layer stack, and both
//! keep hidden-layer exclusion inside this module:
//!
//! - **Stacked ("acetate") painting** — `visible_layers()` yields the paint order, and each
//!   consumer (the canvas renderer, the animation playback overlay, PNG export) paints every
//!   layer's own backgrounds and glyphs bottom-to-top. Glyph ink from different layers can overlap
//!   visibly in one cell; only an opaque upper background occludes what's beneath. This is the
//!   canonical on-screen/raster model.
//! - **Flattening** — `composite_cell()` turns one coordinate's layer stack into one `Cell` (top
//!   glyph wins the cell), with `composite_frame()`/`composite()` as whole-sheet and active-frame
//!   wrappers. The only option for single-glyph-per-cell consumers: text export and thumbnails —
//!   a `.txt` cell can't hold two overlapping characters.

pub mod export_png;
pub mod export_text;
pub mod gascii_json;

use crate::model::{Cell, Document};

/// Flattens `doc`'s active frame into one sheet, bottom-to-top alpha-over compositing. A thin
/// wrapper around `composite_frame` for the common case.
pub fn composite(doc: &Document) -> Vec<Vec<Cell>> {
    composite_frame(doc, doc.active_frame()).expect("active_frame is always a valid index")
}

/// Flattens frame `frame`'s layers into one sheet, bottom-to-top alpha-over compositing. `None`
/// for an out-of-bounds `frame`. The general entry point cross-frame consumers (onion-skinning,
/// per-frame export) need; `composite` is the active-frame convenience wrapper most callers want.
pub fn composite_frame(doc: &Document, frame: usize) -> Option<Vec<Vec<Cell>>> {
    doc.frame_layers(frame)?;
    let (w, h) = (doc.width as usize, doc.height as usize);
    let mut out = vec![vec![Cell::BLANK; w]; h];
    for y in 0..doc.height {
        for x in 0..doc.width {
            out[y as usize][x as usize] = composite_cell(doc, frame, x, y);
        }
    }
    Some(out)
}

/// Flattens a single cell at `(x, y)` across every visible layer of `frame`, bottom-to-top
/// alpha-over compositing — `Cell::BLANK` for an out-of-bounds `frame`/coordinate. One glyph per
/// cell, top wins: the flattening model for consumers that can only hold a single character per
/// cell (text export, thumbnails). Painting consumers (canvas, playback overlay, PNG export) walk
/// `visible_layers` and stack instead — see the module doc.
pub fn composite_cell(doc: &Document, frame: usize, x: u16, y: u16) -> Cell {
    let Some(layers) = doc.frame_layers(frame) else {
        return Cell::BLANK;
    };
    let mut out = Cell::BLANK;
    for layer in 0..layers.len() {
        if !doc.layer_visible(layer) {
            continue;
        }
        let Some(&over) = doc.cell_at(frame, layer, x, y) else {
            continue;
        };
        out = alpha_over(out, over);
    }
    out
}

/// Indices of `frame`'s visible layers, bottom to top — the paint order for stacked ("acetate")
/// rendering. Empty for an out-of-bounds `frame`. Every painter that stacks layers walks this
/// instead of `0..layer_count` so hidden-layer exclusion stays here, next to `composite_cell`'s
/// own — the two models can never disagree about which layers exist.
pub fn visible_layers(doc: &Document, frame: usize) -> Vec<usize> {
    let Some(layers) = doc.frame_layers(frame) else {
        return Vec::new();
    };
    (0..layers.len())
        .filter(|&layer| doc.layer_visible(layer))
        .collect()
}

fn alpha_over(under: Cell, over: Cell) -> Cell {
    if over.is_blank() {
        return under; // fully transparent — nothing to composite (Blank is alpha)
    }
    if over.bg.is_transparent() {
        return Cell {
            ch: over.ch,
            fg: over.fg,
            bg: under.bg,
        }; // glyph/fg opaque, bg shows through
    }
    if over.bg.3 == 255 {
        return over; // fully opaque bg: complete replace
    }
    blended_over(under, over) // partial bg alpha: standard "over" blend on the bg channel
}

/// Standard `out = over*a + under*(1-a)` per-channel blend on `bg`. A cell can only ever show one
/// glyph, so `ch`/`fg` still fully replace regardless of `bg`'s alpha.
fn blended_over(under: Cell, over: Cell) -> Cell {
    let a = over.bg.3 as f32 / 255.0;
    let blend = |o: u8, u: u8| -> u8 { (o as f32 * a + u as f32 * (1.0 - a)).round() as u8 };
    let bg = crate::model::Rgba(
        blend(over.bg.0, under.bg.0),
        blend(over.bg.1, under.bg.1),
        blend(over.bg.2, under.bg.2),
        blend(over.bg.3, under.bg.3),
    );
    Cell {
        ch: over.ch,
        fg: over.fg,
        bg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgba;

    fn cell(ch: char, fg: Rgba, bg: Rgba) -> Cell {
        Cell { ch, fg, bg }
    }

    #[test]
    fn visible_layers_yields_bottom_to_top_and_skips_hidden_layers() {
        let mut doc = Document::new(2, 2);
        let mut history = crate::edit::History::new();
        for _ in 0..2 {
            let edit = crate::layer_ops::add_layer(&doc, doc.layer_count()).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(visible_layers(&doc, 0), vec![0, 1, 2]);

        let hide = crate::layer_ops::set_layer_visibility(&doc, 1, false)
            .unwrap()
            .unwrap();
        history.apply(&mut doc, hide);
        assert_eq!(
            visible_layers(&doc, 0),
            vec![0, 2],
            "a hidden layer is excluded, order stays bottom-to-top"
        );
    }

    #[test]
    fn visible_layers_of_an_out_of_bounds_frame_is_empty() {
        let doc = Document::new(2, 2);
        assert!(visible_layers(&doc, 5).is_empty());
    }

    /// The two consumption models must agree on which layers exist: a layer `composite_cell`
    /// excludes must also be absent from `visible_layers`, and vice versa.
    #[test]
    fn visible_layers_and_composite_cell_agree_on_hidden_layer_exclusion() {
        let mut doc = Document::new(1, 1);
        let mut history = crate::edit::History::new();
        let edit = crate::layer_ops::add_layer(&doc, 1).unwrap();
        history.apply(&mut doc, edit);
        doc.set_cell(1, 0, 0, cell('t', Rgba::WHITE, Rgba::TRANSPARENT));
        let hide = crate::layer_ops::set_layer_visibility(&doc, 1, false)
            .unwrap()
            .unwrap();
        history.apply(&mut doc, hide);

        assert_eq!(visible_layers(&doc, 0), vec![0]);
        assert_eq!(
            composite_cell(&doc, 0, 0, 0),
            Cell::BLANK,
            "the flattened view excludes the same hidden layer"
        );
    }

    #[test]
    fn single_layer_composite_is_identity() {
        let mut doc = Document::new(3, 2);
        doc.set_cell(0, 1, 1, cell('x', Rgba::WHITE, Rgba(1, 2, 3, 255)));
        let out = composite(&doc);
        assert_eq!(out[1][1], *doc.cell(0, 1, 1).unwrap());
        assert_eq!(out[0][0], Cell::BLANK);
    }

    #[test]
    fn fully_blank_top_layer_leaves_bottom_layer_unchanged() {
        let mut doc = Document::new(2, 2);
        doc.set_cell(0, 0, 0, cell('b', Rgba::WHITE, Rgba(5, 5, 5, 255)));
        doc.layers_mut().push(crate::model::Layer::blank(2, 2)); // top layer, entirely Blank
        let out = composite(&doc);
        assert_eq!(out[0][0], *doc.cell(0, 0, 0).unwrap());
    }

    #[test]
    fn fully_opaque_top_layer_completely_replaces_bottom() {
        let mut doc = Document::new(2, 2);
        doc.set_cell(0, 0, 0, cell('b', Rgba::WHITE, Rgba(5, 5, 5, 255)));
        doc.layers_mut().push(crate::model::Layer::blank(2, 2));
        let top = cell('t', Rgba(9, 9, 9, 255), Rgba(200, 0, 0, 255));
        doc.set_cell(1, 0, 0, top);
        let out = composite(&doc);
        assert_eq!(out[0][0], top);
    }

    #[test]
    fn partial_alpha_bg_blends_toward_top_without_full_replace() {
        let mut doc = Document::new(2, 2);
        let bottom_bg = Rgba(0, 0, 0, 255);
        doc.set_cell(0, 0, 0, cell('b', Rgba::WHITE, bottom_bg));
        doc.layers_mut().push(crate::model::Layer::blank(2, 2));
        let top_bg = Rgba(255, 255, 255, 128);
        doc.set_cell(1, 0, 0, cell('t', Rgba::WHITE, top_bg));
        let out = composite(&doc);
        let blended = out[0][0].bg;
        assert!(
            blended.0 > bottom_bg.0 && blended.0 < top_bg.0,
            "red channel should sit strictly between"
        );
        assert!(
            blended.1 > bottom_bg.1 && blended.1 < top_bg.1,
            "green channel should sit strictly between"
        );
        assert!(
            blended.2 > bottom_bg.2 && blended.2 < top_bg.2,
            "blue channel should sit strictly between"
        );
    }

    #[test]
    fn transparent_bg_over_opaque_bg_shows_bottom_bg_through() {
        let mut doc = Document::new(1, 1);
        doc.set_cell(0, 0, 0, cell(' ', Rgba::WHITE, Rgba(7, 8, 9, 255)));
        doc.layers_mut().push(crate::model::Layer::blank(1, 1));
        doc.set_cell(1, 0, 0, cell('t', Rgba(1, 1, 1, 255), Rgba::TRANSPARENT));
        let out = composite(&doc);
        assert_eq!(out[0][0].ch, 't');
        assert_eq!(out[0][0].fg, Rgba(1, 1, 1, 255));
        assert_eq!(out[0][0].bg, Rgba(7, 8, 9, 255));
    }

    #[test]
    fn composite_frame_flattens_an_explicit_non_active_frame() {
        let mut doc = Document::new(2, 2);
        let mut history = crate::edit::History::new();
        let edit = crate::frame_ops::add_frame(&doc, 1, crate::model::Frame::blank(2, 2)).unwrap();
        history.apply(&mut doc, edit);
        assert!(doc.set_active_frame(1));
        doc.set_cell(0, 0, 0, cell('Q', Rgba::WHITE, Rgba::TRANSPARENT));
        assert!(doc.set_active_frame(0));

        let out = composite_frame(&doc, 1).unwrap();
        assert_eq!(
            out[0][0].ch, 'Q',
            "explicit frame 1 must be flattened, independent of the active frame (0)"
        );
    }

    #[test]
    fn composite_frame_returns_none_for_an_out_of_bounds_frame_index() {
        let doc = Document::new(2, 2);
        assert!(composite_frame(&doc, 1).is_none());
    }

    #[test]
    fn composite_matches_composite_frame_of_the_active_frame_index() {
        let mut doc = Document::new(2, 2);
        doc.set_cell(0, 0, 0, cell('a', Rgba::WHITE, Rgba::TRANSPARENT));
        assert_eq!(
            composite(&doc),
            composite_frame(&doc, doc.active_frame()).unwrap()
        );
    }

    #[test]
    fn composite_cell_returns_blank_for_an_out_of_bounds_frame() {
        let doc = Document::new(2, 2);
        assert_eq!(composite_cell(&doc, 1, 0, 0), Cell::BLANK);
    }

    #[test]
    fn composite_cell_skips_a_hidden_layers_content() {
        let mut doc = Document::new(2, 2);
        let mut history = crate::edit::History::new();
        doc.set_cell(0, 0, 0, cell('b', Rgba::WHITE, Rgba(5, 5, 5, 255))); // bottom layer content

        let add = crate::layer_ops::add_layer(&doc, 1).unwrap();
        history.apply(&mut doc, add); // layer 1 becomes active
        doc.set_cell(1, 0, 0, cell('t', Rgba::WHITE, Rgba(9, 9, 9, 255)));

        // Sanity: with both layers visible, the top layer wins.
        assert_eq!(composite_cell(&doc, 0, 0, 0).ch, 't');

        let hide = crate::layer_ops::set_layer_visibility(&doc, 1, false)
            .unwrap()
            .unwrap();
        history.apply(&mut doc, hide);
        assert_eq!(
            composite_cell(&doc, 0, 0, 0).ch,
            'b',
            "a hidden top layer must be excluded, exposing the bottom layer's content"
        );
    }

    #[test]
    fn composite_frame_excludes_a_hidden_layers_content_too() {
        let mut doc = Document::new(2, 2);
        let mut history = crate::edit::History::new();
        doc.set_cell(0, 0, 0, cell('b', Rgba::WHITE, Rgba(5, 5, 5, 255)));
        let add = crate::layer_ops::add_layer(&doc, 1).unwrap();
        history.apply(&mut doc, add);
        doc.set_cell(1, 0, 0, cell('t', Rgba::WHITE, Rgba(9, 9, 9, 255)));
        let hide = crate::layer_ops::set_layer_visibility(&doc, 1, false)
            .unwrap()
            .unwrap();
        history.apply(&mut doc, hide);

        let out = composite_frame(&doc, 0).unwrap();
        assert_eq!(
            out[0][0].ch, 'b',
            "composite_frame must exclude a hidden layer's content via composite_cell"
        );
    }
}
