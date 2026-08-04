//! The windowed-chrome menu bar (File/Edit/Animation/View). Kiosk chrome has no menu bar at all —
//! see `ui::kiosk`.

use eframe::egui;

use crate::app::GasciiApp;
use crate::chords::{self, ChordId};

pub fn show(ui: &mut egui::Ui, app: &mut GasciiApp) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            if ui.add(egui::Button::new("New…").shortcut_text(chords::chord_label(ChordId::New))).clicked() {
                app.new_document_via_menu();
            }
            if ui.add(egui::Button::new("Open…").shortcut_text(chords::chord_label(ChordId::Open))).clicked() {
                app.open_file();
            }
            ui.separator();
            if ui.add(egui::Button::new("Save").shortcut_text(chords::chord_label(ChordId::Save))).clicked() {
                app.save_file();
            }
            if ui
                .add(egui::Button::new("Save As…").shortcut_text(chords::chord_label(ChordId::SaveAs)))
                .clicked()
            {
                app.save_file_as();
            }
            ui.separator();
            if ui
                .add(egui::Button::new("Export…").shortcut_text(chords::chord_label(ChordId::ExportDialog)))
                .clicked()
            {
                app.open_export_dialog();
            }
            ui.separator();
            ui.menu_button("Recent Files", |ui| {
                if app.recent_files.is_empty() {
                    ui.weak("No recent files");
                }
                let mut pick = None;
                for path in &app.recent_files {
                    let label = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    if ui.button(label).clicked() {
                        pick = Some(path.clone());
                    }
                }
                if let Some(path) = pick {
                    app.open_path(&path);
                }
            });
        });
        ui.menu_button("Edit", |ui| {
            // Disabled mid-gesture for the same reason handle_keys ignores Ctrl+Z/Y then:
            // an undo under an in-flight stroke's pinned `before` values commits stale cells.
            let no_stroke = !app.stroke_in_progress();
            let undo = egui::Button::new("Undo").shortcut_text(chords::chord_label(ChordId::Undo));
            if ui.add_enabled(app.history.can_undo() && no_stroke, undo).clicked() {
                app.request_undo();
            }
            // Both Ctrl+Shift+Z and Ctrl+Y trigger a redo — `ChordId::Redo`'s label documents
            // both, closing a label-drift gap (Ctrl+Shift+Z was previously undocumented here
            // even though it already worked).
            let redo = egui::Button::new("Redo").shortcut_text(chords::chord_label(ChordId::Redo));
            if ui.add_enabled(app.history.can_redo() && no_stroke, redo).clicked() {
                app.request_redo();
            }
            ui.separator();
            let can_copy = app
                .selection_slot()
                .and_then(|b| app.slot(b).tool.selection_overlay())
                .and_then(|v| v.marquee)
                .is_some();
            let copy = egui::Button::new("Copy Selection").shortcut_text(chords::chord_label(ChordId::Copy));
            if ui.add_enabled(can_copy, copy).clicked() {
                app.copy_selection(ui.ctx());
            }
            let copy_all =
                egui::Button::new("Copy All as Text").shortcut_text(chords::chord_label(ChordId::CopyAll));
            if ui.add(copy_all).clicked() {
                // Flush first: a pending text burst or floating selection lives only in
                // `app.slots[0].tool`'s overlay until committed into `app.doc` — copying without
                // flushing would silently drop just-typed or just-moved content from the
                // whole-document clipboard contents.
                app.flush_all();
                ui.ctx().copy_text(gascii_core::export_text(&app.doc));
            }
            let paste = egui::Button::new("Paste").shortcut_text(chords::chord_label(ChordId::Paste));
            if ui.add(paste).clicked() {
                // Reads the OS clipboard on demand via `arboard`. A real Ctrl+V keypress pastes
                // through `egui::Event::Paste` instead (`canvas.rs`) — this menu item exists
                // because a menu click is not itself a key event egui surfaces the clipboard on.
                match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
                    Ok(text) => app.paste_text(&text),
                    Err(e) => app.flash_error(format!("paste: clipboard read failed: {e}")),
                }
            }
            let cut = egui::Button::new("Cut").shortcut_text(chords::chord_label(ChordId::Cut));
            if ui.add_enabled(can_copy, cut).clicked() {
                let ctx = ui.ctx().clone();
                app.cut_selection(&ctx);
            }
            let duplicate =
                egui::Button::new("Duplicate Selection").shortcut_text(chords::chord_label(ChordId::Duplicate));
            if ui.add_enabled(can_copy, duplicate).clicked() {
                app.duplicate_selection();
            }
            ui.separator();
            let select_all = egui::Button::new("Select All").shortcut_text(chords::chord_label(ChordId::SelectAll));
            if ui.add(select_all).clicked() {
                app.select_all();
            }
            let can_deselect = app.selection_slot().is_some();
            // Esc is `canvas.rs`'s own Selection-Escape branch, not a `CHORDS` row — a literal
            // hint, since `chord_label` only speaks for registered chords.
            let deselect = egui::Button::new("Deselect").shortcut_text("Esc");
            if ui.add_enabled(can_deselect, deselect).clicked() {
                app.deselect();
            }
            ui.separator();
            if ui.button("Resize Canvas…").clicked() {
                app.open_resize_dialog();
            }
        });
        // Hidden while gascii-anim is disabled: a second frame with no timeline panel to
        // manage it would be stranded. Export of existing multi-frame documents is unaffected
        // — frames are a document property, not the plugin's.
        if app.anim_plugin_enabled() {
            ui.menu_button("Animation", |ui| {
                if ui.button("Add Frame").clicked() {
                    app.add_frame_via_menu();
                }
            });
        }
        ui.menu_button("View", |ui| {
            if ui.add(egui::Button::new("Zoom In").shortcut_text(chords::chord_label(ChordId::ZoomIn))).clicked() {
                app.step_zoom(1);
            }
            if ui
                .add(egui::Button::new("Zoom Out").shortcut_text(chords::chord_label(ChordId::ZoomOut)))
                .clicked()
            {
                app.step_zoom(-1);
            }
            if ui.add(egui::Button::new("Fit").shortcut_text(chords::chord_label(ChordId::Fit))).clicked() {
                app.pending_fit = true;
            }
            ui.separator();
            ui.checkbox(&mut app.show_grid, format!("Grid  ({})", chords::chord_label(ChordId::ToggleGrid)));
            ui.separator();
            ui.menu_button("Theme", |ui| {
                let mut pref = app.theme_pref;
                ui.radio_value(&mut pref, egui::ThemePreference::Light, "Light");
                ui.radio_value(&mut pref, egui::ThemePreference::Dark, "Dark");
                ui.radio_value(&mut pref, egui::ThemePreference::System, "System");
                if pref != app.theme_pref {
                    app.theme_pref = pref;
                    ui.ctx().set_theme(pref);
                }
            });
            ui.separator();
            if ui.button("Plugins…").clicked() {
                app.open_plugins_dialog();
            }
            ui.separator();
            // Kiosk chrome shows no menu bar at all, so the "Exit…" label is unreachable in
            // practice while fullscreen — implemented anyway for symmetry/defensiveness and
            // because the toggle's contract (label always names the action it performs) should
            // hold regardless of which chrome happens to expose it.
            let is_fs = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
            let label = if is_fs { "Exit Full Screen Mode" } else { "Enter Full Screen Mode" };
            if ui
                .add(egui::Button::new(label).shortcut_text(chords::chord_label(ChordId::ToggleFullscreen)))
                .clicked()
            {
                let ctx = ui.ctx().clone();
                app.toggle_fullscreen(&ctx);
            }
        });
    });
}
