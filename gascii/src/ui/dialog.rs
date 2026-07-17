//! The shared modal-dialog surface every dialog (New, Resize, Export, the close/unsaved-changes
//! confirm) is built on: a themed frame around `egui::Modal`, and a right-aligned button row.
//!
//! `egui::Modal`'s backdrop only blocks egui's own occlusion-aware widgets. `gascii/src/canvas.rs`
//! polls raw pointer/keyboard state instead, so it does not stop there — every caller here also
//! needs an explicit flag gate through `GasciiApp::modal_open()` (see that function's doc comment).

use eframe::egui::{self, Align2, Color32, CornerRadius, Frame, Id, Margin, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::theme::{self, Tokens};
use super::widgets;
use crate::fonts;

/// Fixed content width for every dialog — the design spec's 340-360px, scaled up with the type
/// ramp's +2px bump.
const WIDTH: f32 = 380.0;
/// The title strip's height — the spec's 28px, scaled the same way.
const TITLE_H: f32 = 30.0;
/// The strip's close box — the spec's 12px, scaled.
const CLOSE_BOX: f32 = 14.0;
const BODY_MARGIN: f32 = 16.0;

/// A dialog frame's outcome for one frame: the body's return value (always present — the body runs
/// exactly once per call to [`modal`]), and whether the dialog should close (close box, backdrop
/// click, or Escape).
pub struct DialogResponse<R> {
    pub inner: R,
    pub dismissed: bool,
}

/// Draws one themed modal: a pinstriped title strip with a close box, then `body` inside the
/// standard content margin. `id` must be unique per concurrently-open dialog (there is at most one
/// in this app, but `egui::Modal` itself is keyed by it).
pub fn modal<R>(ctx: &egui::Context, id: &str, title: &str, body: impl FnOnce(&mut Ui) -> R) -> DialogResponse<R> {
    let t = theme::current(ctx);
    // Resolves to whichever theme is currently active, so this inherits the same hard offset
    // shadow `Tokens::visuals` already registered for popups — no separate shadow token.
    let shadow = ctx.style_of(ctx.theme()).visuals.popup_shadow;
    let frame = Frame::new()
        .fill(t.bg_panel)
        .stroke(Stroke::new(1.0, t.window_edge))
        .corner_radius(CornerRadius::ZERO)
        .shadow(shadow)
        .inner_margin(Margin::ZERO);

    let mut close_clicked = false;
    let mut inner = None;
    let resp = egui::Modal::new(Id::new(id)).frame(frame).show(ctx, |ui| {
        ui.set_width(WIDTH);
        close_clicked = title_strip(ui, &t, title);
        inner = Some(
            Frame::new()
                .inner_margin(Margin::same(BODY_MARGIN as i8))
                .show(ui, body)
                .inner,
        );
    });

    DialogResponse {
        inner: inner.expect("the body closure always runs inside Modal::show"),
        dismissed: close_clicked || resp.should_close(),
    }
}

/// The title strip: pinstripe bands flanking a centered title, a close box at the leading edge, a
/// soft rule underneath. Returns whether the close box was clicked.
fn title_strip(ui: &mut Ui, t: &Tokens, title: &str) -> bool {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, TITLE_H), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, t.bg_panel);

    let close_rect = Rect::from_min_size(
        Pos2::new(rect.min.x + 8.0, rect.center().y - CLOSE_BOX / 2.0),
        Vec2::splat(CLOSE_BOX),
    );
    let resp = ui.interact(close_rect, ui.id().with("dialog_close"), Sense::click());
    let pressed = resp.is_pointer_button_down_on();
    let (fill, fg) = if pressed {
        (t.bg_inverse, t.fg_inverse)
    } else if resp.hovered() {
        (t.bg_hover, t.fg_text)
    } else {
        (Color32::TRANSPARENT, t.fg_text)
    };
    painter.rect_filled(close_rect, 0.0, fill);
    painter.rect_stroke(close_rect, 0.0, Stroke::new(1.0, t.border_strong), egui::StrokeKind::Inside);
    painter.text(close_rect.center(), Align2::CENTER_CENTER, "×", fonts::mono_id(fonts::size::CAPTION), fg);

    let font = fonts::ui_semibold_id(fonts::size::BODY);
    let title_w = painter
        .layout_no_wrap(title.to_owned(), font.clone(), Color32::PLACEHOLDER)
        .size()
        .x;
    let title_rect = Rect::from_center_size(rect.center(), Vec2::new(title_w + 20.0, TITLE_H));
    painter.text(title_rect.center(), Align2::CENTER_CENTER, title, font, t.fg_text);

    let band = |from: f32, to: f32| {
        if to - from > 8.0 {
            widgets::pinstripe(
                &painter,
                Rect::from_min_max(
                    Pos2::new(from, rect.center().y - 4.5),
                    Pos2::new(to, rect.center().y + 4.5),
                ),
                t.pinstripe,
            );
        }
    };
    band(close_rect.max.x + 10.0, title_rect.min.x);
    band(title_rect.max.x, rect.max.x - 10.0);

    painter.hline(rect.x_range(), rect.max.y, Stroke::new(1.0, t.border_soft));

    resp.clicked()
}

/// What the caller should do after [`buttons`] returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogAction {
    None,
    Cancel,
    Confirm,
}

/// A right-aligned Cancel/Confirm button pair (primary `confirm` renders rightmost — added first to
/// a `right_to_left` layout).
pub fn buttons(ui: &mut Ui, cancel: &str, confirm: &str) -> DialogAction {
    let mut action = DialogAction::None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if widgets::button(ui, confirm, true, true).clicked() {
            action = DialogAction::Confirm;
        }
        ui.add_space(8.0);
        if widgets::button(ui, cancel, false, true).clicked() {
            action = DialogAction::Cancel;
        }
    });
    action
}
