//! The 208px left sidebar: toolbox, palette, colours, write toggles.

use eframe::egui::{self, Sense, Stroke, Ui, Vec2};

use super::widgets::{self, Bound};
use super::theme;
use crate::app::{Binding, GasciiApp, ToolKind, TOOLS};
use crate::fonts;

/// The panel is fixed at this and not resizable; its 12px padding is set by the `Frame` at the
/// call site, so the content width here is 184.
pub const WIDTH: f32 = 208.0;
const TOOL_COLS: usize = 3;
const SWATCH_COLS: usize = 6;
const SWATCH_GAP: f32 = 3.0;

/// Height of the colours + WRITE block pinned to the foot of the panel: the 44px well cluster, the
/// rule and its gaps, the WRITE micro-label, and the toggle row. Only used to decide how far down
/// to push it, so being a few px out costs nothing but the gap above it.
const BOTTOM_BLOCK: f32 = 44.0 + 4.0 + 1.0 + 4.0 + 14.0 + 8.0 + 18.0;

/// Short display names for the palette Pages. `Page::name` stays as the domain term — this is
/// display only, and deliberately does not reach into `gascii-core` to rename anything.
fn page_label(page_name: &str) -> &str {
    match page_name {
        "Box Drawing" => "Box",
        "Blocks & Shades" => "Blocks",
        other => other,
    }
}

fn rgba_to_color32(c: gascii_core::Rgba) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

fn hex(c: gascii_core::Rgba) -> String {
    format!("#{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

pub fn show(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);

    toolbox(ui, app);
    ui.add_space(2.0);
    palette(ui, app);

    // Colours and write toggles sit at the foot of the panel, pushed there with an explicit
    // spacer rather than a `bottom_up` layout: bottom-up mis-measures the nested rows here and
    // draws the rule straight through the colour wells.
    let gap = (ui.available_height() - BOTTOM_BLOCK).max(8.0);
    ui.add_space(gap);
    colors(ui, app);
    ui.add_space(4.0);
    rule(ui, t.border_soft);
    ui.add_space(4.0);
    write_toggles(ui, app);
}

/// A full-width 1px separator. `ui.separator()` sizes itself from the surrounding layout and can
/// collapse to a stub, so the line is allocated and painted explicitly.
fn rule(ui: &mut Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, color));
}

/// MacPaint-style 3-column grid: cells butt together and the 1px gaps are the grid's own border
/// showing through, so the whole block reads as one object rather than nine buttons.
fn toolbox(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    let avail = ui.available_width();
    let cell_w = ((avail - (TOOL_COLS - 1) as f32) / TOOL_COLS as f32).floor();
    let cell = Vec2::new(cell_w, widgets::TOOL_CELL);
    let rows = TOOLS.len().div_ceil(TOOL_COLS);

    let grid_size = Vec2::new(
        cell_w * TOOL_COLS as f32 + (TOOL_COLS - 1) as f32,
        cell.y * rows as f32 + (rows - 1) as f32,
    );
    let (grid_rect, _) = ui.allocate_exact_size(grid_size, Sense::hover());
    // The gap colour: painted once behind the cells, which then leave 1px lines showing.
    ui.painter().rect_filled(grid_rect, 0.0, t.border_strong);

    let mut rebind: Option<(Binding, ToolKind)> = None;
    for (i, def) in TOOLS.iter().enumerate() {
        let (col, row) = (i % TOOL_COLS, i / TOOL_COLS);
        let min = grid_rect.min
            + Vec2::new(col as f32 * (cell_w + 1.0), row as f32 * (cell.y + 1.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_size(min, cell))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let bound = Bound {
            l: app.slot(Binding::L).kind == def.kind,
            r: app.slot(Binding::R).kind == def.kind,
        };
        let resp = widgets::tool_cell(&mut child, def.kind, bound, cell)
            .on_hover_text(format!("{} ({})  —  {}", def.name, def.key.name(), def.tip));
        // Click binds L, right-click binds R — the only place R is set by pointer.
        if resp.clicked() {
            rebind = Some((Binding::L, def.kind));
        } else if resp.secondary_clicked() {
            rebind = Some((Binding::R, def.kind));
        }
    }
    ui.painter().rect_stroke(
        grid_rect,
        0.0,
        Stroke::new(1.0, t.border_strong),
        egui::StrokeKind::Inside,
    );
    if let Some((b, kind)) = rebind {
        app.bind(b, kind);
    }
}

fn palette(ui: &mut Ui, app: &mut GasciiApp) {
    let mut page = app.active_page;
    let options: Vec<(usize, &str)> = app
        .pages
        .iter()
        .enumerate()
        .map(|(i, p)| (i, page_label(p.name)))
        .collect();
    if widgets::segmented(ui, &mut page, &options, false) {
        app.active_page = page;
    }

    if !app.recent_glyphs.is_empty() {
        widgets::micro_label(ui, "RECENT");
        let recent = app.recent_glyphs.clone();
        swatch_row(ui, app, &recent);
    }

    // The ASCII page is 95 glyphs and Box Drawing is 128 — 16 and 22 rows at six per row, far
    // more than one screenful, so this has to scroll.
    let glyphs = app.pages[app.active_page].glyphs.clone();
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            swatch_row(ui, app, &glyphs);
        });
}

/// A wrapped grid of glyph swatches, six per row.
fn swatch_row(ui: &mut Ui, app: &mut GasciiApp, glyphs: &[char]) {
    let mut picked: Option<char> = None;
    ui.spacing_mut().item_spacing = Vec2::splat(SWATCH_GAP);
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(widgets::SWATCH * SWATCH_COLS as f32 + SWATCH_GAP * (SWATCH_COLS - 1) as f32);
        for &ch in glyphs {
            if widgets::glyph_swatch(ui, ch, app.active_glyph == ch).clicked() {
                picked = Some(ch);
            }
        }
    });
    if let Some(ch) = picked {
        app.pick_glyph(ch);
    }
}

/// ANSI 16-color presets offered as a picking aid alongside the truecolor picker. Colors are always
/// stored truecolor — the presets are a convenience, not a constraint.
const ANSI16: [(&str, gascii_core::Rgba); 16] = [
    ("Black", gascii_core::Rgba(0, 0, 0, 255)),
    ("Red", gascii_core::Rgba(205, 49, 49, 255)),
    ("Green", gascii_core::Rgba(13, 188, 121, 255)),
    ("Yellow", gascii_core::Rgba(229, 229, 16, 255)),
    ("Blue", gascii_core::Rgba(36, 114, 200, 255)),
    ("Magenta", gascii_core::Rgba(188, 63, 188, 255)),
    ("Cyan", gascii_core::Rgba(17, 168, 205, 255)),
    ("White", gascii_core::Rgba(229, 229, 229, 255)),
    ("Bright Black", gascii_core::Rgba(102, 102, 102, 255)),
    ("Bright Red", gascii_core::Rgba(241, 76, 76, 255)),
    ("Bright Green", gascii_core::Rgba(35, 209, 139, 255)),
    ("Bright Yellow", gascii_core::Rgba(245, 245, 67, 255)),
    ("Bright Blue", gascii_core::Rgba(59, 142, 234, 255)),
    ("Bright Magenta", gascii_core::Rgba(214, 112, 214, 255)),
    ("Bright Cyan", gascii_core::Rgba(41, 184, 219, 255)),
    ("Bright White", gascii_core::Rgba(255, 255, 255, 255)),
];

/// The picker hung off a colour well. Deliberately egui's stock truecolor widget rather than a
/// custom-painted one.
fn color_popup(ui: &Ui, resp: &egui::Response, color: &mut gascii_core::Rgba) {
    egui::Popup::from_toggle_button_response(resp).show(|ui| {
        widgets::micro_label(ui, "ANSI 16");
        ui.horizontal_wrapped(|ui| {
            for (name, preset) in ANSI16.iter() {
                let sw = ui.add(
                    egui::Button::new("")
                        .fill(rgba_to_color32(*preset))
                        .min_size(Vec2::new(18.0, 16.0)),
                );
                if sw.on_hover_text(*name).clicked() {
                    *color = *preset;
                }
            }
        });
        ui.separator();
        widgets::micro_label(ui, "CUSTOM");
        let mut arr = [color.0, color.1, color.2, color.3];
        if ui.color_edit_button_srgba_unmultiplied(&mut arr).changed() {
            *color = gascii_core::Rgba(arr[0], arr[1], arr[2], arr[3]);
        }
    });
    let _ = ui;
}

fn colors(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    ui.horizontal(|ui| {
        let wells = widgets::color_wells(
            ui,
            rgba_to_color32(app.active_fg),
            rgba_to_color32(app.active_bg),
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (tag, c) in [("FG", app.active_fg), ("BG", app.active_bg)] {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(egui::RichText::new(tag).font(fonts::mono_id(11.0)).color(t.fg_secondary));
                    ui.label(egui::RichText::new(hex(c)).font(fonts::ui_medium_id(11.0)).color(t.fg_text));
                });
            }
        });
        if widgets::swap_button(ui) {
            app.swap_colors();
        }
        color_popup(ui, &wells.fg, &mut app.active_fg);
        color_popup(ui, &wells.bg, &mut app.active_bg);
    });
}

fn write_toggles(ui: &mut Ui, app: &mut GasciiApp) {
    widgets::micro_label(ui, "WRITE");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        widgets::checkbox(ui, &mut app.mask.glyph, "Glyph");
        widgets::checkbox(ui, &mut app.mask.bg, "Background");
    });
}
