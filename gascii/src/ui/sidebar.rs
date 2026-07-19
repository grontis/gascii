//! The left sidebar: toolbox, tool options, palette, colours, write toggles.

use eframe::egui::{self, Sense, Stroke, Ui, Vec2};

use super::widgets::{self, Bound};
use super::theme;
use crate::app::{sized_slot, tool_def, Binding, GasciiApp, ToolDef, ToolKind, TOOLS};
use crate::fonts;
use gascii_core::{BrushShape, Buildup, DensityMode, Fixed, MAX_TOOL_SIZE};

/// The panel's default width; it is resizable (see `app.rs`'s `Panel::left` builder), so this is
/// no longer the only width the sidebar's own content math has to hold up at — `swatch_row`'s
/// per-row count is derived from the available width instead of a fixed 6-per-row cap.
pub const DEFAULT_WIDTH: f32 = 216.0;
/// Floor set by the widest fixed row: the shape segment (~185px) plus the panel's 12px margins.
pub const MIN_WIDTH: f32 = 212.0;
pub const MAX_WIDTH: f32 = 320.0;
const TOOL_COLS: usize = 3;
/// Floor on how few swatches a row ever shows, even at `MIN_WIDTH`.
const SWATCH_COLS_MIN: usize = 4;
/// Ceiling on the RECENT row regardless of available width — it only ever holds `RECENT_GLYPHS`.
const SWATCH_COLS_MAX: usize = 6;
const SWATCH_GAP: f32 = 3.0;
/// Height the palette renders above its own scroll area: the page tabs, the RECENT micro-label
/// and swatch row, and their gaps. Reserved when sizing the glyph scroll.
const PALETTE_RESERVED: f32 = 90.0;
const PALETTE_SCROLL_MAX: f32 = 220.0;
const PALETTE_SCROLL_MIN: f32 = 96.0;

/// Height of the colours + WRITE block pinned to the foot of the panel: the rule above it, the
/// well cluster, the inner rule and its gaps, the WRITE micro-label, and the toggle row. Only
/// used to decide how far down to push it, so being a few px out costs nothing but the gap above.
const BOTTOM_BLOCK: f32 = 9.0 + 48.0 + 4.0 + 1.0 + 4.0 + fonts::size::MICRO + 8.0 + 20.0;

/// Short display names for the palette Pages. `Page::name` stays as the domain term — this is
/// display only, and deliberately does not reach into `gascii-core` to rename anything.
fn page_label(page_name: &str) -> &str {
    match page_name {
        "Box Drawing" => "Box",
        "Blocks & Shades" => "Blocks",
        other => other,
    }
}

/// Short display names for the brush ramps, same rule as `page_label`.
pub(crate) fn ramp_label(name: &str) -> &str {
    match name {
        "ASCII shading" => "ASCII",
        "Block shades" => "Blocks",
        other => other,
    }
}

/// The stamp shape segment, shared with kiosk so both chrome modes name the shapes identically.
pub(crate) const SHAPE_OPTIONS: [(BrushShape, &str); 3] = [
    (BrushShape::Raw, "None"),
    (BrushShape::Square, "Square"),
    (BrushShape::Circle, "Circle"),
];

pub fn show(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    let panel_h = ui.available_height();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
        let top = ui.cursor().min.y;

        toolbox(ui, app);
        rule(ui, t.border_soft);
        binding_options(ui, app);
        rule(ui, t.border_soft);

        // The options block's height varies (shape rows, the brush block), so the glyph scroll
        // gives up height first. `available_height` is unbounded inside the scroll area — size
        // against the panel's real height minus what has actually been consumed.
        let remaining = panel_h - (ui.cursor().min.y - top);
        let scroll_h = (remaining - PALETTE_RESERVED - BOTTOM_BLOCK).clamp(PALETTE_SCROLL_MIN, PALETTE_SCROLL_MAX);
        palette(ui, app, widgets::SWATCH, fonts::size::GLYPH, scroll_h);

        // Colours and write toggles sit at the foot of the panel, pushed there with an explicit
        // spacer rather than a `bottom_up` layout: bottom-up mis-measures the nested rows here and
        // draws the rule straight through the colour wells. On panels too short for everything the
        // spacer bottoms out and the whole sidebar scrolls instead of clipping the colour block.
        let gap = (panel_h - (ui.cursor().min.y - top) - BOTTOM_BLOCK).max(8.0);
        ui.add_space(gap);
        rule(ui, t.border_soft);
        ui.add_space(4.0);
        colors(ui, app);
        ui.add_space(4.0);
        rule(ui, t.border_soft);
        ui.add_space(4.0);
        write_toggles(ui, app);
    });
}

/// A full-width 1px separator. `ui.separator()` sizes itself from the surrounding layout and can
/// collapse to a stub, so the line is allocated and painted explicitly.
pub(crate) fn rule(ui: &mut Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, color));
}

/// MacPaint-style grid: `tools` laid out `cols` wide at `cell_h` tall, cells butted together
/// with the grid's own 1px border showing through the gaps, so the whole block reads as one
/// object rather than a row of separate buttons. Click binds L, secondary-click binds R — the
/// only place R is ever set by pointer, in either chrome mode.
pub(crate) fn tool_grid(ui: &mut Ui, app: &mut GasciiApp, tools: &[ToolDef], cols: usize, cell_h: f32) {
    let t = theme::current(ui.ctx());
    let avail = ui.available_width();
    let cell_w = ((avail - (cols - 1) as f32) / cols as f32).floor();
    let cell = Vec2::new(cell_w, cell_h);
    let rows = tools.len().div_ceil(cols);

    let grid_size = Vec2::new(
        cell_w * cols as f32 + (cols - 1) as f32,
        cell.y * rows as f32 + (rows - 1) as f32,
    );
    let (grid_rect, _) = ui.allocate_exact_size(grid_size, Sense::hover());
    // The gap colour: painted once behind the cells, which then leave 1px lines showing.
    ui.painter().rect_filled(grid_rect, 0.0, t.border_strong);

    let mut rebind: Option<(Binding, ToolKind)> = None;
    for (i, def) in tools.iter().enumerate() {
        let (col, row) = (i % cols, i / cols);
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

/// MacPaint-style 3-column grid: cells butt together and the 1px gaps are the grid's own border
/// showing through, so the whole block reads as one object rather than nine buttons.
fn toolbox(ui: &mut Ui, app: &mut GasciiApp) {
    tool_grid(ui, app, &TOOLS, TOOL_COLS, widgets::TOOL_CELL);
}

/// Per-binding tool options: `L <tool> [− n +]` with the shape segment beneath, and — when that
/// binding holds the Brush — the ramp/intensity/pressure block nested right under it, rather than
/// floating below both rows. A rule divides each binding's block from the next, so L's and R's
/// options read as two distinct sections instead of one undifferentiated list. Brush's controls
/// are app-global state shared by both bindings (see `brush_options`'s own doc), so in the rare
/// case both L and R hold Brush the block is shown once, nested under L. Both bindings' rows show
/// at once — there is no focus segment; the `[`/`]` keys follow `options_focus` instead. Unsized
/// tools get a dash where the stepper would be, so the rows always double as an L/R legend.
/// `kiosk::binding_options` is this same block at touch geometry.
fn binding_options(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    widgets::micro_label(ui, "OPTIONS");
    ui.spacing_mut().item_spacing.y = 6.0;
    let mut brush_shown = false;
    for (i, &b) in Binding::ALL.iter().enumerate() {
        let kind = app.slot(b).kind;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(
                egui::RichText::new(if b == Binding::L { "L" } else { "R" })
                    .font(fonts::mono_id(fonts::size::LABEL))
                    .color(t.fg_secondary),
            );
            ui.label(
                egui::RichText::new(tool_def(kind).name)
                    .font(fonts::ui_medium_id(fonts::size::CONTROL))
                    .color(t.fg_text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(slot) = sized_slot(kind) {
                    let mut size = app.slots[b.ix()].stamps[slot].size;
                    if widgets::stepper(ui, &mut size, 1, MAX_TOOL_SIZE, widgets::STEPPER_H) {
                        app.slots[b.ix()].stamps[slot].size = size;
                    }
                } else {
                    ui.label(
                        egui::RichText::new("–")
                            .font(fonts::mono_id(fonts::size::LABEL))
                            .color(t.fg_secondary),
                    );
                }
            });
        });
        if let Some(slot) = sized_slot(kind) {
            widgets::micro_label(ui, "SHAPE");
            let mut shape = app.slots[b.ix()].stamps[slot].shape;
            if widgets::segmented(ui, &mut shape, &SHAPE_OPTIONS, false) {
                app.slots[b.ix()].stamps[slot].shape = shape;
            }
        }
        if kind == ToolKind::Brush && !brush_shown {
            ui.add_space(2.0);
            brush_options(ui, app);
            brush_shown = true;
        }
        if i + 1 < Binding::ALL.len() {
            ui.add_space(2.0);
            rule(ui, t.border_soft);
        }
    }
}

/// Ramp, intensity mode/level and the pressure toggle — app-global state both bindings' brushes
/// share, shown once whichever binding holds the Brush.
fn brush_options(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    widgets::micro_label(ui, "BRUSH");
    let mut ramp = app.active_ramp;
    let names: Vec<(usize, &str)> = app.ramps.iter().enumerate().map(|(i, r)| (i, ramp_label(r.name))).collect();
    if widgets::segmented(ui, &mut ramp, &names, false) {
        app.active_ramp = ramp;
    }
    let mut buildup = matches!(app.density_mode, DensityMode::Buildup(_));
    let modes = [(false, "Fixed"), (true, "Buildup")];
    let changed = widgets::segmented(ui, &mut buildup, &modes, false);
    if buildup {
        if changed {
            app.density_mode = DensityMode::Buildup(Buildup);
        }
    } else {
        let mut level = match app.density_mode {
            DensityMode::Fixed(Fixed(l)) => l,
            DensityMode::Buildup(_) => 1.0,
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let slider = ui.add_sized(
                Vec2::new(100.0, 20.0),
                egui::Slider::new(&mut level, 0.0..=1.0).show_value(false),
            );
            if slider.changed() || changed {
                app.density_mode = DensityMode::Fixed(Fixed(level));
            }
            ui.label(
                egui::RichText::new(format!("{:.0}%", level * 100.0))
                    .font(fonts::mono_id(fonts::size::LABEL))
                    .color(t.fg_secondary),
            );
        });
    }
    // Only shown once a stylus contact has actually been observed this session — no point
    // offering a pressure toggle before there is any pressure signal to drive it.
    if app.stylus_detected {
        widgets::checkbox(ui, &mut app.brush_pressure, "Pressure");
    }
}

/// The page segmented control, the RECENT row, then every page stacked in one scroll area, each
/// under its own section header — replaces the old page-filtered single-page view. The segmented
/// control is jump-to-section: picking a page both highlights it and scrolls its header into view
/// on the next frame (`app.palette_scroll_target`), rather than hiding every other page's glyphs.
pub(crate) fn palette(ui: &mut Ui, app: &mut GasciiApp, swatch: f32, glyph_px: f32, scroll_h: f32) {
    let mut page = app.active_page;
    let options: Vec<(usize, &str)> = app
        .pages
        .iter()
        .enumerate()
        .map(|(i, p)| (i, page_label(p.name)))
        .collect();
    if widgets::segmented(ui, &mut page, &options, false) {
        app.active_page = page;
        app.palette_scroll_target = Some(page);
    }

    if !app.recent_glyphs.is_empty() {
        widgets::micro_label(ui, "RECENT");
        let recent = app.recent_glyphs.clone();
        swatch_row(ui, app, &recent, swatch, glyph_px);
    }

    // Cloned up front: swatch_row takes &mut app (it calls app.pick_glyph), so an immutable
    // borrow of app.pages can't survive the loop below.
    let sections: Vec<(String, Vec<char>)> =
        app.pages.iter().map(|p| (p.name.to_string(), p.glyphs.clone())).collect();
    egui::ScrollArea::vertical()
        .max_height(scroll_h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, (name, glyphs)) in sections.iter().enumerate() {
                let header = widgets::micro_label_response(ui, page_label(name));
                if app.palette_scroll_target == Some(i) {
                    header.scroll_to_me(Some(egui::Align::TOP));
                    app.palette_scroll_target = None;
                }
                swatch_row(ui, app, glyphs, swatch, glyph_px);
            }
        });
}

/// How many swatches fit per row at the given available width and swatch size, floored at
/// `SWATCH_COLS_MIN` and capped at `SWATCH_COLS_MAX` — the sidebar is resizable and kiosk mode
/// uses a larger swatch, so this can no longer be a fixed six.
fn swatch_cols(avail: f32, swatch: f32) -> usize {
    let cols = ((avail + SWATCH_GAP) / (swatch + SWATCH_GAP)).floor() as i32;
    cols.clamp(SWATCH_COLS_MIN as i32, SWATCH_COLS_MAX as i32) as usize
}

/// A wrapped grid of glyph swatches, reflowing to the sidebar's current width.
fn swatch_row(ui: &mut Ui, app: &mut GasciiApp, glyphs: &[char], swatch: f32, glyph_px: f32) {
    let mut picked: Option<char> = None;
    let cols = swatch_cols(ui.available_width(), swatch);
    ui.spacing_mut().item_spacing = Vec2::splat(SWATCH_GAP);
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(swatch * cols as f32 + SWATCH_GAP * (cols - 1) as f32);
        for &ch in glyphs {
            if widgets::glyph_swatch(ui, ch, app.active_glyph == ch, swatch, glyph_px).clicked() {
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

/// Buffer backing `color_picker_body`'s HEX field, kept in egui temp memory keyed by the field's
/// `Id` rather than on `GasciiApp` — `synced` is the colour the buffer was last derived from, so
/// in-progress/invalid typing survives frames without being clobbered by the picker/preset paths
/// (which write `*color` directly), while a change from either of those reformats the buffer.
#[derive(Clone)]
struct HexBuf {
    text: String,
    synced: gascii_core::Rgba,
}

/// ANSI-16 presets, the egui HS-square + hue-bar + RGBA picker, and an editable HEX field. Edits
/// `color` in place. Shared by the normal-mode well popup and the kiosk well popup — deliberately
/// egui's stock HS/hue picker rather than a custom-painted wheel.
pub(crate) fn color_picker_body(ui: &mut Ui, color: &mut gascii_core::Rgba) {
    widgets::micro_label(ui, "ANSI 16");
    ui.horizontal_wrapped(|ui| {
        for (name, preset) in ANSI16.iter() {
            let sw = ui.add(
                egui::Button::new("")
                    .fill(widgets::rgba_to_color32(*preset))
                    .min_size(Vec2::new(18.0, 16.0)),
            );
            if sw.on_hover_text(*name).clicked() {
                *color = *preset;
            }
        }
    });
    ui.separator();
    let mut c32 = egui::Color32::from_rgba_unmultiplied(color.0, color.1, color.2, color.3);
    if egui::color_picker::color_picker_color32(ui, &mut c32, egui::color_picker::Alpha::OnlyBlend) {
        let [r, g, b, a] = c32.to_srgba_unmultiplied();
        *color = gascii_core::Rgba(r, g, b, a);
    }
    ui.separator();
    widgets::micro_label(ui, "HEX");
    let id = ui.id().with("hex_edit");
    let mut buf = ui
        .ctx()
        .data_mut(|d| d.get_temp::<HexBuf>(id))
        .unwrap_or_else(|| HexBuf { text: widgets::hex_string_rgba(*color), synced: *color });
    // The ANSI-16 or picker paths above may have just written `*color` directly — reformat the
    // buffer to match rather than let it go stale, but only when it wasn't this field's own edit.
    if buf.synced != *color {
        buf.text = widgets::hex_string_rgba(*color);
        buf.synced = *color;
    }
    let resp = ui.add(
        egui::TextEdit::singleline(&mut buf.text).desired_width(90.0).font(fonts::mono_id(fonts::size::CONTROL)),
    );
    if resp.changed() {
        if let Some(parsed) = widgets::parse_hex(&buf.text) {
            *color = parsed;
            buf.synced = parsed;
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, buf));
}

/// The picker hung off a colour well. Deliberately egui's stock HS/hue picker rather than a
/// custom-painted one.
fn color_popup(ui: &Ui, resp: &egui::Response, color: &mut gascii_core::Rgba) {
    egui::Popup::from_toggle_button_response(resp).show(|ui| {
        color_picker_body(ui, color);
    });
    let _ = ui;
}

fn colors(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    ui.horizontal(|ui| {
        let wells = widgets::color_wells(
            ui,
            widgets::rgba_to_color32(app.active_fg),
            widgets::rgba_to_color32(app.active_bg),
            widgets::WELL,
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (tag, c) in [("FG", app.active_fg), ("BG", app.active_bg)] {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(egui::RichText::new(tag).font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_secondary));
                    ui.label(egui::RichText::new(widgets::hex_string(c)).font(fonts::ui_medium_id(fonts::size::LABEL)).color(t.fg_text));
                });
            }
        });
        if widgets::swap_button(ui, widgets::SWAP_BUTTON) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `swatch_cols` is the pure math behind the sidebar's resizable-width reflow (`WS6a`'s own
    /// "checked by formula, not rendered" note) — this exercises it directly rather than only by
    /// code inspection, across the panel's actual `size_range` (`MIN_WIDTH..=MAX_WIDTH`) minus
    /// plausible content margins, plus the two clamp boundaries and adversarial (zero/negative)
    /// inputs a future margin-accounting change could still hand it.
    #[test]
    fn swatch_cols_stays_within_min_and_max_across_the_sidebars_real_width_range() {
        // A generous margin allowance either side of the panel's raw 190..=320 range — the actual
        // available content width is narrower than the panel width (padding/margins), but must
        // never be negative in practice.
        for avail in [0.0, 50.0, 121.0 /* just below the min-clamp boundary */, 150.0, 170.0, 200.0, 260.0, 320.0, 1000.0] {
            let cols = swatch_cols(avail, widgets::SWATCH);
            assert!(
                (SWATCH_COLS_MIN..=SWATCH_COLS_MAX).contains(&cols),
                "avail={avail}: cols={cols} outside [{SWATCH_COLS_MIN},{SWATCH_COLS_MAX}]"
            );
        }
    }

    #[test]
    fn swatch_cols_clamps_to_the_minimum_at_a_very_narrow_width() {
        assert_eq!(swatch_cols(0.0, widgets::SWATCH), SWATCH_COLS_MIN);
    }

    #[test]
    fn swatch_cols_clamps_to_the_maximum_at_a_very_wide_width() {
        assert_eq!(swatch_cols(1000.0, widgets::SWATCH), SWATCH_COLS_MAX);
    }

    /// A negative available width (a defensive/adversarial input this pure function has no reason
    /// to assume can't happen, e.g. from a future margin-subtraction bug) must clamp to the
    /// minimum rather than underflowing the `usize` cast or panicking.
    #[test]
    fn swatch_cols_does_not_panic_or_underflow_on_a_negative_width() {
        assert_eq!(swatch_cols(-500.0, widgets::SWATCH), SWATCH_COLS_MIN);
    }

    /// Pins the exact column count at the sidebar's default width (216, `DEFAULT_WIDTH`) minus a
    /// representative content margin, so a future change to `SWATCH`/`SWATCH_GAP`/the panel's
    /// default that silently drops the row below or above 6 columns is caught here, not just by
    /// eyeballing the default layout.
    #[test]
    fn swatch_cols_at_the_default_sidebar_width_fits_six_per_row() {
        // DEFAULT_WIDTH (216) minus a representative panel margin (~16px, matching the app's own
        // sidebar content-frame inset) still comfortably fits the full 6-column cap.
        let content_width = DEFAULT_WIDTH - 16.0;
        assert_eq!(swatch_cols(content_width, widgets::SWATCH), SWATCH_COLS_MAX);
    }

    /// The options block is the sidebar's widest fixed content (the shape segment doesn't reflow
    /// the way swatch rows do), and `MIN_WIDTH` exists to fit it — this renders the worst case
    /// (a sized Brush on L showing shape + brush controls, a sized Line on R) at the minimum
    /// panel's content width and pins that nothing allocates wider.
    #[test]
    fn options_block_fits_the_sidebars_minimum_content_width() {
        let mut app = crate::app::GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Brush);
        app.bind(Binding::R, ToolKind::Line);

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |_ui| {});

        // The panel's 12px inner margin each side (`app.rs`'s sidebar frame).
        let content_w = MIN_WIDTH - 24.0;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let rect = egui::Rect::from_min_size(ui.cursor().min, Vec2::new(content_w, 2000.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            binding_options(&mut child, &mut app);
            assert!(
                child.min_rect().width() <= content_w,
                "options block allocates {:.1}px, wider than the minimum sidebar's {content_w:.1}px content",
                child.min_rect().width()
            );
        });
    }

    /// Pins kiosk's own combination: a fixed 340px sidebar (`kiosk::SIDEBAR_W`) with a 16px inner
    /// margin on each side (`kiosk::sidebar`'s panel frame) and full-scale 48px swatches
    /// (`kiosk::SWATCH`) — a 6-per-row glyph grid at the column cap. A future change to any of
    /// those three numbers that silently drops kiosk below 6 columns is caught here, not just by
    /// eyeballing the layout.
    #[test]
    fn swatch_cols_at_the_kiosk_sidebar_width_fits_six_per_row() {
        const KIOSK_SIDEBAR_W: f32 = 340.0;
        const KIOSK_MARGIN: f32 = 16.0;
        const KIOSK_SWATCH: f32 = 48.0;
        let content_width = KIOSK_SIDEBAR_W - KIOSK_MARGIN * 2.0;
        assert_eq!(swatch_cols(content_width, KIOSK_SWATCH), SWATCH_COLS_MAX);
    }

    /// The palette's stacked-scroll restructure: a pending jump-to-section target must render
    /// without panicking (the target section's header calls `scroll_to_me` mid-layout) and a
    /// render with no pointer input must not mutate `active_page` — the segmented control is the
    /// only thing that changes it.
    #[test]
    fn palette_renders_with_a_pending_scroll_target_without_panicking_or_mutating_active_page() {
        let mut app = crate::app::GasciiApp::headless();
        app.active_page = 0;
        app.palette_scroll_target = Some(1);

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            palette(ui, &mut app, widgets::SWATCH, fonts::size::GLYPH, 200.0);
        });

        assert_eq!(app.active_page, 0, "a render pass with no input must not change the active page");
        // The scroll target is consumed once its section's header is laid out during the render
        // above — it must not still be pending afterward.
        assert_eq!(app.palette_scroll_target, None, "the jump target must be consumed by the render");
    }

    /// Walks every valid section index (the palette's actual page count, not just the single index
    /// the coder's own test picked) — each must be consumed (cleared to `None`) by exactly one
    /// render pass, matching the code review's trace of `ScrollArea`'s single-consumption
    /// `pass_state.scroll_target.take()`.
    #[test]
    fn palette_consumes_a_scroll_target_at_every_valid_section_index() {
        let app_template = crate::app::GasciiApp::headless();
        let page_count = app_template.pages.len();
        assert!(page_count > 0, "sanity: the built-in palette must have at least one page");

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        for target in 0..page_count {
            let mut app = crate::app::GasciiApp::headless();
            app.palette_scroll_target = Some(target);
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                palette(ui, &mut app, widgets::SWATCH, fonts::size::GLYPH, 200.0);
            });
            assert_eq!(
                app.palette_scroll_target, None,
                "target {target} of {page_count} sections must be consumed by the render"
            );
        }
    }

    /// The code review argued an out-of-range `palette_scroll_target` is "structurally impossible"
    /// because the only production writer (the segmented control) always hands it an index drawn
    /// from `app.pages`' own range — true, but the field itself is a bare `Option<usize>` with no
    /// type-level guarantee against a future caller setting it directly. Confirms the defensive
    /// case explicitly: an out-of-range target never matches any section's index in the render
    /// loop, so it is simply never consumed (stays pending rather than panic-indexing anything) —
    /// a silent no-op, not a crash, across two render passes.
    #[test]
    fn palette_does_not_panic_on_an_out_of_range_scroll_target_and_leaves_it_pending() {
        let mut app = crate::app::GasciiApp::headless();
        let out_of_range = app.pages.len() + 50;
        app.palette_scroll_target = Some(out_of_range);

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                palette(ui, &mut app, widgets::SWATCH, fonts::size::GLYPH, 200.0);
            });
        }
        assert_eq!(
            app.palette_scroll_target,
            Some(out_of_range),
            "an out-of-range target matches no section header, so it is left pending rather than \
             silently dropped or, worse, matched against the wrong section"
        );
    }

    /// `color_picker_body` (ANSI-16 presets, the egui HS/hue/RGBA picker, and the HEX field) must
    /// render in both themes without panicking, and a render with no pointer/keyboard input must
    /// leave the edited colour untouched.
    #[test]
    fn color_picker_body_renders_in_both_themes_without_mutating_color_on_a_no_input_render() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let mut color = gascii_core::Rgba(0x12, 0x34, 0x56, 255);
            let ctx = egui::Context::default();
            fonts::install_fonts(&ctx);
            ctx.set_theme(theme);
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                color_picker_body(ui, &mut color);
            });
            assert_eq!(color, gascii_core::Rgba(0x12, 0x34, 0x56, 255), "{theme:?}: no-input render must not change the colour");
        }
    }

    /// Regression guard for the `HexBuf` write-back gating around `hex_string_rgba`: seeding
    /// `color_picker_body` with the app's own default transparent BG (`Rgba::TRANSPARENT`, the
    /// exact value that was silently forced opaque before the alpha-preserving formatter landed)
    /// and rendering with no pointer/keyboard input must leave the colour untouched — the
    /// `resp.changed()` gate must not fire on its own, regardless of what the buffer is seeded
    /// with or formatted through.
    #[test]
    fn color_picker_body_does_not_mutate_a_transparent_colour_on_a_no_input_render() {
        let mut color = gascii_core::Rgba::TRANSPARENT;
        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            color_picker_body(ui, &mut color);
        });
        assert_eq!(color, gascii_core::Rgba::TRANSPARENT, "a no-input render must not force a transparent colour opaque");
    }
}
