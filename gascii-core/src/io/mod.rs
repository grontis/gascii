//! Layer-general compositing and file I/O. `composite()` is the single place that turns
//! `Document::layers` into one flattened sheet of `Cell`s; every exporter builds on it rather
//! than re-walking layers itself.

pub mod export_text;
pub mod gascii_json;

use crate::model::{Cell, Document};

/// Flattens `doc`'s layers into one sheet, bottom-to-top alpha-over compositing.
pub fn composite(doc: &Document) -> Vec<Vec<Cell>> {
    let (w, h) = (doc.width as usize, doc.height as usize);
    let mut out = vec![vec![Cell::BLANK; w]; h];
    for layer in 0..doc.layers.len() {
        for y in 0..doc.height {
            for x in 0..doc.width {
                let Some(&over) = doc.cell(layer, x, y) else { continue };
                let dst = &mut out[y as usize][x as usize];
                *dst = alpha_over(*dst, over);
            }
        }
    }
    out
}

fn alpha_over(under: Cell, over: Cell) -> Cell {
    if over.is_blank() {
        return under; // fully transparent — nothing to composite (Blank is alpha)
    }
    if over.bg.is_transparent() {
        return Cell { ch: over.ch, fg: over.fg, bg: under.bg }; // glyph/fg opaque, bg shows through
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
    Cell { ch: over.ch, fg: over.fg, bg }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgba;

    fn cell(ch: char, fg: Rgba, bg: Rgba) -> Cell {
        Cell { ch, fg, bg }
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
        doc.layers.push(crate::model::Layer::blank(2, 2)); // top layer, entirely Blank
        let out = composite(&doc);
        assert_eq!(out[0][0], *doc.cell(0, 0, 0).unwrap());
    }

    #[test]
    fn fully_opaque_top_layer_completely_replaces_bottom() {
        let mut doc = Document::new(2, 2);
        doc.set_cell(0, 0, 0, cell('b', Rgba::WHITE, Rgba(5, 5, 5, 255)));
        doc.layers.push(crate::model::Layer::blank(2, 2));
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
        doc.layers.push(crate::model::Layer::blank(2, 2));
        let top_bg = Rgba(255, 255, 255, 128);
        doc.set_cell(1, 0, 0, cell('t', Rgba::WHITE, top_bg));
        let out = composite(&doc);
        let blended = out[0][0].bg;
        assert!(blended.0 > bottom_bg.0 && blended.0 < top_bg.0, "red channel should sit strictly between");
        assert!(blended.1 > bottom_bg.1 && blended.1 < top_bg.1, "green channel should sit strictly between");
        assert!(blended.2 > bottom_bg.2 && blended.2 < top_bg.2, "blue channel should sit strictly between");
    }

    #[test]
    fn transparent_bg_over_opaque_bg_shows_bottom_bg_through() {
        let mut doc = Document::new(1, 1);
        doc.set_cell(0, 0, 0, cell(' ', Rgba::WHITE, Rgba(7, 8, 9, 255)));
        doc.layers.push(crate::model::Layer::blank(1, 1));
        doc.set_cell(1, 0, 0, cell('t', Rgba(1, 1, 1, 255), Rgba::TRANSPARENT));
        let out = composite(&doc);
        assert_eq!(out[0][0].ch, 't');
        assert_eq!(out[0][0].fg, Rgba(1, 1, 1, 255));
        assert_eq!(out[0][0].bg, Rgba(7, 8, 9, 255));
    }
}
