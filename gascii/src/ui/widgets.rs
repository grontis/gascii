//! The custom-painted control kit.
//!
//! The design spec calls for these to be allocated and painted by hand rather than styled from
//! egui's stock widgets, and the reason is [`theme`](super::theme)'s core rule: selection is
//! inversion. egui expresses a selected control through `Visuals`, which cannot express "swap fg and
//! bg, keep the 1px border, change nothing else" without fighting every default it ships with.
//!
//! Each widget reads [`Tokens`] from the context itself, so call sites never thread a palette
//! through. Sizes come from the spec's §4/§5 and live in the consts below.

use eframe::egui::{
    self, Align2, Color32, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use super::theme::{self, Tokens};
use crate::app::ToolKind;
use crate::fonts;

/// Toolbox cell, per spec §4. Icons are 17px inside it.
pub const TOOL_CELL: f32 = 42.0;
const ICON: f32 = 17.0;
/// Palette glyph swatch, per spec §4.
pub const SWATCH: f32 = 26.0;
/// FG/BG colour wells, per spec §5.
const WELL: f32 = 24.0;
/// How far the FG well overlaps the BG well, per spec §5.
const WELL_OVERLAP: f32 = 4.0;
const CHECKBOX: f32 = 12.0;
const STEPPER_H: f32 = 24.0;
const STEPPER_BTN_W: f32 = 22.0;
const STEPPER_VALUE_W: f32 = 30.0;
/// Segmented control padding, per spec §5 (4–6px vertical, 10–12px horizontal).
const SEG_PAD: Vec2 = Vec2::new(11.0, 5.0);
/// The mono `L`/`R` corner badges — the one place the spec allows text below 10px.
const BADGE_PX: f32 = 8.0;

fn tokens(ui: &Ui) -> Tokens {
    theme::current(ui.ctx())
}

/// Laid-out size of `text`. Goes through the painter rather than `Ui::fonts`, which hands out a
/// shared `&Fonts` while the measurement methods want `&mut`.
fn measure(ui: &Ui, text: &str, font: &egui::FontId) -> Vec2 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER)
        .size()
}

/// A 1px border, always inside the rect so adjacent controls butt together without doubling.
fn border(painter: &Painter, rect: Rect, color: Color32) {
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
}

/// Hard offset shadow — no blur, per the design's one depth rule.
fn hard_shadow(painter: &Painter, rect: Rect, offset: f32, color: Color32) {
    painter.rect_filled(rect.translate(Vec2::splat(offset)), 0.0, color);
}

/// Fill + border + centered text for one inverted-or-not cell. The shape every control here is
/// built from, and the single place the inversion rule is expressed.
fn cell(painter: &Painter, rect: Rect, t: &Tokens, selected: bool, hovered: bool) -> Color32 {
    let (fill, fg) = if selected {
        (t.bg_inverse, t.fg_inverse)
    } else {
        (Color32::TRANSPARENT, t.fg_text)
    };
    painter.rect_filled(rect, 0.0, fill);
    if !selected && hovered {
        // Hover darkens the border rather than filling — the spec allows no hover fills.
        border(painter, rect, t.border_strong);
    }
    fg
}

/// A segmented control: one bordered group, 1px dividers, the selected segment inverted.
///
/// Returns true if the selection changed. Used for the L/R binding, brush shape, palette Pages,
/// presets, export format and the zoom cluster — `soft` picks the quieter border the status bar's
/// cluster uses.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, &str)],
    soft: bool,
) -> bool {
    let t = tokens(ui);
    let edge = if soft { t.border_soft } else { t.border_strong };
    let font = fonts::ui_medium_id(12.0);

    let sizes: Vec<Vec2> = options.iter().map(|(_, label)| measure(ui, label, &font)).collect();
    let widths: Vec<f32> = sizes.iter().map(|s| s.x + SEG_PAD.x * 2.0).collect();
    let row_h = sizes.iter().map(|s| s.y).fold(0.0, f32::max);
    let total = Vec2::new(widths.iter().sum(), row_h + SEG_PAD.y * 2.0);

    let (rect, group) = ui.allocate_exact_size(total, Sense::hover());
    let painter = ui.painter().clone();
    let mut changed = false;
    let mut x = rect.min.x;

    for (i, ((opt, label), w)) in options.iter().zip(&widths).enumerate() {
        let seg = Rect::from_min_size(Pos2::new(x, rect.min.y), Vec2::new(*w, rect.height()));
        // Salted off the group's own allocation id + index — never off screen position, which
        // changes whenever a neighbouring label grows (a tool rebind widens the L/R segment) and
        // makes egui see a brand-new widget mid-interaction, dropping its press state.
        let resp = ui.interact(seg, group.id.with(i), Sense::click());
        let selected = *value == *opt;
        let fg = cell(&painter, seg, &t, selected, resp.hovered());
        painter.text(seg.center(), Align2::CENTER_CENTER, *label, font.clone(), fg);
        if x > rect.min.x {
            // 1px divider between segments, drawn on the group's own border colour.
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

/// A `[−][value][+]` stepper. The value cell is mono and scroll-wheel-adjustable, per spec §5.
pub fn stepper(ui: &mut Ui, value: &mut u16, min: u16, max: u16) -> bool {
    let t = tokens(ui);
    let size = Vec2::new(STEPPER_BTN_W * 2.0 + STEPPER_VALUE_W, STEPPER_H);
    let (rect, group) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter().clone();
    let before = *value;

    let minus = Rect::from_min_size(rect.min, Vec2::new(STEPPER_BTN_W, STEPPER_H));
    let val = Rect::from_min_size(
        Pos2::new(minus.max.x, rect.min.y),
        Vec2::new(STEPPER_VALUE_W, STEPPER_H),
    );
    let plus = Rect::from_min_size(Pos2::new(val.max.x, rect.min.y), Vec2::new(STEPPER_BTN_W, STEPPER_H));

    for (r, label, delta) in [(minus, "–", -1i32), (plus, "+", 1)] {
        let resp = ui.interact(r, group.id.with(label), Sense::click());
        let fg = cell(&painter, r, &t, false, resp.hovered());
        painter.text(r.center(), Align2::CENTER_CENTER, label, fonts::ui_medium_id(12.0), fg);
        if resp.clicked() {
            *value = (*value as i32 + delta).clamp(min as i32, max as i32) as u16;
        }
    }

    let val_resp = ui.interact(val, group.id.with("val"), Sense::hover());
    painter.text(
        val.center(),
        Align2::CENTER_CENTER,
        value.to_string(),
        fonts::mono_id(12.0),
        t.fg_text,
    );
    if val_resp.hovered() {
        // Raw `MouseWheel` events, NOT `smooth_scroll_delta`: the smoothed value eases one wheel
        // notch across several frames, and stepping on each nonzero frame turns one notch into
        // 5–10 steps. A notch is one event, so summing the frame's events and taking the sign
        // gives exactly one step per notch (a trackpad's stream of small deltas still reads as
        // smooth stepping — one step per frame while the fingers move).
        let scroll: f32 = ui.input(|i| {
            i.events
                .iter()
                .map(|e| match e {
                    egui::Event::MouseWheel { delta, .. } => delta.y,
                    _ => 0.0,
                })
                .sum()
        });
        if scroll != 0.0 {
            *value = (*value as i32 + scroll.signum() as i32).clamp(min as i32, max as i32) as u16;
        }
    }

    // Inner dividers soft, outer border strong — the value cell should read as recessed, not as
    // three separate buttons.
    painter.vline(val.min.x, rect.y_range(), Stroke::new(1.0, t.border_soft));
    painter.vline(val.max.x, rect.y_range(), Stroke::new(1.0, t.border_soft));
    border(&painter, rect, t.border_strong);
    *value != before
}

/// A 12px square checkbox; checked inverts and shows a tick, per spec §5.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> bool {
    let t = tokens(ui);
    let font = fonts::ui_medium_id(12.0);
    let text = measure(ui, label, &font);
    let size = Vec2::new(CHECKBOX + 5.0 + text.x, CHECKBOX.max(text.y));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter().clone();

    let box_rect = Rect::from_min_size(
        Pos2::new(rect.min.x, rect.center().y - CHECKBOX / 2.0),
        Vec2::splat(CHECKBOX),
    );
    painter.rect_filled(box_rect, 0.0, if *checked { t.bg_inverse } else { Color32::TRANSPARENT });
    // Always a strong border, hovered or not — at 12px there is no room for a hover treatment that
    // reads as anything but noise.
    border(&painter, box_rect, t.border_strong);
    if *checked {
        painter.text(box_rect.center(), Align2::CENTER_CENTER, "✓", fonts::mono_id(9.0), t.fg_inverse);
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

/// Which bindings a toolbox cell currently holds. Both can be true — the same tool may be bound to
/// both buttons.
#[derive(Clone, Copy, Default)]
pub struct Bound {
    pub l: bool,
    pub r: bool,
}

/// A toolbox cell: icon, inversion when it holds L, and mono corner badges for each binding.
///
/// `size` is passed in because the mockup's grid is `repeat(3, 1fr)` — cells stretch to fill the
/// sidebar's content width and are only [`TOOL_CELL`] tall, not square.
///
/// The cell inverts for L only. R is shown as a badge alone, because inversion is what marks "the
/// tool you are drawing with", and every keyboard shortcut and every glyph action targets L.
pub fn tool_cell(ui: &mut Ui, kind: ToolKind, bound: Bound, size: Vec2) -> Response {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter().clone();

    let fg = if bound.l {
        painter.rect_filled(rect, 0.0, t.bg_inverse);
        t.fg_inverse
    } else {
        // Idle sits on the panel; hover gets the soft fill the spec specifies for this control
        // (the one place a hover fill is allowed, since a border would fight the grid's own lines).
        painter.rect_filled(rect, 0.0, if resp.hovered() { t.border_soft } else { t.bg_panel });
        t.fg_text
    };

    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(ICON));
    super::icons::paint(&painter, kind, icon_rect, fg);

    let badge = |text: &str, color: Color32| {
        painter.text(
            Pos2::new(rect.max.x - 3.0, rect.min.y + 2.0),
            Align2::RIGHT_TOP,
            text,
            fonts::mono_id(BADGE_PX),
            color,
        );
    };
    match (bound.l, bound.r) {
        (true, true) => badge("LR", fg),
        (true, false) => badge("L", fg),
        (false, true) => badge("R", t.fg_secondary),
        (false, false) => {}
    }
    resp
}

/// A 26px palette glyph swatch. Idle has a soft border, hover a strong one, selected inverts.
pub fn glyph_swatch(ui: &mut Ui, ch: char, selected: bool) -> Response {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(SWATCH), Sense::click());
    let painter = ui.painter().clone();

    painter.rect_filled(rect, 0.0, if selected { t.bg_inverse } else { Color32::TRANSPARENT });
    let edge = if selected {
        t.bg_inverse
    } else if resp.hovered() {
        t.border_strong
    } else {
        t.border_soft
    };
    border(&painter, rect, edge);
    // The canvas font, not the UI mono: a swatch is a preview of the glyph as it will land on the
    // canvas, so it must be the same face the canvas uses.
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        ch,
        fonts::canvas_font_id(13.0),
        if selected { t.fg_inverse } else { t.fg_text },
    );
    resp
}

/// The two wells' click targets, for the caller to hang a colour picker off.
pub struct WellsResponse {
    pub fg: Response,
    pub bg: Response,
}

/// Overlapping FG/BG wells, per spec §5 — the paint-app convention. FG sits in front, top-left.
pub fn color_wells(ui: &mut Ui, fg: Color32, bg: Color32) -> WellsResponse {
    let t = tokens(ui);
    let span = WELL * 2.0 - WELL_OVERLAP;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(span, span), Sense::hover());
    let painter = ui.painter().clone();

    let bg_rect = Rect::from_min_size(rect.min + Vec2::splat(WELL - WELL_OVERLAP), Vec2::splat(WELL));
    let fg_rect = Rect::from_min_size(rect.min, Vec2::splat(WELL));

    // FG is interacted with FIRST so it wins the overlapped corner — egui gives the click to the
    // last-registered widget at a position, and BG must not steal clicks from the well on top of it.
    let fg_resp = ui.interact(fg_rect, ui.id().with("well_fg"), Sense::click());
    let bg_resp = ui.interact(bg_rect, ui.id().with("well_bg"), Sense::click());

    painter.rect_filled(bg_rect, 0.0, bg);
    border(&painter, bg_rect, t.border_strong);
    hard_shadow(&painter, fg_rect, 2.0, t.shadow);
    painter.rect_filled(fg_rect, 0.0, fg);
    border(&painter, fg_rect, t.border_strong);

    WellsResponse { fg: fg_resp, bg: bg_resp }
}

/// The ⇄ swap control that sits beside the wells. Separate from [`color_wells`] so callers can place
/// it per the mockup (pushed to the row's trailing edge).
pub fn swap_button(ui: &mut Ui) -> bool {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    let painter = ui.painter().clone();
    let fg = cell(&painter, rect, &t, false, resp.hovered());
    if !resp.hovered() {
        border(&painter, rect, t.border_soft);
    }
    // `⇄` exists in neither Instrument Sans nor Fragment Mono; the Iosevka backstop on the tail of
    // the chrome font chains is what resolves it.
    painter.text(rect.center(), Align2::CENTER_CENTER, "⇄", fonts::mono_id(12.0), fg);
    resp.on_hover_text("Swap FG/BG (X)").clicked()
}

/// A button: 1px border, transparent fill. `primary` inverts and gains a 2px hard offset shadow.
///
/// No callers yet — dialogs are the consumers, and none is custom-painted so far. Part of the
/// control kit regardless: the shape is settled and belongs with its siblings.
#[allow(dead_code)]
pub fn button(ui: &mut Ui, label: &str, primary: bool) -> Response {
    let t = tokens(ui);
    let font = fonts::ui_medium_id(12.0);
    let text = measure(ui, label, &font);
    // Spec §5: 6px vertical, 16px horizontal padding.
    let size = Vec2::new(text.x + 32.0, text.y + 12.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter().clone();

    if primary {
        hard_shadow(&painter, rect, 2.0, t.shadow);
    }
    let fg = cell(&painter, rect, &t, primary, resp.hovered());
    border(&painter, rect, t.border_strong);
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
    resp
}

/// A compact mono button for the status bar's zoom cluster: no fill, `soft` picks the quieter
/// border the cluster uses so it sits back against the panel.
pub fn mini_button(ui: &mut Ui, label: &str, soft: bool) -> bool {
    let t = tokens(ui);
    let font = fonts::mono_id(11.0);
    let text = measure(ui, label, &font);
    let size = Vec2::new(text.x + 16.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter().clone();
    let fg = cell(&painter, rect, &t, false, resp.hovered());
    if !resp.hovered() {
        border(&painter, rect, if soft { t.border_soft } else { t.border_strong });
    }
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
    resp.clicked()
}

/// Title-bar pinstripes: 1px lines every 3px. Decorative only.
pub fn pinstripe(painter: &Painter, rect: Rect, color: Color32) {
    let mut y = rect.min.y;
    while y < rect.max.y {
        painter.hline(rect.x_range(), y, Stroke::new(1.0, color));
        y += 3.0;
    }
}

/// A section micro-label: mono 10px, uppercase, letter-spaced — `RECENT`, `WRITE`.
pub fn micro_label(ui: &mut Ui, text: &str) {
    let t = tokens(ui);
    // egui has no letter-spacing, so the +0.08em the spec asks for is faked by spacing the chars.
    let spaced: String = text.chars().flat_map(|c| [c, '\u{2009}']).collect();
    ui.label(
        egui::RichText::new(spaced).font(fonts::mono_id(10.0)).color(t.fg_secondary),
    );
}
