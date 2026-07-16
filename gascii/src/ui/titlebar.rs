//! The custom 30px title bar, and the window-edge resize handling that comes with turning the OS
//! decorations off.
//!
//! With `with_decorations(false)` winit gives us no frame, which means no drag region, no caption
//! buttons, and — the part that is easy to forget — no resize borders. All three are reimplemented
//! here on top of `ViewportCommand`.

use eframe::egui::{
    self, Align2, Context, CursorIcon, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind, Ui,
    Vec2, ViewportCommand,
};

use super::theme;
use super::widgets;
use crate::app::GasciiApp;
use crate::fonts;

pub const HEIGHT: f32 = 32.0;
/// Caption box side.
const BOX: f32 = 16.0;
/// How far in from the window edge still counts as a resize grip.
const RESIZE_GRIP: f32 = 5.0;
/// Pinstripe band height.
const PINSTRIPE_H: f32 = 9.0;

pub fn show(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    let bar = ui.max_rect();
    let painter = ui.painter().clone();

    // The whole bar drags the window, except where a caption box takes the click.
    let drag = ui.interact(bar, ui.id().with("titlebar_drag"), Sense::click_and_drag());
    if drag.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        toggle_maximized(ui.ctx());
    }

    let title = app.window_title();
    let font = fonts::ui_semibold_id(fonts::size::BODY);
    let text_w = ui
        .painter()
        .layout_no_wrap(title.clone(), font.clone(), t.fg_text)
        .size()
        .x;

    // Caption boxes hug the trailing edge; the title sits centred in the bar with pinstripes filling
    // the gaps either side of it.
    let boxes_w = BOX * 3.0 + 6.0 * 2.0;
    let center = bar.center().x;
    let title_rect = Rect::from_center_size(
        Pos2::new(center, bar.center().y),
        Vec2::new(text_w + 20.0, bar.height()),
    );
    painter.text(title_rect.center(), Align2::CENTER_CENTER, &title, font, t.fg_text);

    let band = |from: f32, to: f32| {
        if to - from > 8.0 {
            widgets::pinstripe(
                &painter,
                Rect::from_min_max(
                    Pos2::new(from, bar.center().y - PINSTRIPE_H / 2.0),
                    Pos2::new(to, bar.center().y + PINSTRIPE_H / 2.0),
                ),
                t.pinstripe,
            );
        }
    };
    band(bar.min.x + 10.0, title_rect.min.x);
    band(title_rect.max.x, bar.max.x - boxes_w - 16.0);

    // Caption boxes, laid out from the trailing edge inwards.
    let mut x = bar.max.x - 10.0 - BOX;
    for (label, action) in [("×", Caption::Close), ("□", Caption::Max), ("–", Caption::Min)] {
        let r = Rect::from_min_size(Pos2::new(x, bar.center().y - BOX / 2.0), Vec2::splat(BOX));
        let resp = ui.interact(r, ui.id().with(label), Sense::click());
        // Pressed inverts (the shared selection rule); hover otherwise gets the ordinary wash, not
        // full inversion — a caption box is not itself a toggled state.
        let pressed = resp.is_pointer_button_down_on();
        let (fill, fg) = if pressed {
            (t.bg_inverse, t.fg_inverse)
        } else if resp.hovered() {
            (t.bg_hover, t.fg_text)
        } else {
            (egui::Color32::TRANSPARENT, t.fg_text)
        };
        painter.rect_filled(r, 0.0, fill);
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, t.border_strong), StrokeKind::Inside);
        painter.text(r.center(), Align2::CENTER_CENTER, label, fonts::mono_id(fonts::size::CAPTION), fg);
        if resp.clicked() {
            match action {
                // Routed through the ordinary close request, so the unsaved-changes veto still runs.
                Caption::Close => ui.ctx().send_viewport_cmd(ViewportCommand::Close),
                Caption::Max => toggle_maximized(ui.ctx()),
                Caption::Min => ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
            }
        }
        x -= BOX + 6.0;
    }
}

enum Caption {
    Close,
    Max,
    Min,
}

fn toggle_maximized(ctx: &Context) {
    let is_max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    ctx.send_viewport_cmd(ViewportCommand::Maximized(!is_max));
}

/// Which edge or corner `pos` is grabbing, if any.
///
/// Pure so the hit-test is testable without a window: the corner cases (a corner must win over
/// either edge it touches) are exactly where this kind of code goes wrong.
fn resize_hit(pos: Pos2, rect: Rect, grip: f32) -> Option<ResizeDirection> {
    let w = pos.x - rect.min.x < grip;
    let e = rect.max.x - pos.x < grip;
    let n = pos.y - rect.min.y < grip;
    let s = rect.max.y - pos.y < grip;
    if !rect.contains(pos) {
        return None;
    }
    // Corners first — a corner grip overlaps both of its edges, and resizing one axis when the user
    // grabbed a corner is the classic bug here.
    match (n, s, e, w) {
        (true, _, _, true) => Some(ResizeDirection::NorthWest),
        (true, _, true, _) => Some(ResizeDirection::NorthEast),
        (_, true, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, true, _) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::North),
        (_, true, _, _) => Some(ResizeDirection::South),
        (_, _, true, _) => Some(ResizeDirection::East),
        (_, _, _, true) => Some(ResizeDirection::West),
        _ => None,
    }
}

fn cursor_for(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

/// The resize border winit no longer provides. Runs before the panels so the grip wins over any
/// widget sitting under the window's outermost few pixels.
///
/// Returns true while the pointer is over a grip, so the caller can suppress the canvas's own
/// interaction for that frame.
pub fn handle_resize(ctx: &Context) -> bool {
    // A maximized window has no edges to drag.
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return false;
    }
    // `viewport_rect`, not `content_rect`: the grip belongs at the true window edge.
    let rect = ctx.viewport_rect();
    let Some(pos) = ctx.pointer_latest_pos() else {
        return false;
    };
    let Some(dir) = resize_hit(pos, rect, RESIZE_GRIP) else {
        return false;
    };
    ctx.set_cursor_icon(cursor_for(dir));
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
    true
}

/// The 1px window edge the OS frame used to draw.
pub fn paint_window_edge(ctx: &Context) {
    let t = theme::current(ctx);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("window_edge"),
    ));
    painter.rect_stroke(
        ctx.viewport_rect(),
        0.0,
        Stroke::new(1.0, t.window_edge),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))
    }

    /// A corner must beat both of the edges it overlaps. Ordering the match with the edges first
    /// silently makes every corner resize one axis only.
    #[test]
    fn corners_win_over_the_edges_they_overlap() {
        let g = 5.0;
        assert_eq!(resize_hit(Pos2::new(1.0, 1.0), win(), g), Some(ResizeDirection::NorthWest));
        assert_eq!(resize_hit(Pos2::new(999.0, 1.0), win(), g), Some(ResizeDirection::NorthEast));
        assert_eq!(resize_hit(Pos2::new(1.0, 799.0), win(), g), Some(ResizeDirection::SouthWest));
        assert_eq!(resize_hit(Pos2::new(999.0, 799.0), win(), g), Some(ResizeDirection::SouthEast));
    }

    #[test]
    fn edges_resolve_to_their_own_direction() {
        let g = 5.0;
        assert_eq!(resize_hit(Pos2::new(500.0, 1.0), win(), g), Some(ResizeDirection::North));
        assert_eq!(resize_hit(Pos2::new(500.0, 799.0), win(), g), Some(ResizeDirection::South));
        assert_eq!(resize_hit(Pos2::new(1.0, 400.0), win(), g), Some(ResizeDirection::West));
        assert_eq!(resize_hit(Pos2::new(999.0, 400.0), win(), g), Some(ResizeDirection::East));
    }

    /// The grip is a thin ring. If the interior reported a direction, every click in the app would
    /// start a window resize instead of hitting a widget.
    #[test]
    fn the_interior_is_not_a_grip() {
        assert_eq!(resize_hit(Pos2::new(500.0, 400.0), win(), 5.0), None);
        assert_eq!(resize_hit(Pos2::new(6.0, 6.0), win(), 5.0), None);
        // Outside the window entirely.
        assert_eq!(resize_hit(Pos2::new(-5.0, 400.0), win(), 5.0), None);
    }
}
