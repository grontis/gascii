//! A minimal, vendored copy of the three custom-painted controls `options_ui` needs
//! (`segmented`, `checkbox`, `micro_label`) plus the font-id helpers they call — copied from
//! `gascii::ui::widgets`/`gascii::fonts` line-for-line (only the `super::theme`/`crate::fonts`
//! paths changed) so the brush block renders pixel-identical to its pre-migration self without
//! threading a full theme/font handle through `PluginHost` for one options block. See
//! `crate::theme` for why the tokens themselves are duplicated the same way.

use egui::{Align2, Color32, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::theme::{self, Tokens};

const SEG_PAD: Vec2 = Vec2::new(11.0, 5.0);
const CHECKBOX: f32 = 14.0;

pub(crate) mod size {
    pub(crate) const CONTROL: f32 = 14.0;
    pub(crate) const LABEL: f32 = 13.0;
    pub(crate) const MICRO: f32 = 12.0;
    pub(crate) const CAPTION: f32 = 11.0;
}

pub(crate) fn mono_id(px: f32) -> egui::FontId {
    egui::FontId::new(px, egui::FontFamily::Monospace)
}

pub(crate) fn ui_medium_id(px: f32) -> egui::FontId {
    egui::FontId::new(
        px,
        egui::FontFamily::Name(std::sync::Arc::from("ui-medium")),
    )
}

fn tokens(ui: &Ui) -> Tokens {
    theme::current(ui.ctx())
}

fn measure(ui: &Ui, text: &str, font: &egui::FontId) -> Vec2 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER)
        .size()
}

fn border(painter: &Painter, rect: Rect, color: Color32) {
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
}

/// Fill + border + centered text for one inverted-or-not cell — the shape both `segmented` and
/// `checkbox` build from.
fn cell(painter: &Painter, rect: Rect, t: &Tokens, selected: bool, hovered: bool) -> Color32 {
    let (fill, fg) = if selected {
        (t.bg_inverse, t.fg_inverse)
    } else if hovered {
        (t.bg_hover, t.fg_text)
    } else {
        (Color32::TRANSPARENT, t.fg_text)
    };
    painter.rect_filled(rect, 0.0, fill);
    if !selected && hovered {
        border(painter, rect, t.border_strong);
    }
    fg
}

/// A segmented control: one bordered group, 1px dividers, the selected segment inverted. Returns
/// true if the selection changed.
pub(crate) fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, &str)],
    soft: bool,
) -> bool {
    let t = tokens(ui);
    let edge = if soft { t.border_soft } else { t.border_strong };
    let font = ui_medium_id(size::CONTROL);

    let sizes: Vec<Vec2> = options
        .iter()
        .map(|(_, label)| measure(ui, label, &font))
        .collect();
    let widths: Vec<f32> = sizes.iter().map(|s| s.x + SEG_PAD.x * 2.0).collect();
    let row_h = sizes.iter().map(|s| s.y).fold(0.0, f32::max);
    let total = Vec2::new(widths.iter().sum(), row_h + SEG_PAD.y * 2.0);

    let (rect, group) = ui.allocate_exact_size(total, Sense::hover());
    let painter = ui.painter().clone();
    let mut changed = false;
    let mut x = rect.min.x;

    for (i, ((opt, label), w)) in options.iter().zip(&widths).enumerate() {
        let seg = Rect::from_min_size(Pos2::new(x, rect.min.y), Vec2::new(*w, rect.height()));
        let resp = ui.interact(seg, group.id.with(i), Sense::click());
        let selected = *value == *opt;
        let fg = cell(&painter, seg, &t, selected, resp.hovered());
        painter.text(
            seg.center(),
            Align2::CENTER_CENTER,
            *label,
            font.clone(),
            fg,
        );
        if x > rect.min.x {
            painter.vline(x, seg.y_range(), Stroke::new(1.0, edge));
        }
        if resp.clicked() && !selected {
            *value = *opt;
            changed = true;
        }
        x += w;
    }
    border(&painter, rect, edge);
    changed
}

/// A 14px square checkbox; checked inverts and shows a tick, unchecked-and-hovered gets the hover
/// wash.
pub(crate) fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> bool {
    let t = tokens(ui);
    let font = ui_medium_id(size::CONTROL);
    let text = measure(ui, label, &font);
    let size = Vec2::new(CHECKBOX + 5.0 + text.x, CHECKBOX.max(text.y));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter().clone();

    let box_rect = Rect::from_min_size(
        Pos2::new(rect.min.x, rect.center().y - CHECKBOX / 2.0),
        Vec2::splat(CHECKBOX),
    );
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
        painter.text(
            box_rect.center(),
            Align2::CENTER_CENTER,
            "\u{2713}",
            mono_id(size::CAPTION),
            t.fg_inverse,
        );
    }
    painter.text(
        Pos2::new(box_rect.max.x + 5.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        t.fg_text,
    );
    if resp.clicked() {
        *checked = !*checked;
        return true;
    }
    false
}

/// A section micro-label: mono, uppercase, letter-spaced — matches `gascii::ui::widgets::
/// micro_label`'s exact visual treatment.
pub(crate) fn micro_label(ui: &mut Ui, text: &str) -> Response {
    let t = tokens(ui);
    let spaced: String = text.chars().flat_map(|c| [c, '\u{2009}']).collect();
    ui.label(
        egui::RichText::new(spaced)
            .font(mono_id(size::MICRO))
            .color(t.fg_secondary),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host normally registers the "ui-medium" named family at startup
    /// (`gascii::fonts::install_fonts`); this crate has no dependency on `gascii` to reuse that
    /// call, so tests exercising `ui_medium_id` map the name onto egui's own default proportional
    /// chain instead — enough to render without panicking, not a claim about the real typeface.
    fn install_test_fonts(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        let default_chain = fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        fonts.families.insert(
            egui::FontFamily::Name(std::sync::Arc::from("ui-medium")),
            default_chain,
        );
        ctx.set_fonts(fonts);
        let _ = ctx.run_ui(egui::RawInput::default(), |_ui| {});
    }

    /// Rendering must not panic and must leave an untouched value alone with no interaction —
    /// mirrors the "a render pass with no input must not mutate state" convention every other
    /// widget test in this project follows.
    #[test]
    fn segmented_and_checkbox_render_without_panicking_or_mutating_on_a_no_input_render() {
        let ctx = egui::Context::default();
        install_test_fonts(&ctx);
        let mut value = false;
        let mut checked = true;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let changed = segmented(
                ui,
                &mut value,
                &[(false, "Fixed"), (true, "Buildup")],
                false,
            );
            assert!(!changed);
            let changed = checkbox(ui, &mut checked, "Pressure");
            assert!(!changed);
            micro_label(ui, "BRUSH");
        });
        assert!(!value);
        assert!(checked);
    }
}
