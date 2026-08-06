//! Kiosk layers-panel variant: the same capability set as `panel::show`, not a reduced subset,
//! rendered at touch geometry via `panel::body`. Mirrors `gascii-anim::kiosk`'s own delegation to
//! `timeline::body` at a different `row_h`/`control_h`.
//!
//! `TOUCH_CONTROL_H` mirrors `gascii::ui::kiosk::TOOL_CELL_H` (68.0) — its literal value is
//! redeclared here rather than assumed reusable, matching `gascii-anim::kiosk`'s own
//! small-duplication discipline for exactly this class of cross-crate constant.

use egui::Ui;
use gascii_core::Document;
use gascii_plugin_api::PanelOutcome;

const TOUCH_CONTROL_H: f32 = 68.0;
const TOUCH_ROW_H: f32 = 56.0;
const PANEL_W: f32 = crate::panel::PANEL_W + 60.0;

pub(crate) fn show(ui: &mut Ui, doc: &Document) -> PanelOutcome {
    let mut outcome = PanelOutcome::default();
    egui::Panel::right("gascii_layers_panel_kiosk")
        .frame(crate::panel::panel_frame(ui.ctx()))
        .exact_size(PANEL_W)
        .show(ui, |ui| {
            outcome = crate::panel::body(ui, doc, TOUCH_ROW_H, TOUCH_CONTROL_H);
        });
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::History;

    fn doc_with_layers(n: usize) -> Document {
        let mut doc = Document::new(4, 4);
        let mut history = History::new();
        for _ in 1..n {
            let edit = gascii_core::add_layer(&doc, doc.layer_count()).unwrap();
            history.apply(&mut doc, edit);
        }
        doc
    }

    /// Both chrome variants must produce equivalent `PanelOutcome`s for the same document state on
    /// a no-input render — proves kiosk didn't drop a capability `panel::show` has, only its
    /// geometry differs.
    #[test]
    fn kiosk_and_windowed_panels_expose_the_same_layer_operations_on_a_no_input_render() {
        let doc = doc_with_layers(3);

        let ctx = egui::Context::default();
        let mut windowed = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            windowed = Some(crate::panel::show(ui, &doc));
        });
        let windowed = windowed.unwrap();

        let ctx2 = egui::Context::default();
        let mut kiosk = None;
        let _ = ctx2.run_ui(egui::RawInput::default(), |ui| kiosk = Some(show(ui, &doc)));
        let kiosk = kiosk.unwrap();

        assert_eq!(windowed.edits.len(), kiosk.edits.len());
        assert_eq!(windowed.properties, kiosk.properties);
    }

    #[test]
    fn kiosk_show_renders_without_panicking() {
        let doc = doc_with_layers(2);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = show(ui, &doc);
        });
    }
}
