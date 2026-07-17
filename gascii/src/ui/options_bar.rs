//! The 40px contextual options bar: an L/R segment and that binding's options.

use eframe::egui::{self, Ui, Vec2};

use super::theme;
use super::widgets;
use crate::app::{sized_slot, Binding, GasciiApp, ToolKind};
use crate::fonts;
use gascii_core::{BrushShape, Buildup, DensityMode, Fixed, MAX_TOOL_SIZE};

pub const HEIGHT: f32 = 44.0;

pub fn show(ui: &mut Ui, app: &mut GasciiApp) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);

        // The L/R segment: which binding this bar is editing. Drawing with either button selects
        // its own segment, so the bar follows the button you last used.
        let mut focus = app.options_focus;
        let labels: Vec<(Binding, String)> = Binding::ALL
            .iter()
            .map(|&b| (b, format!("{}  {}", if b == Binding::L { "L" } else { "R" }, crate::app::tool_def(app.slot(b).kind).name)))
            .collect();
        let opts: Vec<(Binding, &str)> = labels.iter().map(|(b, s)| (*b, s.as_str())).collect();
        if widgets::segmented(ui, &mut focus, &opts, false) {
            app.options_focus = focus;
        }

        ui.add_space(6.0);
        divider(ui);
        ui.add_space(6.0);

        let b = app.options_focus;
        let kind = app.slot(b).kind;
        tool_options(ui, app, b, kind);
    });
}

fn divider(ui: &mut Ui) {
    let t = theme::current(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 20.0), egui::Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), egui::Stroke::new(1.0, t.border_soft));
}

fn label(ui: &mut Ui, text: &str) {
    let t = theme::current(ui.ctx());
    ui.label(egui::RichText::new(text).font(fonts::ui_medium_id(fonts::size::CONTROL)).color(t.fg_secondary));
}

/// The options for one binding's tool. Only the focused binding's show — that is what "contextual"
/// means here, and it is why the bar can stay 40px with nine tools.
fn tool_options(ui: &mut Ui, app: &mut GasciiApp, b: Binding, kind: ToolKind) {
    if let Some(slot) = sized_slot(kind) {
        label(ui, "Size");
        let mut size = app.slots[b.ix()].stamps[slot].size;
        if widgets::stepper(ui, &mut size, 1, MAX_TOOL_SIZE, widgets::STEPPER_H) {
            app.slots[b.ix()].stamps[slot].size = size;
        }
        ui.add_space(6.0);

        label(ui, "Shape");
        let mut shape = app.slots[b.ix()].stamps[slot].shape;
        let shapes = [
            (BrushShape::Raw, "No Shape"),
            (BrushShape::Square, "Square"),
            (BrushShape::Circle, "Circle"),
        ];
        if widgets::segmented(ui, &mut shape, &shapes, false) {
            app.slots[b.ix()].stamps[slot].shape = shape;
        }
    }

    if kind == ToolKind::Brush {
        ui.add_space(6.0);
        divider(ui);
        ui.add_space(6.0);
        brush_options(ui, app);
    }
}

/// Ramp, intensity mode and level. App-global state (both bindings' brushes share them), so the
/// bar shows them whenever the focused binding holds a Brush.
fn brush_options(ui: &mut Ui, app: &mut GasciiApp) {
    label(ui, "Ramp");
    let mut ramp = app.active_ramp;
    let names: Vec<(usize, &str)> = app.ramps.iter().enumerate().map(|(i, r)| (i, r.name)).collect();
    if widgets::segmented(ui, &mut ramp, &names, false) {
        app.active_ramp = ramp;
    }

    ui.add_space(6.0);
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
        ui.add_space(6.0);
        // Narrow: the bar already carries Size, Shape and Ramp.
        let slider = ui.add_sized(
            Vec2::new(80.0, 20.0),
            egui::Slider::new(&mut level, 0.0..=1.0).show_value(false),
        );
        if slider.changed() || changed {
            app.density_mode = DensityMode::Fixed(Fixed(level));
        }
        let t = theme::current(ui.ctx());
        ui.label(
            egui::RichText::new(format!("{:.0}%", level * 100.0))
                .font(fonts::mono_id(fonts::size::LABEL))
                .color(t.fg_secondary),
        );
    }

    // Only shown once a stylus contact has actually been observed this session — no point
    // offering a pressure toggle before there is any pressure signal to drive it.
    if app.stylus_detected {
        ui.add_space(6.0);
        widgets::checkbox(ui, &mut app.brush_pressure, "Pressure");
    }
}
