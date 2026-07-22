//! A minimal, vendored copy of the host's own button/label/checkbox painting — mirrors
//! `gascii-density-brush::widgets`' precedent of copying just what one plugin's UI needs rather
//! than threading a full theme/font handle through `PluginHost`.

use egui::{Align2, Color32, Painter, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::theme::{self, Tokens};

const CHECKBOX: f32 = 14.0;

pub(crate) mod size {
    pub(crate) const LABEL: f32 = 13.0;
    pub(crate) const MICRO: f32 = 12.0;
}

pub(crate) fn mono_id(px: f32) -> egui::FontId {
    egui::FontId::new(px, egui::FontFamily::Monospace)
}

fn tokens(ui: &Ui) -> Tokens {
    theme::current(ui.ctx())
}

fn measure(ui: &Ui, text: &str, font: &egui::FontId) -> Vec2 {
    ui.painter().layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER).size()
}

fn border(painter: &Painter, rect: Rect, color: Color32) {
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
}

/// A small bordered text button, at least `min_h` tall (kiosk passes its own touch-target floor;
/// the normal timeline passes a small value that the text+padding size already exceeds). `enabled:
/// false` senses only hover, so `.clicked()` can never fire — mirrors `gascii::ui::widgets::
/// button`'s disabled-state contract.
pub(crate) fn button(ui: &mut Ui, label: &str, enabled: bool, min_h: f32) -> Response {
    let t = tokens(ui);
    let font = mono_id(size::LABEL);
    let text = measure(ui, label, &font);
    let pad = Vec2::new(10.0, 5.0);
    let mut size = text + pad * 2.0;
    size.y = size.y.max(min_h);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let painter = ui.painter().clone();

    let hovered = enabled && resp.hovered();
    let (fill, fg, edge) = if hovered {
        (t.bg_hover, t.fg_text, t.border_strong)
    } else if enabled {
        (Color32::TRANSPARENT, t.fg_text, t.border_soft)
    } else {
        (Color32::TRANSPARENT, t.fg_secondary, t.border_soft)
    };
    painter.rect_filled(rect, 2.0, fill);
    border(&painter, rect, edge);
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
    resp
}

/// A 14px square checkbox; checked inverts and shows a tick.
pub(crate) fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> bool {
    let t = tokens(ui);
    let font = mono_id(size::LABEL);
    let text = measure(ui, label, &font);
    let size = Vec2::new(CHECKBOX + 5.0 + text.x, CHECKBOX.max(text.y));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter().clone();

    let box_rect = Rect::from_min_size(egui::Pos2::new(rect.min.x, rect.center().y - CHECKBOX / 2.0), Vec2::splat(CHECKBOX));
    let fill = if *checked {
        t.bg_inverse
    } else if resp.hovered() {
        t.bg_hover
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(box_rect, 0.0, fill);
    border(&painter, box_rect, t.border_strong);
    if *checked {
        painter.text(box_rect.center(), Align2::CENTER_CENTER, "\u{2713}", mono_id(11.0), t.fg_inverse);
    }
    painter.text(egui::Pos2::new(box_rect.max.x + 5.0, rect.center().y), Align2::LEFT_CENTER, label, font, t.fg_text);
    if resp.clicked() {
        *checked = !*checked;
        return true;
    }
    false
}

/// A section micro-label: mono, uppercase, letter-spaced — matches `gascii::ui::widgets::
/// micro_label`'s visual treatment.
pub(crate) fn micro_label(ui: &mut Ui, text: &str) -> Response {
    let t = tokens(ui);
    let spaced: String = text.chars().flat_map(|c| [c, '\u{2009}']).collect();
    ui.label(egui::RichText::new(spaced).font(mono_id(size::MICRO)).color(t.fg_secondary))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rendering must not panic and a no-input render must leave state untouched.
    #[test]
    fn button_and_checkbox_render_without_panicking_or_mutating_on_a_no_input_render() {
        let ctx = egui::Context::default();
        let mut checked = false;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let resp = button(ui, "Play", true, 0.0);
            assert!(!resp.clicked());
            let changed = checkbox(ui, &mut checked, "Loop");
            assert!(!changed);
            micro_label(ui, "TIMELINE");
        });
        assert!(!checked);
    }

    #[test]
    fn disabled_button_never_reports_clicked() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let resp = button(ui, "Delete", false, 0.0);
            assert!(!resp.clicked());
        });
    }

    /// `min_h` is a floor, not the exact size — a button must never shrink below it even when the
    /// label's own measured size would otherwise be smaller (the touch-target property kiosk's
    /// larger `control_h` relies on).
    #[test]
    fn button_height_never_shrinks_below_min_h() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let resp = button(ui, "Go", true, 48.0);
            assert!(resp.rect.height() >= 48.0);
        });
    }
}
