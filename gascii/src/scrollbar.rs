//! Desk-edge scrollbars for the canvas: a horizontal bar along the bottom and a vertical bar
//! along the right, shown per-axis only while the document overflows the view on that axis.
//!
//! Split in two halves so the canvas's frame pipeline can use them at the right times:
//! [`interact`] runs in the input phase (before stroke routing, so a press on a bar never starts
//! a stroke), and [`paint`] runs at the end of the paint phase (so the bars sit above the
//! document card). Both derive their geometry from the same [`Axis`] model, recomputed from the
//! viewport's current pan, so the painted thumb always reflects this frame's final state.

use eframe::egui::{self, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::viewport::Viewport;

/// Track thickness, flush against the canvas edge.
const THICKNESS: f32 = 12.0;
/// Inset between the track edge and the thumb fill.
const INSET: f32 = 2.0;
/// Floor on the thumb's length so it stays grabbable however large the document gets.
const MIN_THUMB: f32 = 24.0;

/// One axis's scroll model, in screen pixels at the current zoom.
///
/// The document occupies `[0, doc]`; the view shows `[-pan, -pan + view]`. The bar's range is the
/// *union* of the two: a view panned past the document's edge still maps to a valid thumb instead
/// of clamping (which would snap the content), so dragging the thumb back is also the recovery
/// gesture for a document flung far off-screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Axis {
    doc: f32,
    view: f32,
    pan: f32,
}

impl Axis {
    pub(crate) fn new(doc: f32, view: f32, pan: f32) -> Self {
        Axis { doc, view, pan }
    }

    fn lo(self) -> f32 {
        (-self.pan).min(0.0)
    }

    fn hi(self) -> f32 {
        (-self.pan + self.view).max(self.doc)
    }

    /// Whether this axis needs a bar at all. The half-pixel slack keeps a document that exactly
    /// fits (or a centering pan's float residue) from flickering a bar in and out.
    pub(crate) fn overflows(self) -> bool {
        self.hi() - self.lo() > self.view + 0.5
    }

    /// `(start, len)` of the thumb within `[0, track]`, or `None` when nothing overflows.
    ///
    /// The position maps the scrollable remainder onto the track the thumb doesn't cover, so a
    /// min-length-clamped thumb still reaches both track ends exactly at the range's ends.
    pub(crate) fn thumb(self, track: f32) -> Option<(f32, f32)> {
        if !self.overflows() || track <= 0.0 {
            return None;
        }
        let range = self.hi() - self.lo();
        let len = (track * self.view / range).max(MIN_THUMB.min(track / 2.0));
        let scrollable = range - self.view;
        let start = (-self.pan - self.lo()) / scrollable * (track - len);
        Some((start.clamp(0.0, track - len), len))
    }

    /// The pan delta equivalent to dragging the thumb by `drag` track pixels. Dragging the thumb
    /// forward moves the view forward through the content, i.e. pans the content backward.
    pub(crate) fn pan_delta(self, track: f32, drag: f32) -> f32 {
        let Some((_, len)) = self.thumb(track) else {
            return 0.0;
        };
        if track - len <= 0.0 {
            return 0.0;
        }
        let scrollable = (self.hi() - self.lo()) - self.view;
        -drag * scrollable / (track - len)
    }

    /// The pan that centers the view on the content under track position `t` — the track-click
    /// jump. Clamped so the jump never overscrolls past either end of the range.
    pub(crate) fn pan_for_track_click(self, track: f32, t: f32) -> f32 {
        let range = self.hi() - self.lo();
        let target = self.lo() + (t / track).clamp(0.0, 1.0) * range;
        let view_lo = (target - self.view / 2.0).clamp(self.lo(), self.hi() - self.view);
        -view_lo
    }
}

/// Per-bar visual state, captured at interaction time and consumed at paint time.
#[derive(Default, Clone, Copy)]
pub(crate) struct BarVis {
    pub hovered: bool,
    pub dragged: bool,
}

/// What the canvas's frame pipeline needs back from [`interact`]: the visual state for the later
/// [`paint`] call, plus `pointer_on_bar` — the press-routing gate that keeps a click on a bar
/// from also starting a stroke (same contract as the window resize grip's flag).
#[derive(Default, Clone, Copy)]
pub(crate) struct Bars {
    pub h: BarVis,
    pub v: BarVis,
    pub pointer_on_bar: bool,
}

fn axes(vp: &Viewport, rect: Rect, doc_size: Vec2) -> (Axis, Axis) {
    (
        Axis::new(doc_size.x, rect.width(), vp.pan.x),
        Axis::new(doc_size.y, rect.height(), vp.pan.y),
    )
}

/// The two track rects. When both bars show, each yields the shared corner to the other so they
/// never overlap.
fn tracks(rect: Rect, both: bool) -> (Rect, Rect) {
    let corner = if both { THICKNESS } else { 0.0 };
    let h = Rect::from_min_max(
        Pos2::new(rect.left(), rect.bottom() - THICKNESS),
        Pos2::new(rect.right() - corner, rect.bottom()),
    );
    let v = Rect::from_min_max(
        Pos2::new(rect.right() - THICKNESS, rect.top()),
        Pos2::new(rect.right(), rect.bottom() - corner),
    );
    (h, v)
}

fn thumb_rect(track: Rect, horizontal: bool, start: f32, len: f32) -> Rect {
    if horizontal {
        Rect::from_min_max(
            Pos2::new(track.left() + start, track.top() + INSET),
            Pos2::new(track.left() + start + len, track.bottom() - INSET),
        )
    } else {
        Rect::from_min_max(
            Pos2::new(track.left() + INSET, track.top() + start),
            Pos2::new(track.right() - INSET, track.top() + start + len),
        )
    }
}

/// One bar's interaction: thumb drag pans proportionally, a click on the open track jumps the
/// view to center on the clicked point. The thumb is registered after the track so it wins the
/// hit-test where they overlap.
fn interact_bar(
    ui: &Ui,
    pan: &mut f32,
    axis: Axis,
    track: Rect,
    horizontal: bool,
    id: egui::Id,
) -> BarVis {
    let track_len = if horizontal {
        track.width()
    } else {
        track.height()
    };
    let Some((start, len)) = axis.thumb(track_len) else {
        return BarVis::default();
    };

    let track_resp = ui.interact(track, id.with("track"), Sense::click());
    if track_resp.clicked() {
        if let Some(pos) = track_resp.interact_pointer_pos() {
            let t = if horizontal {
                pos.x - track.left()
            } else {
                pos.y - track.top()
            };
            *pan = axis.pan_for_track_click(track_len, t);
        }
    }

    let thumb = thumb_rect(track, horizontal, start, len);
    let thumb_resp = ui.interact(thumb, id.with("thumb"), Sense::drag());
    if thumb_resp.dragged() {
        let d = thumb_resp.drag_delta();
        *pan += axis.pan_delta(track_len, if horizontal { d.x } else { d.y });
    }

    BarVis {
        hovered: thumb_resp.hovered() || track_resp.hovered(),
        dragged: thumb_resp.dragged(),
    }
}

/// Input-phase half: registers the bars' interactions and applies any drag/jump to the viewport's
/// pan. Must run before the canvas's press routing — see [`Bars::pointer_on_bar`].
pub(crate) fn interact(ui: &Ui, vp: &mut Viewport, rect: Rect, doc_size: Vec2) -> Bars {
    let (h_axis, v_axis) = axes(vp, rect, doc_size);
    let both = h_axis.overflows() && v_axis.overflows();
    let (h_track, v_track) = tracks(rect, both);
    let id = ui.id().with("canvas_scrollbar");

    let mut bars = Bars::default();
    if h_axis.overflows() {
        bars.h = interact_bar(ui, &mut vp.pan.x, h_axis, h_track, true, id.with("h"));
    }
    if v_axis.overflows() {
        bars.v = interact_bar(ui, &mut vp.pan.y, v_axis, v_track, false, id.with("v"));
    }

    let ptr = ui.input(|i| i.pointer.latest_pos());
    let over = ptr.is_some_and(|p| {
        (h_axis.overflows() && h_track.contains(p)) || (v_axis.overflows() && v_track.contains(p))
    });
    bars.pointer_on_bar = over || bars.h.dragged || bars.v.dragged;
    bars
}

fn paint_bar(ui: &Ui, t: &crate::ui::theme::Tokens, thumb: Rect, vis: BarVis) {
    let painter = ui.painter();
    // The chrome's four-state contract, floating over the desk: an idle thumb is a small panel
    // card, hover adds the shared wash, and dragging inverts.
    let fill = if vis.dragged {
        t.bg_inverse
    } else {
        t.bg_panel
    };
    painter.rect_filled(thumb, 0.0, fill);
    if vis.hovered && !vis.dragged {
        painter.rect_filled(thumb, 0.0, t.bg_hover);
    }
    let border = if vis.hovered || vis.dragged {
        t.border_strong
    } else {
        t.border_soft
    };
    painter.rect_stroke(thumb, 0.0, Stroke::new(1.0, border), StrokeKind::Inside);
}

/// Paint-phase half: draws the thumbs from the viewport's *final* pan for this frame, so they
/// never lag a same-frame wheel pan, zoom, or their own drag.
pub(crate) fn paint(ui: &Ui, vp: &Viewport, rect: Rect, doc_size: Vec2, bars: Bars) {
    let t = crate::ui::theme::current(ui.ctx());
    let (h_axis, v_axis) = axes(vp, rect, doc_size);
    let both = h_axis.overflows() && v_axis.overflows();
    let (h_track, v_track) = tracks(rect, both);

    if let Some((start, len)) = h_axis.thumb(h_track.width()) {
        paint_bar(ui, &t, thumb_rect(h_track, true, start, len), bars.h);
    }
    if let Some((start, len)) = v_axis.thumb(v_track.height()) {
        paint_bar(ui, &t, thumb_rect(v_track, false, start, len), bars.v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_that_fits_shows_no_bar() {
        // Centered by a fit: the doc is smaller than the view on both sides.
        let axis = Axis::new(500.0, 800.0, 150.0);
        assert!(!axis.overflows());
        assert_eq!(axis.thumb(800.0), None);
    }

    #[test]
    fn an_exact_fit_shows_no_bar_despite_float_residue() {
        let axis = Axis::new(800.0, 800.0, 0.25);
        assert!(!axis.overflows());
    }

    #[test]
    fn an_overflowing_document_shows_a_proportional_thumb() {
        // Doc twice the view, view at the doc's start: thumb covers the first half of the track.
        let axis = Axis::new(1600.0, 800.0, 0.0);
        let (start, len) = axis.thumb(400.0).unwrap();
        assert_eq!(start, 0.0);
        assert!((len - 200.0).abs() < 0.01, "len={len}");
    }

    #[test]
    fn thumb_reaches_the_far_end_when_panned_to_the_far_end() {
        // View showing the doc's last page: pan = view - doc.
        let axis = Axis::new(1600.0, 800.0, -800.0);
        let (start, len) = axis.thumb(400.0).unwrap();
        assert!(
            (start + len - 400.0).abs() < 0.01,
            "start={start} len={len}"
        );
    }

    #[test]
    fn min_thumb_clamp_still_reaches_both_ends() {
        // A huge doc would yield a sub-pixel thumb; the clamp keeps it grabbable and the
        // remainder mapping keeps its travel exact.
        let doc = 1_000_000.0;
        let at_start = Axis::new(doc, 800.0, 0.0).thumb(400.0).unwrap();
        assert_eq!(at_start.1, MIN_THUMB);
        assert_eq!(at_start.0, 0.0);

        let at_end = Axis::new(doc, 800.0, 800.0 - doc).thumb(400.0).unwrap();
        assert!((at_end.0 + at_end.1 - 400.0).abs() < 0.01);
    }

    #[test]
    fn overscroll_extends_the_range_instead_of_pinning_the_thumb() {
        // Doc flung off to the right (positive pan beyond the view): the union range grows and
        // the thumb sits hard at the start, with room to drag back.
        let axis = Axis::new(1600.0, 800.0, 400.0);
        assert!(axis.overflows());
        let (start, _) = axis.thumb(400.0).unwrap();
        assert_eq!(start, 0.0);
        // And dragging forward pans the content backward (negative delta).
        assert!(axis.pan_delta(400.0, 50.0) < 0.0);
    }

    #[test]
    fn drag_round_trips_through_pan() {
        // Dragging the thumb by d and recomputing must move the thumb by exactly d.
        let track = 400.0;
        let axis = Axis::new(1600.0, 800.0, -200.0);
        let (start_before, _) = axis.thumb(track).unwrap();
        let drag = 60.0;
        let moved = Axis::new(1600.0, 800.0, -200.0 + axis.pan_delta(track, drag));
        let (start_after, _) = moved.thumb(track).unwrap();
        assert!(
            (start_after - start_before - drag).abs() < 0.01,
            "moved {}",
            start_after - start_before
        );
    }

    #[test]
    fn track_click_centers_and_clamps() {
        let track = 400.0;
        let axis = Axis::new(1600.0, 800.0, 0.0);
        // Mid-track click centers the view on the doc's midpoint.
        let pan = axis.pan_for_track_click(track, 200.0);
        assert!((pan - -400.0).abs() < 0.01, "pan={pan}");
        // End clicks clamp to the range's ends instead of overscrolling.
        assert_eq!(axis.pan_for_track_click(track, 0.0), 0.0);
        assert!((axis.pan_for_track_click(track, track) - (800.0 - 1600.0)).abs() < 0.01);
    }

    #[test]
    fn degenerate_tracks_and_fits_never_divide_by_zero() {
        let fits = Axis::new(500.0, 800.0, 0.0);
        assert_eq!(fits.pan_delta(400.0, 10.0), 0.0);
        let overflowing = Axis::new(1600.0, 800.0, 0.0);
        assert_eq!(overflowing.thumb(0.0), None);
        assert_eq!(overflowing.pan_delta(0.0, 10.0), 0.0);
    }

    #[test]
    fn tracks_yield_the_shared_corner_only_when_both_bars_show() {
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(800.0, 600.0));
        let (h, v) = tracks(rect, true);
        assert_eq!(h.right(), 800.0 - THICKNESS);
        assert_eq!(v.bottom(), 600.0 - THICKNESS);
        assert!(h.intersect(v).size().min_elem() <= 0.0, "bars overlap");

        let (h_solo, v_solo) = tracks(rect, false);
        assert_eq!(h_solo.right(), 800.0);
        assert_eq!(v_solo.bottom(), 600.0);
    }
}
