//! Full Screen Mode's chrome: touch/stylus-first, replaces the titlebar/menubar/normal sidebar/
//! normal status bar entirely while active. Built on the same painting primitives normal chrome
//! uses (`widgets`, `sidebar::tool_grid`/`palette`) at larger geometry — the only genuinely new
//! painter is `widgets::color_swatch` (the quick-color row has no normal-mode equivalent).

use eframe::egui::{self, Align2, Pos2, Rect, Ui, UiBuilder, Vec2};

use super::sidebar::{self, color_picker_body, palette, rule, tool_grid};
use super::{theme, widgets};
use crate::app::{tools, GasciiApp};
use crate::fonts;
use gascii_plugin_api::OptionsGeom;

pub const TOP_H: f32 = 44.0;
pub const SIDEBAR_W: f32 = 340.0;
pub const STATUS_H: f32 = 36.0;
const TOOL_COLS: usize = 4;
const TOOL_CELL_H: f32 = 68.0;
const SWATCH: f32 = 48.0;
const GLYPH_PX: f32 = 26.0;
const PALETTE_SCROLL_MAX: f32 = 300.0;
const PALETTE_SCROLL_MIN: f32 = 120.0;
/// Height the palette's own tabs/RECENT rows plus the colour block need below the options
/// section, split into what scales with the touch geometry (the RECENT row, the colour block —
/// `color_wells` paints an overlapped 2×`WELL` square, not a single `WELL`-tall row) and what
/// doesn't (labels, tabs, spacing). The glyph scroll area gets whatever is left, clamped.
const PALETTE_RESERVED_FIXED: f32 = 152.0;
const PALETTE_RESERVED_SCALED: f32 = 150.0;
const SIZE_STEPPER_H: f32 = 36.0;
const WELL: f32 = 36.0;
const SWAP_BUTTON: f32 = 36.0;
const QUICK_COLOR_H: f32 = 32.0;
/// Panel content height at which the full-size touch geometry fits with the glyph scroll at its
/// minimum. Shorter panels shrink every touch-sized element together via `scale_for` instead of
/// forcing the sidebar to scroll.
const COMFORT_H: f32 = 880.0;
/// Floor on the shrink so targets stay comfortably tappable.
const SCALE_MIN: f32 = 0.7;

fn scale_for(panel_h: f32) -> f32 {
    (panel_h / COMFORT_H).clamp(SCALE_MIN, 1.0)
}

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

/// The sidebar's tool grid: every registry entry with `kiosk_visible` set (every tool but Text).
/// Kiosk has no keyboard-driven session UI, so a binding parked on Text here would have no cell to
/// highlight — see `crate::app::tool_shortcut_reachable`, which keeps the `T` shortcut from
/// reaching Text while fullscreen for the same reason (the same `kiosk_visible` fact). A binding
/// already on Text when fullscreen is entered is left alone by both; `tool_grid`'s equality-based
/// highlight simply shows no cell selected in that case, never panics or paints a phantom badge
/// (pinned by `kiosk_sidebars_tool_list_excludes_text` and
/// `tool_grid_renders_without_panicking_when_a_binding_holds_a_tool_absent_from_its_list`).
fn kiosk_tools(app: &GasciiApp) -> Vec<crate::app::ToolDef> {
    tools().iter().copied().filter(|d| d.kiosk_visible && app.tool_enabled(d.kind)).collect()
}

/// The sidebar: a 4×2 tool grid (Text excluded), both bindings' tool options, the glyph palette
/// at touch-sized swatches, and colours. The options block's height varies (shape rows, the brush
/// block), so the glyph scroll gives up height first; short panels shrink the touch geometry via
/// `scale_for`, and if even that doesn't fit the whole sidebar scrolls rather than clipping the
/// colour block out of reach.
pub fn sidebar(ui: &mut Ui, app: &mut GasciiApp) {
    let t = theme::current(ui.ctx());
    let panel_h = ui.available_height();
    let k = scale_for(panel_h);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(10.0, 12.0);
        let top = ui.cursor().min.y;
        let ktools = kiosk_tools(app);
        tool_grid(ui, app, &ktools, TOOL_COLS, TOOL_CELL_H * k);
        rule(ui, t.border_soft);
        binding_options(ui, app, k);
        rule(ui, t.border_soft);
        sidebar::trace_controls(ui, app, k);
        rule(ui, t.border_soft);
        // available_height is unbounded inside the scroll area — size the glyph scroll against
        // the panel's real height minus what the grid and options actually consumed.
        let remaining = panel_h - (ui.cursor().min.y - top);
        let reserved = PALETTE_RESERVED_FIXED + PALETTE_RESERVED_SCALED * k;
        let scroll_h = (remaining - reserved).clamp(PALETTE_SCROLL_MIN * k, PALETTE_SCROLL_MAX);
        palette(ui, app, SWATCH * k, GLYPH_PX * k, scroll_h);
        ui.add_space(8.0);
        rule(ui, t.border_soft);
        colors(ui, app, k);
    });
}

/// Per-binding tool options at touch geometry — kiosk's own `OptionsGeom`, delegating the actual
/// per-tool layout to `sidebar::binding_options_geom` (the same renderer the normal sidebar's own
/// `binding_options` uses at its geometry) so the row/shape/options-callback structure is written
/// once. Kiosk's deltas: a taller stepper, an 18px SHAPE indent (wrapped in its own `ui.horizontal`
/// per row, clear of the L/R gutter), no vertical-spacing override (inherits `sidebar`'s own
/// `item_spacing`), and Brush's Fixed/Buildup control sharing a row with its slider.
fn binding_options(ui: &mut Ui, app: &mut GasciiApp, k: f32) {
    sidebar::binding_options_geom(
        ui,
        app,
        OptionsGeom {
            stepper_h: SIZE_STEPPER_H * k,
            shape_indent: 18.0,
            item_spacing_y: None,
            inline_controls: true,
            slider_h: 24.0,
        },
    );
}

fn colors(ui: &mut Ui, app: &mut GasciiApp, k: f32) {
    widgets::micro_label(ui, "COLOR");
    let mut wells = None;
    ui.horizontal(|ui| {
        // Tapping a well opens the full picker in a popup, touch-scaled — the quick-color row
        // below stays for fast taps.
        wells = Some(widgets::color_wells(
            ui,
            widgets::rgba_to_color32(app.active_fg),
            widgets::rgba_to_color32(app.active_bg),
            WELL * k,
        ));
        ui.add_space(14.0);
        if widgets::swap_button(ui, SWAP_BUTTON * k) {
            app.swap_colors();
        }
    });
    if let Some(wells) = wells {
        touch_color_popup(ui, &wells.fg, &mut app.active_fg, k);
        touch_color_popup(ui, &wells.bg, &mut app.active_bg, k);
    }
    ui.add_space(8.0);
    quick_colors(ui, app, k);
    ui.add_space(8.0);
    // Same explicit recolor action the windowed sidebar's COLORS section offers.
    let can_recolor = app
        .selection_slot()
        .and_then(|b| app.slot(b).tool.selection_overlay())
        .and_then(|v| v.marquee)
        .is_some();
    if widgets::button(ui, "Recolor Selection", false, can_recolor).clicked() {
        app.recolor_selection();
    }
}

/// `color_picker_body` hung off a well, scaled up for touch — bumps the interact size and slider
/// width the picker's RGBA sliders and hue bar use, so the popup keeps kiosk's tappable geometry
/// rather than falling back to normal-chrome sizing.
fn touch_color_popup(ui: &Ui, resp: &egui::Response, color: &mut gascii_core::Rgba, k: f32) {
    egui::Popup::from_toggle_button_response(resp).show(|ui| {
        ui.spacing_mut().interact_size *= k;
        ui.spacing_mut().slider_width *= k;
        color_picker_body(ui, color);
    });
    let _ = ui;
}

fn quick_colors(ui: &mut Ui, app: &mut GasciiApp, k: f32) {
    let t = theme::current(ui.ctx());
    ui.spacing_mut().item_spacing = Vec2::splat(5.0);
    ui.horizontal(|ui| {
        for &c in QUICK_COLORS.iter() {
            let color = widgets::rgba_to_color32(c);
            let selected = app.active_fg == c;
            let resp = widgets::color_swatch(ui, color, t.border_soft, selected, QUICK_COLOR_H * k);
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
    if let Some((err, left)) = app.error_flash(std::time::Instant::now()) {
        let t = theme::current(ui.ctx());
        ui.label(egui::RichText::new(err).font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_error));
        ui.ctx().request_repaint_after(left);
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        super::status_bar::mono(ui, format!("doc {}×{}", app.doc.width, app.doc.height), false);
        super::status_bar::mono(ui, "zoom: fit (auto)".to_owned(), true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Binding, ToolKind, BRUSH_KIND};
    use gascii_core::{Buildup, BrushShape, DensityMode};

    /// Full size on tall panels, proportional shrink below `COMFORT_H`, floored at `SCALE_MIN` so
    /// touch targets never collapse on very short screens.
    #[test]
    fn scale_for_is_full_size_on_tall_panels_proportional_below_and_floored() {
        assert_eq!(scale_for(COMFORT_H), 1.0);
        assert_eq!(scale_for(2000.0), 1.0);
        assert!((scale_for(COMFORT_H * 0.85) - 0.85).abs() < 1e-4);
        assert_eq!(scale_for(100.0), SCALE_MIN);
    }

    /// Kiosk's sidebar has no cell for Text (no keyboard-driven session UI) — its tool list must
    /// never include it, and must otherwise stay in sync with the tool registry.
    #[test]
    fn kiosk_sidebars_tool_list_excludes_text() {
        let app = crate::app::GasciiApp::headless();
        let tools = kiosk_tools(&app);
        assert_eq!(tools.len(), crate::app::tools().len() - 1, "every registry entry except Text");
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
            let ktools = kiosk_tools(&app);
            tool_grid(ui, &mut app, &ktools, TOOL_COLS, TOOL_CELL_H);
        });

        // Structural guarantee, not just "it didn't panic": `tool_grid`'s highlight is an equality
        // check against each listed tool's kind, so a kind absent from the list (Text) can never
        // match — no cell shows a phantom L/R badge for it.
        assert!(
            kiosk_tools(&app).iter().all(|d| d.kind != app.slot(Binding::L).kind),
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
            binding_options(ui, &mut app, 1.0);
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
        app.bind(Binding::R, BRUSH_KIND);
        app.brush_plugin_mut().set_density_mode(DensityMode::Buildup(Buildup));
        let ramp_before = app.brush_plugin_mut().active_ramp();

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            binding_options(ui, &mut app, 1.0);
        });

        assert!(
            matches!(app.brush_plugin_mut().density_mode(), DensityMode::Buildup(_)),
            "a render pass with no input must not flip the density mode"
        );
        assert_eq!(app.brush_plugin_mut().active_ramp(), ramp_before, "a render pass with no input must not change the ramp");
    }

    /// The colour block (wells + K1 well popups + quick-color row) must render without panicking,
    /// and a render with no pointer input must not mutate either active colour.
    #[test]
    fn colors_renders_without_panicking_or_mutating_active_colors() {
        let mut app = crate::app::GasciiApp::headless();
        let fg_before = app.active_fg;
        let bg_before = app.active_bg;

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            colors(ui, &mut app, 1.0);
        });

        assert_eq!(app.active_fg, fg_before, "a render pass with no input must not change the FG colour");
        assert_eq!(app.active_bg, bg_before, "a render pass with no input must not change the BG colour");
    }

    /// Full-stack smoke test for kiosk's `TRACE` wiring (`sidebar::trace_controls` called from
    /// kiosk's own `sidebar`, sitting between `binding_options` and the palette): the whole sidebar
    /// must render with an image background loaded without panicking, and a no-input render must
    /// not mutate the loaded image's settings — mirrors the normal-chrome coverage in
    /// `ui::sidebar`'s own tests.
    #[test]
    fn kiosk_sidebar_renders_with_an_image_loaded_without_panicking_or_mutating_its_settings() {
        let mut app = crate::app::GasciiApp::headless();
        app.image_bg = Some(crate::image_bg::ImageBackground::new(image::RgbaImage::new(4, 3), None, None));

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            sidebar(ui, &mut app);
        });

        let bg = app.image_bg.as_ref().expect("a no-input render must not clear the loaded image");
        assert!(bg.show_as_trace, "a no-input render must not change trace visibility");
        assert!((bg.trace_opacity - 0.5).abs() < f32::EPSILON, "a no-input render must not change opacity");
    }
}
