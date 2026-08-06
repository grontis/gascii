//! Kiosk timeline variant: the same capability set as `timeline::show`, not a reduced subset,
//! rendered at touch geometry via `timeline::body`. Mirrors `gascii::ui::kiosk::sidebar` delegating
//! to `sidebar::binding_options_geom` for the shared per-tool layout.
//!
//! `SWATCH`/`TOOL_CELL_H` (`gascii::ui::kiosk`'s own touch-target constants) aren't `pub` outside
//! `gascii` — their literal values are redeclared here rather than assumed reusable, with a doc
//! comment cross-referencing the originals, matching `gascii-density-brush`'s own small-duplication
//! discipline for exactly this class of cross-crate constant.

use egui::{Ui, Vec2};
use gascii_core::Document;
use gascii_plugin_api::PanelOutcome;

use crate::shared::SharedState;
use crate::thumbnail::ThumbnailCache;

/// Mirrors `gascii::ui::kiosk::TOOL_CELL_H` (68.0).
const TOUCH_CONTROL_H: f32 = 68.0;
/// Mirrors `gascii::ui::kiosk::SWATCH` (48.0), doubled on the wide axis so a thumbnail stays
/// legible at touch scale.
const TOUCH_THUMB: Vec2 = Vec2::new(96.0, 60.0);
/// Sized for `body`'s two control rows at `TOUCH_CONTROL_H` plus the touch-scale thumbnail strip —
/// absolute rather than derived from `timeline::PANEL_H`, whose windowed control rows are far
/// shorter than the 68px touch ones.
const PANEL_H: f32 = 240.0;

pub(crate) fn show(
    ui: &mut Ui,
    doc: &Document,
    state: &SharedState,
    thumbs: &mut ThumbnailCache,
    top_edit_id: Option<u64>,
) -> PanelOutcome {
    let mut outcome = PanelOutcome::default();
    let resp = egui::Panel::bottom("gascii_anim_timeline_kiosk")
        .frame(crate::timeline::panel_frame(ui.ctx()))
        .exact_size(PANEL_H)
        .show(ui, |ui| {
            outcome = crate::timeline::body(
                ui,
                doc,
                state,
                thumbs,
                TOUCH_THUMB,
                TOUCH_CONTROL_H,
                top_edit_id,
            );
        });
    outcome.pressed_inside = crate::timeline::pressed_inside(ui, resp.response.rect);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use gascii_core::{add_frame, Frame, History};

    fn doc_with_frames(n: usize) -> Document {
        let mut doc = Document::default_document();
        let mut history = History::new();
        for i in 1..n {
            let edit = add_frame(&doc, i, Frame::blank(doc.width, doc.height)).unwrap();
            history.apply(&mut doc, edit);
        }
        doc
    }

    /// Both chrome variants must produce equivalent `PanelOutcome`s for the same document state on
    /// a no-input render — proves kiosk didn't drop a capability `timeline::show` has, only its
    /// geometry differs.
    #[test]
    fn kiosk_and_windowed_timelines_expose_the_same_frame_operations() {
        let doc = doc_with_frames(3);
        let state = SharedState::new();
        let mut thumbs_windowed = ThumbnailCache::new();
        let mut thumbs_kiosk = ThumbnailCache::new();

        let ctx = egui::Context::default();
        let mut windowed = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            windowed = Some(crate::timeline::show(
                ui,
                &doc,
                &state,
                &mut thumbs_windowed,
                Some(1),
            ));
        });
        let windowed = windowed.unwrap();

        let ctx2 = egui::Context::default();
        let mut kiosk = None;
        let _ = ctx2.run_ui(egui::RawInput::default(), |ui| {
            kiosk = Some(show(ui, &doc, &state, &mut thumbs_kiosk, Some(1)))
        });
        let kiosk = kiosk.unwrap();

        assert_eq!(windowed.edits.len(), kiosk.edits.len());
        assert_eq!(windowed.properties, kiosk.properties);
    }
}
