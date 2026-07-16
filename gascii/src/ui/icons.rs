//! The nine tool icons, transcribed from SVG paths (quoted above each constant).
//!
//! egui cannot render SVG without a new dependency, so each path is stored as polylines in its
//! source 16×16 viewBox and stroked at 1.4px, exactly as the source markup specifies. Curves
//! (the fill droplet, the brush head) are approximated with a few points — at the 17px they render
//! at, the difference from a true bézier is below one pixel.
//!
//! Keeping the coordinates in viewBox space means `paint` is the only place that knows about
//! scaling, and the numbers here can be diffed against the quoted paths by eye.

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

use crate::app::ToolKind;

/// The viewBox the source paths are authored in.
const VIEW_BOX: f32 = 16.0;
/// Stroke width in viewBox units (`stroke-width="1.4"` in the source paths).
const STROKE_W: f32 = 1.4;
/// The Selection icon's `stroke-dasharray="2.5 2"`.
const DASH: (f32, f32) = (2.5, 2.0);

struct SubPath {
    pts: &'static [(f32, f32)],
    closed: bool,
    dashed: bool,
}

const fn open(pts: &'static [(f32, f32)]) -> SubPath {
    SubPath { pts, closed: false, dashed: false }
}

const fn closed(pts: &'static [(f32, f32)]) -> SubPath {
    SubPath { pts, closed: true, dashed: false }
}

// M3 13l1-4 7.5-7.5 3 3L7 12l-4 1z
const PENCIL: &[SubPath] =
    &[closed(&[(3.0, 13.0), (4.0, 9.0), (11.5, 1.5), (14.5, 4.5), (7.0, 12.0)])];

// M6 13l-3.5-3.5L9 3l3.5 3.5L7 12H14
const ERASER: &[SubPath] =
    &[open(&[(6.0, 13.0), (2.5, 9.5), (9.0, 3.0), (12.5, 6.5), (7.0, 12.0), (14.0, 12.0)])];

// M8 5l-5 5v3h3l5-5M8 5l2-2 3 3-2 2M8 5l3 3
const EYEDROPPER: &[SubPath] = &[
    open(&[(8.0, 5.0), (3.0, 10.0), (3.0, 13.0), (6.0, 13.0), (11.0, 8.0)]),
    open(&[(8.0, 5.0), (10.0, 3.0), (13.0, 6.0), (11.0, 8.0)]),
    open(&[(8.0, 5.0), (11.0, 8.0)]),
];

// M3 4V3h10v1M8 3v10M6.5 13h3
const TEXT: &[SubPath] = &[
    open(&[(3.0, 4.0), (3.0, 3.0), (13.0, 3.0), (13.0, 4.0)]),
    open(&[(8.0, 3.0), (8.0, 13.0)]),
    open(&[(6.5, 13.0), (9.5, 13.0)]),
];

// M7 2l5 5-4.5 4.5a1.4 1.4 0 01-2 0L2.5 8.5 7 4  +  the droplet
const FILL: &[SubPath] = &[
    open(&[
        (7.0, 2.0),
        (12.0, 7.0),
        (7.5, 11.5),
        (6.5, 12.2), // the a1.4 arc, two points is plenty at this size
        (5.5, 11.5),
        (2.5, 8.5),
        (7.0, 4.0),
    ]),
    closed(&[(13.0, 10.5), (14.2, 13.0), (13.6, 14.2), (12.4, 14.2), (11.8, 13.0)]),
];

// rect x=2.5 y=3.5 w=11 h=9
const RECTANGLE: &[SubPath] =
    &[closed(&[(2.5, 3.5), (13.5, 3.5), (13.5, 12.5), (2.5, 12.5)])];

// M2.5 13.5l11-11
const LINE: &[SubPath] = &[open(&[(2.5, 13.5), (13.5, 2.5)])];

// rect x=2.5 y=2.5 w=11 h=11, dashed
const SELECTION: &[SubPath] = &[SubPath {
    pts: &[(2.5, 2.5), (13.5, 2.5), (13.5, 13.5), (2.5, 13.5), (2.5, 2.5)],
    closed: false, // drawn as an explicitly-closed polyline so the dash pattern runs continuously
    dashed: true,
}];

// M13 2c-3 1-6 4-7.5 6.5l2 2C10 9 13 6 14 3z  +  M5 9c-1.5.5-2 2-2 4 2 0 3.5-.5 4-2
const BRUSH: &[SubPath] = &[
    closed(&[
        (13.0, 2.0),
        (10.0, 3.0),
        (7.0, 5.5),
        (5.5, 8.5),
        (7.5, 10.5),
        (10.0, 9.0),
        (13.0, 6.0),
        (14.0, 3.0),
    ]),
    open(&[(5.0, 9.0), (3.5, 9.5), (3.0, 11.0), (3.0, 13.0), (5.0, 13.0), (6.5, 12.5), (7.0, 11.0)]),
];

fn paths(kind: ToolKind) -> &'static [SubPath] {
    match kind {
        ToolKind::Pencil => PENCIL,
        ToolKind::Eraser => ERASER,
        ToolKind::Eyedropper => EYEDROPPER,
        ToolKind::Text => TEXT,
        ToolKind::Fill => FILL,
        ToolKind::Rectangle => RECTANGLE,
        ToolKind::Line => LINE,
        ToolKind::Selection => SELECTION,
        ToolKind::Brush => BRUSH,
    }
}

/// Strokes `kind`'s icon to fill `rect`, which is assumed square. Scaling lives only here.
pub fn paint(painter: &Painter, kind: ToolKind, rect: Rect, color: Color32) {
    let scale = rect.width() / VIEW_BOX;
    let map = |(x, y): (f32, f32)| rect.min + Vec2::new(x * scale, y * scale);
    let stroke = Stroke::new(STROKE_W * scale, color);

    for sub in paths(kind) {
        let mut pts: Vec<Pos2> = sub.pts.iter().copied().map(map).collect();
        if sub.dashed {
            painter.extend(Shape::dashed_line(&pts, stroke, DASH.0 * scale, DASH.1 * scale));
        } else if sub.closed {
            painter.add(Shape::closed_line(pts, stroke));
        } else {
            // `Shape::line` on two points degenerates to nothing in some epaint versions; a
            // single-segment path is common enough here (Line, the eyedropper's shaft) to be worth
            // not relying on that.
            if pts.len() == 2 {
                painter.add(Shape::line_segment([pts[0], pts[1]], stroke));
            } else {
                pts.dedup();
                painter.add(Shape::line(pts, stroke));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ToolKind; 9] = [
        ToolKind::Pencil,
        ToolKind::Eraser,
        ToolKind::Eyedropper,
        ToolKind::Text,
        ToolKind::Fill,
        ToolKind::Rectangle,
        ToolKind::Line,
        ToolKind::Selection,
        ToolKind::Brush,
    ];

    /// Every tool has an icon. `paths` is a total match, so a new `ToolKind` fails to compile rather
    /// than rendering an empty 42px cell — but this also catches an entry wired to an empty slice.
    #[test]
    fn every_tool_kind_has_a_non_empty_icon() {
        for kind in ALL {
            let subs = paths(kind);
            assert!(!subs.is_empty(), "{kind:?} has no icon paths");
            for sub in subs {
                assert!(sub.pts.len() >= 2, "{kind:?} has a sub-path with fewer than 2 points");
            }
        }
    }

    /// The paths are authored in a 16×16 viewBox, and `paint` maps that box onto the cell. A stray
    /// coordinate outside it would silently paint over the cell's border or bleed into the
    /// neighbouring tool.
    #[test]
    fn every_icon_point_stays_inside_the_view_box() {
        for kind in ALL {
            for sub in paths(kind) {
                for &(x, y) in sub.pts {
                    assert!(
                        (0.0..=VIEW_BOX).contains(&x) && (0.0..=VIEW_BOX).contains(&y),
                        "{kind:?} has a point outside the 16x16 viewBox: ({x}, {y})"
                    );
                }
            }
        }
    }
}
