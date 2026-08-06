//! Tool icons. Most are transcribed from SVG paths (quoted above each constant); the eyedropper is
//! an original pipette design authored directly in viewBox space. Each `ToolDef`/
//! `PluginToolCapabilities` row carries its own `IconPath` slice — this module owns the built-ins'
//! path data and the shared paint/fallback logic, and no longer knows about `ToolKind` at all.
//!
//! egui cannot render SVG without a new dependency, so each path is stored as polylines in its
//! source 16×16 viewBox and stroked at 1.4px, exactly as the source markup specifies. Curves
//! (the fill droplet, the brush head) are approximated with a few points — at the 17px they render
//! at, the difference from a true bézier is below one pixel.
//!
//! Keeping the coordinates in viewBox space means `paint` is the only place that knows about
//! scaling, and the numbers here can be diffed against the quoted paths by eye.

use eframe::egui::{Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use gascii_plugin_api::IconPath;

/// The viewBox the source paths are authored in.
const VIEW_BOX: f32 = 16.0;
/// Stroke width in viewBox units (`stroke-width="1.4"` in the source paths).
const STROKE_W: f32 = 1.4;
/// The Selection icon's `stroke-dasharray="2.5 2"`.
const DASH: (f32, f32) = (2.5, 2.0);

// M3 13l1-4 7.5-7.5 3 3L7 12l-4 1z
pub(crate) const PENCIL: &[IconPath] = &[IconPath::closed(&[
    (3.0, 13.0),
    (4.0, 9.0),
    (11.5, 1.5),
    (14.5, 4.5),
    (7.0, 12.0),
])];

// M6 13l-3.5-3.5L9 3l3.5 3.5L7 12H14
pub(crate) const ERASER: &[IconPath] = &[IconPath::open(&[
    (6.0, 13.0),
    (2.5, 9.5),
    (9.0, 3.0),
    (12.5, 6.5),
    (7.0, 12.0),
    (14.0, 12.0),
])];

// pipette: squeeze bulb (top-left), tapered barrel to the tip (bottom-right), collar band
pub(crate) const EYEDROPPER: &[IconPath] = &[
    IconPath::closed(&[(2.5, 5.0), (2.0, 3.0), (3.5, 2.0), (5.5, 2.5), (6.0, 4.5)]),
    IconPath::open(&[(5.0, 4.0), (12.5, 11.5), (13.5, 13.5)]),
    IconPath::open(&[(4.0, 5.5), (6.5, 3.0)]),
];

// M3 4V3h10v1M8 3v10M6.5 13h3
pub(crate) const TEXT: &[IconPath] = &[
    IconPath::open(&[(3.0, 4.0), (3.0, 3.0), (13.0, 3.0), (13.0, 4.0)]),
    IconPath::open(&[(8.0, 3.0), (8.0, 13.0)]),
    IconPath::open(&[(6.5, 13.0), (9.5, 13.0)]),
];

// M7 2l5 5-4.5 4.5a1.4 1.4 0 01-2 0L2.5 8.5 7 4  +  the droplet
pub(crate) const FILL: &[IconPath] = &[
    IconPath::open(&[
        (7.0, 2.0),
        (12.0, 7.0),
        (7.5, 11.5),
        (6.5, 12.2), // the a1.4 arc, two points is plenty at this size
        (5.5, 11.5),
        (2.5, 8.5),
        (7.0, 4.0),
    ]),
    IconPath::closed(&[
        (13.0, 10.5),
        (14.2, 13.0),
        (13.6, 14.2),
        (12.4, 14.2),
        (11.8, 13.0),
    ]),
];

// rect x=2.5 y=3.5 w=11 h=9
pub(crate) const RECTANGLE: &[IconPath] = &[IconPath::closed(&[
    (2.5, 3.5),
    (13.5, 3.5),
    (13.5, 12.5),
    (2.5, 12.5),
])];

// M2.5 13.5l11-11
pub(crate) const LINE: &[IconPath] = &[IconPath::open(&[(2.5, 13.5), (13.5, 2.5)])];

// rect x=2.5 y=2.5 w=11 h=11, dashed
pub(crate) const SELECTION: &[IconPath] = &[IconPath {
    pts: &[
        (2.5, 2.5),
        (13.5, 2.5),
        (13.5, 13.5),
        (2.5, 13.5),
        (2.5, 2.5),
    ],
    closed: false, // drawn as an explicitly-closed polyline so the dash pattern runs continuously
    dashed: true,
}];

/// Strokes `icon` to fill `rect`, which is assumed square. Scaling lives only here. An empty
/// `icon` slice falls back to `fallback_letter`, painted centered in the cell — a plugin tool with
/// no icon of its own gets a legible cell rather than a blank one.
pub fn paint(
    painter: &Painter,
    icon: &[IconPath],
    rect: Rect,
    color: Color32,
    fallback_letter: char,
) {
    if icon.is_empty() {
        painter.text(
            rect.center(),
            eframe::egui::Align2::CENTER_CENTER,
            fallback_letter,
            FontId::monospace(rect.height() * 0.55),
            color,
        );
        return;
    }

    let scale = rect.width() / VIEW_BOX;
    let map = |(x, y): (f32, f32)| rect.min + Vec2::new(x * scale, y * scale);
    let stroke = Stroke::new(STROKE_W * scale, color);

    for sub in icon {
        let mut pts: Vec<Pos2> = sub.pts.iter().copied().map(map).collect();
        if sub.dashed {
            painter.extend(Shape::dashed_line(
                &pts,
                stroke,
                DASH.0 * scale,
                DASH.1 * scale,
            ));
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

    const BUILTIN_ICONS: [&[IconPath]; 8] = [
        PENCIL, ERASER, EYEDROPPER, TEXT, FILL, RECTANGLE, LINE, SELECTION,
    ];

    /// Every `tools()` row must carry either a real icon (>=2 points per sub-path) or an empty
    /// slice that falls back cleanly (never a panic, never a blank cell) — replaces the old total
    /// `paths(kind)` match's own exhaustiveness guarantee now that icons are per-row data.
    #[test]
    fn every_tools_row_has_a_paintable_icon_or_falls_back_cleanly() {
        for d in crate::app::tools() {
            for sub in d.icon {
                assert!(
                    sub.pts.len() >= 2,
                    "{}: a sub-path has fewer than 2 points",
                    d.name
                );
            }
        }

        let ctx = eframe::egui::Context::default();
        let _ = ctx.run_ui(eframe::egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(24.0));
            // An empty icon must render without panicking, falling back to a letter.
            paint(&painter, &[], rect, Color32::WHITE, 'Z');
        });
    }

    /// Every built-in row's icon is authored in a 16x16 viewBox; a stray coordinate outside it
    /// would silently paint over the cell's border or bleed into the neighbouring tool.
    #[test]
    fn every_builtin_icon_point_stays_inside_the_view_box() {
        for icon in BUILTIN_ICONS {
            for sub in icon {
                for &(x, y) in sub.pts {
                    assert!(
                        (0.0..=VIEW_BOX).contains(&x) && (0.0..=VIEW_BOX).contains(&y),
                        "a built-in icon has a point outside the 16x16 viewBox: ({x}, {y})"
                    );
                }
            }
        }
    }

    /// The bulb is the feature that distinguishes the eyedropper's silhouette from the pencil's.
    #[test]
    fn eyedropper_has_a_closed_bulb_subpath() {
        assert!(
            EYEDROPPER.iter().any(|s| s.closed),
            "eyedropper is missing its closed bulb subpath"
        );
    }

    /// D4: the brush icon now ships from the plugin crate, not from this module — this module no
    /// longer defines a `BRUSH` const at all, and Brush's `tools()` row must carry the plugin's icon
    /// verbatim (identity check by pointer, not just by value, so a coincidental shape match can't
    /// paper over a stale copy left behind in this crate).
    #[test]
    fn the_brush_icon_now_comes_from_the_plugin_crate() {
        let brush = crate::app::tool_def(crate::app::BRUSH_KIND);
        assert!(
            std::ptr::eq(brush.icon, gascii_density_brush::BRUSH_ICON),
            "Brush's icon must be the plugin crate's own const, not a host copy"
        );
    }
}
