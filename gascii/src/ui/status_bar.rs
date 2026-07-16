//! The 28px status bar: `cell 12,4 · sel 13×2 · [− 200% + Fit] · doc 80×25`. All mono 11px.

use eframe::egui::{self, Ui};

use super::theme;
use super::widgets;
use crate::app::GasciiApp;
use crate::fonts;
use crate::viewport::ZOOM_SCALES;

pub const HEIGHT: f32 = 28.0;

fn mono(ui: &mut Ui, text: String, secondary: bool) {
    let t = theme::current(ui.ctx());
    let color = if secondary { t.fg_secondary } else { t.fg_text };
    ui.label(egui::RichText::new(text).font(fonts::mono_id(11.0)).color(color));
}

pub fn show(ui: &mut Ui, app: &mut GasciiApp) {
    ui.spacing_mut().item_spacing.x = 16.0;

    let coord = app
        .hovered_cell
        .map(|(x, y)| format!("cell {x},{y}"))
        .unwrap_or_else(|| "cell –".to_owned());
    mono(ui, coord, false);

    // The selection readout only exists while a selection does.
    if let Some(rect) = app
        .selection_slot()
        .and_then(|b| app.slot(b).tool.selection_overlay())
        .and_then(|v| v.marquee)
    {
        mono(ui, format!("sel {}×{}", rect.width(), rect.height()), true);
    }

    // Errors take the flexible middle — the one place with room, and next to nothing they would
    // push around. `fg_error`, never `fg_text`: an error rendered like ordinary telemetry is an
    // error the user misses.
    if let Some(err) = app.last_error.clone() {
        let t = theme::current(ui.ctx());
        ui.label(egui::RichText::new(err).font(fonts::mono_id(11.0)).color(t.fg_error));
    }

    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        mono(ui, format!("doc {}×{}", app.doc.width, app.doc.height), false);
        ui.add_space(0.0);
        zoom_cluster(ui, app);
    });
}

/// `[− 200% + Fit]` — a segmented group with soft borders.
fn zoom_cluster(ui: &mut Ui, app: &mut GasciiApp) {
    // Right-to-left layout, so these are added in reverse of how they read.
    if widgets::mini_button(ui, "Fit", true) {
        app.pending_fit = true;
    }
    if widgets::mini_button(ui, "+", true) {
        step_zoom(app, 1);
    }
    mono(ui, format!("{:.0}%", app.viewport.scale() * 100.0), false);
    if widgets::mini_button(ui, "–", true) {
        step_zoom(app, -1);
    }
}

/// Zoom by one step about the viewport's centre. Unlike `zoom_at`, there is no cursor to anchor to —
/// the buttons are in the status bar — so the pan is left alone and the document grows from its
/// top-left. `Fit` is the way back to centred.
fn step_zoom(app: &mut GasciiApp, dir: i32) {
    let next = (app.viewport.zoom_step as i32 + dir).clamp(0, ZOOM_SCALES.len() as i32 - 1);
    app.viewport.zoom_step = next as usize;
}
