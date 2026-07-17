//! Full Screen Mode's chrome: touch/stylus-first, replaces the titlebar/menubar/options bar/
//! normal sidebar/normal status bar entirely while active. Built on the same painting primitives
//! normal chrome uses (`widgets`, `sidebar::tool_grid`/`palette`) at larger geometry — the only
//! genuinely new painter is `widgets::color_swatch` (the quick-color row has no normal-mode
//! equivalent).

use eframe::egui::{self, Align2, Pos2, Rect, Ui, UiBuilder, Vec2};

use super::sidebar::{palette, tool_grid};
use super::{theme, widgets};
use crate::app::{sized_slot, tool_def, Binding, GasciiApp, ToolKind, TOOLS};
use crate::fonts;
use gascii_core::{BrushShape, Buildup, DensityMode, Fixed, MAX_TOOL_SIZE};

pub const TOP_H: f32 = 44.0;
pub const SIDEBAR_W: f32 = 300.0;
pub const STATUS_H: f32 = 36.0;
const TOOL_COLS: usize = 2;
const TOOL_CELL_H: f32 = 74.0;
const SWATCH: f32 = 48.0;
const GLYPH_PX: f32 = 26.0;
const PALETTE_SCROLL_MAX: f32 = 300.0;
const PALETTE_SCROLL_MIN: f32 = 120.0;
/// Height the palette's own tabs/RECENT rows plus the colour block need below the options
/// section — the glyph scroll area gets whatever is left, clamped.
const PALETTE_RESERVED: f32 = 250.0;
const SIZE_STEPPER_H: f32 = 36.0;
const WELL: f32 = 36.0;
const SWAP_BUTTON: f32 = 36.0;
const QUICK_COLOR_H: f32 = 32.0;

/// The curated quick-color row: a fixed swatch set rather than the full palette, for a fast-tap
/// touch surface.
const QUICK_COLORS: [gascii_core::Rgba; 8] = [
    gascii_core::Rgba(0xE8, 0xE6, 0xE2, 255),
    gascii_core::Rgba(0x00, 0x00, 0x00, 255),
    gascii_core::Rgba(0xC9, 0x4F, 0x3D, 255),
    gascii_core::Rgba(0xD9, 0xA0, 0x3E, 255),
    gascii_core::Rgba(0x8F, 0xAE, 0x5C, 255),
    gascii_core::Rgba(0x4E, 0x8F, 0xA8, 255),
    gascii_core::Rgba(0x7F, 0xA8, 0xD9, 255),
    gascii_core::Rgba(0x9A, 0x6F, 0xA8, 255),
];

/// The top bar: a title, then Undo/Redo/Clear/Exit laid out right-to-left from the trailing edge.
/// Widths are measured and rects assigned before any button is painted or interacted with, and the
/// clicked action (if any) is collected into a local `Action` and applied only after the loop —
/// painting into a child `Ui` per button while also holding `app: &mut GasciiApp` for the whole
/// loop would be a double-mutable-borrow, so the loop itself never touches `app`.
pub fn top_bar(ui: &mut Ui, app: &mut GasciiApp, ctx: &egui::Context) {
    let t = theme::current(ctx);
    let bar = ui.max_rect();
    let painter = ui.painter().clone();

    let font = fonts::ui_semibold_id(fonts::size::BODY);
    let title = app.window_title();
    let title_w = painter.layout_no_wrap(title.clone(), font.clone(), t.fg_text).size().x;
    painter.text(Pos2::new(bar.min.x + 16.0, bar.center().y), Align2::LEFT_CENTER, &title, font, t.fg_text);

    enum Action {
        None,
        Undo,
        Redo,
        Clear,
        Exit,
    }
    let no_stroke = !app.stroke_in_progress();
    // Laid right-to-left from the trailing edge: Exit sits at the far right, Undo/Redo read
    // left-to-right beside it, and Clear Drawing stands apart with a wider gap so a destructive
    // tap is never one slip away from the history pair.
    let buttons = [
        ("Exit Full Screen (Esc)", true, true, Action::Exit, 0.0),
        ("Redo", false, app.history.can_redo() && no_stroke, Action::Redo, 0.0),
        ("Undo", false, app.history.can_undo() && no_stroke, Action::Undo, 0.0),
        ("Clear Drawing", false, true, Action::Clear, 22.0),
    ];
    let mut action = Action::None;
    let mut x = bar.max.x - 16.0;
    let mut rightmost_used = x;
    for (label, primary, enabled, act, lead_gap) in buttons {
        x -= lead_gap;
        let w = widgets::button_size(ui, label).x;
        x -= w;
        let rect = Rect::from_min_size(Pos2::new(x, bar.min.y + 4.0), Vec2::new(w, bar.height() - 8.0));
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
        if widgets::button(&mut child, label, primary, enabled).clicked() {
            action = act;
        }
        x -= 10.0;
        rightmost_used = rightmost_used.min(x);
    }
    match action {
        Action::Undo => app.request_undo(),
        Action::Redo => app.request_redo(),
        Action::Clear => app.clear_document(),
        Action::Exit => app.toggle_fullscreen(ctx),
        Action::None => {}
    }

    let gap_min = bar.min.x + 16.0 + title_w + 14.0;
    let gap_max = rightmost_used + 10.0 - 14.0; // undo the loop's trailing 10px gap
    if gap_max > gap_min {
        widgets::pinstripe(
            &painter,
            Rect::from_min_max(Pos2::new(gap_min, bar.center().y - 4.5), Pos2::new(gap_max, bar.center().y + 4.5)),
            t.pinstripe,
        );
    }
}

/// The sidebar's tool grid: every `TOOLS` entry except Text. Kiosk has no keyboard-driven session
/// UI, so a binding parked on Text here would have no cell to highlight — see
/// `crate::app::tool_shortcut_reachable`, which keeps the `T` shortcut from reaching Text while
/// fullscreen for the same reason. A binding already on Text when fullscreen is entered is left
/// alone by both; `tool_grid`'s equality-based highlight simply shows no cell selected in that
/// case, never panics or paints a phantom badge (pinned by `kiosk_sidebars_tool_list_excludes_text`
/// and `tool_grid_renders_without_panicking_when_a_binding_holds_a_tool_absent_from_its_list`).
fn kiosk_tools() -> Vec<crate::app::ToolDef> {
    TOOLS.iter().copied().filter(|d| d.kind != ToolKind::Text).collect()
}

/// The sidebar: an 8-tool grid (Text excluded), both bindings' tool options, the glyph palette at
/// touch-sized swatches, and colours. The options block's height varies (shape rows, the brush
/// block), so the glyph scroll gives up height first and the colour block never falls off the
/// panel.
pub fn sidebar(ui: &mut Ui, app: &mut GasciiApp) {
    ui.spacing_mut().item_spacing = Vec2::new(10.0, 12.0);
    tool_grid(ui, app, &kiosk_tools(), TOOL_COLS, TOOL_CELL_H);
    ui.add_space(2.0);
    binding_options(ui, app);
    ui.add_space(2.0);
    let scroll_h = (ui.available_height() - PALETTE_RESERVED).clamp(PALETTE_SCROLL_MIN, PALETTE_SCROLL_MAX);
    palette(ui, app, SWATCH, GLYPH_PX, scroll_h);
    ui.add_space(8.0);
    colors(ui, app);
}

/// Per-binding tool options: `L <tool> [− n +]` with the shape segment beneath. There is no
/// options bar in this mode and no L/R focus segment either — both bindings simply show at once.
/// Unsized tools get a dash where the stepper would be, so the rows always double as an L/R
/// legend. Brush's shared controls follow once, whichever binding holds it.
fn binding_options(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    widgets::micro_label(ui, "OPTIONS");
    for &b in Binding::ALL.iter() {
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
                    if widgets::stepper(ui, &mut size, 1, MAX_TOOL_SIZE, SIZE_STEPPER_H) {
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
            ui.horizontal(|ui| {
                // Indented to sit under the tool name, clear of the L/R gutter.
                ui.add_space(18.0);
                let mut shape = app.slots[b.ix()].stamps[slot].shape;
                let shapes = [
                    (BrushShape::Raw, "No Shape"),
                    (BrushShape::Square, "Square"),
                    (BrushShape::Circle, "Circle"),
                ];
                if widgets::segmented(ui, &mut shape, &shapes, false) {
                    app.slots[b.ix()].stamps[slot].shape = shape;
                }
            });
        }
    }
    if Binding::ALL.iter().any(|&b| app.slot(b).kind == ToolKind::Brush) {
        ui.add_space(2.0);
        brush_options(ui, app);
    }
}

/// Ramp, intensity mode/level and the pressure toggle — the same app-global state the options bar
/// edits, shown once regardless of which binding holds the Brush.
fn brush_options(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    widgets::micro_label(ui, "BRUSH");
    let mut ramp = app.active_ramp;
    let names: Vec<(usize, &str)> = app.ramps.iter().enumerate().map(|(i, r)| (i, r.name)).collect();
    if widgets::segmented(ui, &mut ramp, &names, false) {
        app.active_ramp = ramp;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
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
            let slider = ui.add_sized(
                Vec2::new(100.0, 24.0),
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
        }
    });
    // Same gate as the options bar: only offered once a pressure signal has been seen.
    if app.stylus_detected {
        widgets::checkbox(ui, &mut app.brush_pressure, "Pressure");
    }
}

fn colors(ui: &mut Ui, app: &mut GasciiApp) {
    widgets::micro_label(ui, "COLOR");
    ui.horizontal(|ui| {
        // Display + swap only, no popup — kiosk's touch-first colour row hands precise picking
        // off to the quick-color swatches below instead.
        widgets::color_wells(ui, widgets::rgba_to_color32(app.active_fg), widgets::rgba_to_color32(app.active_bg), WELL);
        ui.add_space(14.0);
        if widgets::swap_button(ui, SWAP_BUTTON) {
            app.swap_colors();
        }
    });
    ui.add_space(8.0);
    quick_colors(ui, app);
}

fn quick_colors(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    ui.spacing_mut().item_spacing = Vec2::splat(5.0);
    ui.horizontal(|ui| {
        for &c in QUICK_COLORS.iter() {
            let color = widgets::rgba_to_color32(c);
            let selected = app.active_fg == c;
            let resp = widgets::color_swatch(ui, color, t.border_soft, selected, QUICK_COLOR_H);
            if resp.clicked() {
                app.active_fg = c;
            } else if resp.secondary_clicked() {
                app.active_bg = c;
            }
        }
    });
}

/// The status bar: hovered cell, any live error, document size, and a static zoom readout —
/// kiosk's zoom auto-fits continuously, so there is no interactive zoom cluster to show here.
pub fn status_bar(ui: &mut Ui, app: &mut GasciiApp) {
    ui.spacing_mut().item_spacing.x = 20.0;
    let coord = app.hovered_cell.map(|(x, y)| format!("cell {x},{y}")).unwrap_or_else(|| "cell –".to_owned());
    super::status_bar::mono(ui, coord, false);
    if let Some(err) = app.last_error.clone() {
        let t = theme::current(ui.ctx());
        ui.label(egui::RichText::new(err).font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_error));
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        super::status_bar::mono(ui, format!("doc {}×{}", app.doc.width, app.doc.height), false);
        super::status_bar::mono(ui, "zoom: fit (auto)".to_owned(), true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Binding;

    /// Kiosk's sidebar has no cell for Text (no keyboard-driven session UI) — its tool list must
    /// never include it, and must otherwise stay in sync with `TOOLS`.
    #[test]
    fn kiosk_sidebars_tool_list_excludes_text() {
        let tools = kiosk_tools();
        assert_eq!(tools.len(), TOOLS.len() - 1, "every TOOLS entry except Text");
        assert!(!tools.iter().any(|d| d.kind == ToolKind::Text), "Text must not appear in the kiosk grid");
    }

    /// A binding already parked on Text when fullscreen is entered (deliberate user state, left
    /// alone per design) has no matching cell in kiosk's Text-excluded grid. `tool_grid` must
    /// render this without panicking and without highlighting any cell as if it matched.
    #[test]
    fn tool_grid_renders_without_panicking_when_a_binding_holds_a_tool_absent_from_its_list() {
        let mut app = crate::app::GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Text);

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            tool_grid(ui, &mut app, &kiosk_tools(), TOOL_COLS, TOOL_CELL_H);
        });

        // Structural guarantee, not just "it didn't panic": `tool_grid`'s highlight is an equality
        // check against each listed tool's kind, so a kind absent from the list (Text) can never
        // match — no cell shows a phantom L/R badge for it.
        assert!(
            kiosk_tools().iter().all(|d| d.kind != app.slot(Binding::L).kind),
            "sanity: L's Text binding has no equal in the kiosk grid's tool list"
        );
    }

    /// The options rows must render for a sized and an unsized binding alike (stepper+shape vs.
    /// dash), and rendering alone must never mutate any binding's configured size or shape.
    #[test]
    fn binding_option_rows_render_for_sized_and_unsized_tools_without_changing_any_setting() {
        let mut app = crate::app::GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, ToolKind::Fill);
        let l_slot = crate::app::sized_slot(ToolKind::Pencil).unwrap();
        app.slots[Binding::L.ix()].stamps[l_slot].size = 5;
        app.slots[Binding::L.ix()].stamps[l_slot].shape = BrushShape::Circle;

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            binding_options(ui, &mut app);
        });

        assert_eq!(
            app.slots[Binding::L.ix()].stamps[l_slot].size,
            5,
            "a render pass with no input must not step the configured size"
        );
        assert_eq!(
            app.slots[Binding::L.ix()].stamps[l_slot].shape,
            BrushShape::Circle,
            "a render pass with no input must not change the configured shape"
        );
        assert_eq!(app.slot(Binding::R).kind, ToolKind::Fill, "the unsized row is display-only");
    }

    /// The shared brush block renders whichever binding holds the Brush, and rendering alone must
    /// never flip the density mode or the active ramp.
    #[test]
    fn brush_block_renders_when_a_binding_holds_brush_without_mutating_brush_state() {
        let mut app = crate::app::GasciiApp::headless();
        app.bind(Binding::R, ToolKind::Brush);
        app.density_mode = DensityMode::Buildup(Buildup);
        let ramp_before = app.active_ramp;

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            binding_options(ui, &mut app);
        });

        assert!(
            matches!(app.density_mode, DensityMode::Buildup(_)),
            "a render pass with no input must not flip the density mode"
        );
        assert_eq!(app.active_ramp, ramp_before, "a render pass with no input must not change the ramp");
    }
}
