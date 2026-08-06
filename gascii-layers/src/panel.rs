//! The windowed layers panel: a right-edge strip listing every layer, top-of-stack first (the
//! highest layer index drawn at the top of the list — the convention every layer-based editor
//! uses), above an Add/Duplicate/Delete/Move control row. `body` is the shared capability set both
//! this and `kiosk.rs` render, mirroring `gascii-anim::timeline`'s own windowed/kiosk split.
//!
//! Because the list is drawn top-of-stack first, "Move Up"/"Move Down" (which move a row up or
//! down *in the displayed list*) map to *raising*/*lowering* the layer's own index — the opposite
//! direction `frame_ops`'s own index-0-first strip would use. Pinned by name in this module's own
//! tests rather than left implicit.
//!
//! `layer_ops::*` failures (hitting `MAX_LAYERS`, the cell budget, `LastLayer`) surface through
//! `PanelOutcome::error` via `layer_op_error_message`, mirroring `gascii-anim::timeline::
//! frame_op_error_message`'s own per-variant wording convention exactly.
//!
//! Renaming: a pencil-icon button next to each name swaps the row into an inline
//! `egui::TextEdit`, committing on Enter or any other loss of focus (`egui::TextEdit`'s own Enter
//! handling surrenders focus rather than consuming the event, so a single `lost_focus()` check
//! covers both). No existing click-to-rename label precedent was found under `gascii/src/ui/`, so
//! this is the documented safe default.

use egui::{Color32, Sense, Ui, Vec2};
use gascii_core::{Document, Edit, LayerOpError};
use gascii_plugin_api::{DocProperty, PanelOutcome};

use crate::theme;
use crate::widgets;

pub(crate) const PANEL_W: f32 = 220.0;

/// No frame stroke: egui's `Panel` already draws its own separator line at the content-facing
/// edge, and a stroked frame is wider than the `exact_size` the panel reserves from the parent —
/// a right-edge panel resolves that overflow by sliding off-window, letting the central panel
/// paint over its left edge.
pub(crate) fn panel_frame(ctx: &egui::Context) -> egui::Frame {
    let t = theme::current(ctx);
    egui::Frame::new()
        .fill(t.bg_panel)
        .inner_margin(egui::Margin::symmetric(10, 8))
}

pub(crate) fn show(ui: &mut Ui, doc: &Document) -> PanelOutcome {
    let mut outcome = PanelOutcome::default();
    egui::Panel::right("gascii_layers_panel")
        .frame(panel_frame(ui.ctx()))
        .exact_size(PANEL_W)
        .show(ui, |ui| {
            outcome = body(ui, doc, 26.0, 24.0);
        });
    outcome
}

/// Maps a `layer_ops` failure to a specific, readable message — mirrors `gascii-anim::timeline::
/// frame_op_error_message`'s own per-variant convention exactly, so a failure at the same boundary
/// reads identically regardless of which crate's control triggered it.
pub(crate) fn layer_op_error_message(action: &str, err: LayerOpError) -> String {
    match err {
        LayerOpError::TooManyLayers { max, .. } => format!("{action}: exceeds the {max} maximum"),
        LayerOpError::TotalCellBudgetExceeded { .. } => {
            format!("{action}: exceeds the maximum total cell budget")
        }
        LayerOpError::IndexOutOfBounds { .. }
        | LayerOpError::LastLayer
        | LayerOpError::LayerCountDesync => {
            format!("{action}: unexpected error")
        }
    }
}

fn add_layer_after_active(doc: &Document) -> Result<Edit, LayerOpError> {
    gascii_core::add_layer(doc, doc.active_layer() + 1)
}

fn duplicate_active_layer(doc: &Document) -> Result<Edit, LayerOpError> {
    gascii_core::duplicate_layer(doc, doc.active_layer())
}

fn delete_active_layer(doc: &Document) -> Option<Edit> {
    gascii_core::remove_layer(doc, doc.active_layer()).ok()
}

/// Moves the active layer's row up *in the displayed list* — since the list is drawn top-of-stack
/// first, that means raising its own index toward the top of the stack. `None` at the top index
/// (the row is already at the top of the list, nothing to move it past).
fn move_active_layer_up(doc: &Document) -> Option<Edit> {
    let a = doc.active_layer();
    if a + 1 >= doc.layer_count() {
        return None;
    }
    gascii_core::reorder_layer(doc, a, a + 1).ok().flatten()
}

/// Moves the active layer's row down *in the displayed list* — lowering its own index toward the
/// bottom of the stack. `None` at index 0 (the row is already at the bottom of the list).
fn move_active_layer_down(doc: &Document) -> Option<Edit> {
    let a = doc.active_layer();
    if a == 0 {
        return None;
    }
    gascii_core::reorder_layer(doc, a, a - 1).ok().flatten()
}

fn toggle_layer_visibility(doc: &Document, index: usize) -> Option<Edit> {
    gascii_core::set_layer_visibility(doc, index, !doc.layer_visible(index))
        .ok()
        .flatten()
}

/// `None` when `name` already matches the layer's current name (`set_layer_name`'s own no-op
/// contract) — a rename commit that didn't actually change anything produces no edit.
fn commit_layer_rename(doc: &Document, index: usize, name: &str) -> Option<Edit> {
    gascii_core::set_layer_name(doc, index, name.to_string())
        .ok()
        .flatten()
}

/// Whether a row click should request `index` become the active layer — the pure decision core,
/// separated from click detection itself so it's directly testable without a live `Ui`.
fn active_layer_request(index: usize, active: usize, clicked: bool) -> Option<DocProperty> {
    if clicked && index != active {
        Some(DocProperty::ActiveLayer(index))
    } else {
        None
    }
}

/// The one row, if any, currently mid-rename, plus its live edit buffer — lives in egui's own temp
/// storage (mirrors `gascii::ui::sidebar::color_picker_body`'s own `HexBuf` precedent) rather than
/// plugin-owned state, since `LayersPlugin` itself is stateless.
#[derive(Clone)]
struct RenameBuf {
    index: usize,
    text: String,
    /// Focus is requested exactly once, the frame the rename opens — re-requesting on later
    /// frames would immediately steal focus back whenever the user surrenders it (Enter,
    /// click-away), so the `lost_focus` commit below could never fire.
    focus_requested: bool,
}

fn rename_id() -> egui::Id {
    egui::Id::new("gascii_layers_rename_buf")
}

#[cfg(test)]
fn renaming_row(ui: &Ui, index: usize) -> Option<RenameBuf> {
    ui.ctx()
        .data_mut(|d| d.get_temp::<RenameBuf>(rename_id()))
        .filter(|b| b.index == index)
}

fn begin_rename(ui: &Ui, index: usize, current_name: &str) {
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            rename_id(),
            RenameBuf {
                index,
                text: current_name.to_string(),
                focus_requested: false,
            },
        )
    });
}

fn end_rename(ui: &Ui) {
    ui.ctx().data_mut(|d| d.remove::<RenameBuf>(rename_id()));
}

/// Add/Duplicate/Delete/Move all shift which layer index a given row refers to. None of the
/// buttons that trigger them take keyboard focus away from an open rename `TextEdit`, so a rename
/// left open across one of these must be invalidated explicitly — otherwise its buffer's `index`
/// can point at a different layer than the one the user opened the rename on, and a later commit
/// would rename the wrong row.
fn is_structural_layer_edit(edit: &Edit) -> bool {
    matches!(
        edit,
        Edit::AddLayer { .. } | Edit::RemoveLayer { .. } | Edit::ReorderLayer { .. }
    )
}

/// One layer's row: visibility toggle, name (or its inline rename editor when mid-rename), and a
/// rename button. `renaming` is this row's own rename buffer, if any — read once per frame by
/// `body` rather than per row, since the temp-storage lookup clones the buffer's `String`.
fn row(
    ui: &mut Ui,
    doc: &Document,
    index: usize,
    row_h: f32,
    renaming: Option<RenameBuf>,
    outcome: &mut PanelOutcome,
) {
    let t = theme::current(ui.ctx());
    let active = index == doc.active_layer();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;

        let visible = doc.layer_visible(index);
        let eye = if visible { "\u{25C9}" } else { "\u{25CB}" };
        if widgets::square_button(ui, eye, true, row_h).clicked() {
            if let Some(edit) = toggle_layer_visibility(doc, index) {
                outcome.edits.push(edit);
            }
        }

        if let Some(mut buf) = renaming {
            let avail = (ui.available_width() - row_h - 4.0).max(20.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut buf.text)
                    .desired_width(avail)
                    .font(widgets::mono_id(widgets::size::LABEL)),
            );
            if !buf.focus_requested {
                resp.request_focus();
                buf.focus_requested = true;
            }
            if resp.lost_focus() {
                if let Some(edit) = commit_layer_rename(doc, index, &buf.text) {
                    outcome.edits.push(edit);
                }
                end_rename(ui);
            } else {
                ui.ctx().data_mut(|d| d.insert_temp(rename_id(), buf));
            }
        } else {
            let size = Vec2::new((ui.available_width() - row_h - 4.0).max(20.0), row_h);
            let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
            let (fill, fg) = if active {
                (t.bg_inverse, t.fg_inverse)
            } else if resp.hovered() {
                (t.bg_hover, t.fg_text)
            } else {
                (Color32::TRANSPARENT, t.fg_text)
            };
            ui.painter().rect_filled(rect, 2.0, fill);
            let name = doc.layer_name(index).unwrap_or("?");
            ui.painter().text(
                rect.left_center() + Vec2::new(6.0, 0.0),
                egui::Align2::LEFT_CENTER,
                name,
                widgets::mono_id(widgets::size::LABEL),
                fg,
            );
            if let Some(prop) = active_layer_request(index, doc.active_layer(), resp.clicked()) {
                outcome.properties.push(prop);
            }

            if widgets::square_button(ui, "\u{270E}", true, row_h).clicked() {
                begin_rename(ui, index, name);
            }
        }
    });
}

/// The shared row list + control-row body both chrome variants render. `row_h`/`control_h` are the
/// only geometry deltas between windowed and kiosk, mirroring `gascii-anim::timeline::body`'s own
/// split.
pub(crate) fn body(ui: &mut Ui, doc: &Document, row_h: f32, control_h: f32) -> PanelOutcome {
    let mut outcome = PanelOutcome::default();

    ui.vertical(|ui| {
        widgets::micro_label(ui, "LAYERS");
        ui.add_space(6.0);

        // The list must never starve the control rows below it: an unbounded vertical scroll area
        // fills all remaining panel height, pushing everything after it past the panel's bottom
        // edge. Cap it to what's left after both control rows' worth of space.
        let controls_reserve = 2.0 * control_h + ui.spacing().item_spacing.y + 12.0;
        egui::ScrollArea::vertical()
            .id_salt("gascii_layers_rows")
            .auto_shrink([false, true])
            .max_height((ui.available_height() - controls_reserve).max(row_h))
            .show(ui, |ui| {
                let clip = ui.clip_rect();
                let rename = ui.ctx().data_mut(|d| d.get_temp::<RenameBuf>(rename_id()));
                // Top-of-stack first: the highest layer index is drawn at the top of the list.
                for i in (0..doc.layer_count()).rev() {
                    let renaming = rename.as_ref().filter(|b| b.index == i).cloned();
                    // Rows scrolled out of view keep their exact space but skip widget
                    // construction — except a mid-rename row, whose `TextEdit` must stay live or
                    // its focus (and the lost-focus commit) silently drops while offscreen.
                    let row_top = ui.cursor().min.y;
                    if renaming.is_none() && (row_top > clip.max.y || row_top + row_h < clip.min.y)
                    {
                        ui.allocate_space(Vec2::new(ui.available_width(), row_h));
                        ui.add_space(2.0);
                        continue;
                    }
                    row(ui, doc, i, row_h, renaming, &mut outcome);
                    ui.add_space(2.0);
                }
            });

        // Every control row must fit the panel's inner width — content wider than a right-edge
        // panel slides it off-window (see `panel_frame`), so the buttons are grouped two rows deep
        // with tight spacing (matching `row`'s own) and the reorder pair is icon-only, mirroring
        // the timeline's own bare ◀/▶ move buttons.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if widgets::button(ui, "Add", true, control_h).clicked() {
                match add_layer_after_active(doc) {
                    Ok(edit) => outcome.edits.push(edit),
                    Err(e) => outcome.error = Some(layer_op_error_message("add layer", e)),
                }
            }
            if widgets::button(ui, "Duplicate", true, control_h).clicked() {
                match duplicate_active_layer(doc) {
                    Ok(edit) => outcome.edits.push(edit),
                    Err(e) => outcome.error = Some(layer_op_error_message("duplicate layer", e)),
                }
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let can_delete = doc.layer_count() > 1;
            if widgets::button(ui, "Delete", can_delete, control_h).clicked() && can_delete {
                if let Some(edit) = delete_active_layer(doc) {
                    outcome.edits.push(edit);
                }
            }
            let can_move_up = doc.active_layer() + 1 < doc.layer_count();
            if widgets::square_button(ui, "\u{25B2}", can_move_up, control_h).clicked()
                && can_move_up
            {
                if let Some(edit) = move_active_layer_up(doc) {
                    outcome.edits.push(edit);
                }
            }
            let can_move_down = doc.active_layer() > 0;
            if widgets::square_button(ui, "\u{25BC}", can_move_down, control_h).clicked()
                && can_move_down
            {
                if let Some(edit) = move_active_layer_down(doc) {
                    outcome.edits.push(edit);
                }
            }
        });
    });

    if outcome.edits.iter().any(is_structural_layer_edit) {
        end_rename(ui);
    }

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

    #[test]
    fn layer_op_error_message_covers_every_variant_with_the_given_action_prefix() {
        assert_eq!(
            layer_op_error_message(
                "add layer",
                LayerOpError::TooManyLayers {
                    found: 257,
                    max: 256
                }
            ),
            "add layer: exceeds the 256 maximum"
        );
        assert_eq!(
            layer_op_error_message(
                "add layer",
                LayerOpError::TotalCellBudgetExceeded {
                    total_cells: 1,
                    max: 2
                }
            ),
            "add layer: exceeds the maximum total cell budget"
        );
        assert_eq!(
            layer_op_error_message(
                "add layer",
                LayerOpError::IndexOutOfBounds {
                    index: 9,
                    layer_count: 1
                }
            ),
            "add layer: unexpected error"
        );
        assert_eq!(
            layer_op_error_message("remove layer", LayerOpError::LastLayer),
            "remove layer: unexpected error"
        );
        assert_eq!(
            layer_op_error_message("add layer", LayerOpError::LayerCountDesync),
            "add layer: unexpected error"
        );
    }

    /// Exercised through the exact wrapper the panel's own Add button calls: the newly inserted
    /// layer's index becomes active, not wherever the old cursor shifted to.
    #[test]
    fn add_layer_after_active_makes_the_new_layer_active_when_applied() {
        let mut doc = doc_with_layers(1);
        let mut history = History::new();
        let edit = add_layer_after_active(&doc).unwrap();
        history.apply(&mut doc, edit);
        assert_eq!(doc.layer_count(), 2);
        assert_eq!(
            doc.active_layer(),
            1,
            "the panel's Add button must select the newly inserted layer"
        );
    }

    /// Exercised through the exact wrapper the panel's own Duplicate button calls.
    #[test]
    fn duplicate_active_layer_selects_the_duplicate_when_applied() {
        let mut doc = doc_with_layers(2);
        assert!(doc.set_active_layer(0));
        let mut history = History::new();
        let edit = duplicate_active_layer(&doc).unwrap();
        history.apply(&mut doc, edit);
        assert_eq!(doc.layer_count(), 3);
        assert_eq!(doc.active_layer(), 1, "the panel's Duplicate button must select the duplicate, landing immediately after the source");
    }

    #[test]
    fn delete_active_layer_is_none_at_one_layer_and_some_otherwise() {
        let doc = doc_with_layers(1);
        assert!(
            delete_active_layer(&doc).is_none(),
            "a single-layer document must not produce a delete edit"
        );

        let doc = doc_with_layers(2);
        assert!(delete_active_layer(&doc).is_some());
    }

    /// The index-direction mapping this module's whole doc comment calls out: moving a row *up in
    /// the displayed (top-of-stack-first) list* raises the layer's own index.
    #[test]
    fn move_active_layer_up_raises_the_active_index_when_applied() {
        let mut doc = doc_with_layers(3);
        assert!(doc.set_active_layer(0));
        let mut history = History::new();
        let edit = move_active_layer_up(&doc).unwrap();
        history.apply(&mut doc, edit);
        assert_eq!(
            doc.active_layer(),
            1,
            "Up must raise the index, moving the row up in the top-of-stack-first list"
        );
    }

    /// The mirror-image mapping: moving a row *down in the displayed list* lowers the index.
    #[test]
    fn move_active_layer_down_lowers_the_active_index_when_applied() {
        let mut doc = doc_with_layers(3);
        assert!(doc.set_active_layer(2));
        let mut history = History::new();
        let edit = move_active_layer_down(&doc).unwrap();
        history.apply(&mut doc, edit);
        assert_eq!(
            doc.active_layer(),
            1,
            "Down must lower the index, moving the row down in the top-of-stack-first list"
        );
    }

    #[test]
    fn move_active_layer_up_is_none_at_the_top_index_and_some_otherwise() {
        let mut doc = doc_with_layers(3);
        // `doc_with_layers` leaves the active layer on the top index (adding a layer selects it) —
        // reset to the bottom to exercise the "not yet at the top" case first.
        assert!(doc.set_active_layer(0));
        assert!(
            move_active_layer_up(&doc).is_some(),
            "not yet at the top index"
        );
        assert!(doc.set_active_layer(2));
        assert!(
            move_active_layer_up(&doc).is_none(),
            "already the top index — nothing above it to move past"
        );
    }

    #[test]
    fn move_active_layer_down_is_none_at_index_zero_and_some_otherwise() {
        let mut doc = doc_with_layers(3);
        assert!(doc.set_active_layer(1));
        assert!(move_active_layer_down(&doc).is_some());
        assert!(doc.set_active_layer(0));
        assert!(
            move_active_layer_down(&doc).is_none(),
            "already index 0 — nothing below it to move past"
        );
    }

    #[test]
    fn toggle_layer_visibility_flips_the_current_value_when_applied() {
        let mut doc = doc_with_layers(1);
        assert!(doc.layer_visible(0), "sanity: a fresh layer starts visible");
        let mut history = History::new();
        let edit = toggle_layer_visibility(&doc, 0).unwrap();
        history.apply(&mut doc, edit);
        assert!(!doc.layer_visible(0));

        let edit = toggle_layer_visibility(&doc, 0).unwrap();
        history.apply(&mut doc, edit);
        assert!(doc.layer_visible(0));
    }

    /// Begin renaming row 1, then apply the exact edit `body`'s Delete button produces for the
    /// active layer (index 0) — deleting index 0 shifts what used to be row 1 down to row 0, so
    /// the open rename buffer (still keyed on 1) must be invalidated by the same structural-edit
    /// check `body` runs, or a later Enter could commit the typed text onto the wrong (shifted)
    /// layer.
    #[test]
    fn a_structural_delete_invalidates_a_rename_buffer_pointing_at_a_row_it_could_have_shifted() {
        let mut doc = doc_with_layers(2);
        assert!(doc.set_active_layer(0));
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            begin_rename(ui, 1, "Layer 2");
            assert!(renaming_row(ui, 1).is_some(), "sanity: the rename buffer is open on row 1");

            let delete_edit = delete_active_layer(&doc).expect("two layers: delete must produce an edit");
            assert!(is_structural_layer_edit(&delete_edit), "sanity: Delete is a structural edit");
            if is_structural_layer_edit(&delete_edit) {
                end_rename(ui);
            }

            assert!(
                renaming_row(ui, 1).is_none(),
                "a structural edit must invalidate an open rename buffer, since it may have shifted the buffer's target row"
            );
        });
    }

    #[test]
    fn commit_layer_rename_produces_an_edit_only_when_the_name_actually_changed() {
        let doc = doc_with_layers(1);
        let current = doc.layer_name(0).unwrap().to_string();
        assert!(
            commit_layer_rename(&doc, 0, &current).is_none(),
            "committing the unchanged name must not produce an edit"
        );
        assert!(
            commit_layer_rename(&doc, 0, "Ink").is_some(),
            "a real name change must produce an edit"
        );
    }

    #[test]
    fn is_structural_layer_edit_covers_add_remove_reorder_but_not_visibility_or_rename() {
        let mut doc = doc_with_layers(2);
        assert!(is_structural_layer_edit(
            &add_layer_after_active(&doc).unwrap()
        ));
        assert!(is_structural_layer_edit(
            &duplicate_active_layer(&doc).unwrap()
        ));
        assert!(is_structural_layer_edit(
            &delete_active_layer(&doc).unwrap()
        ));
        assert!(doc.set_active_layer(0));
        assert!(is_structural_layer_edit(
            &move_active_layer_up(&doc).unwrap()
        ));
        assert!(!is_structural_layer_edit(
            &toggle_layer_visibility(&doc, 0).unwrap()
        ));
        assert!(!is_structural_layer_edit(
            &commit_layer_rename(&doc, 0, "Ink").unwrap()
        ));
    }

    #[test]
    fn active_layer_request_requests_the_clicked_row_only_when_it_is_not_already_active() {
        assert_eq!(
            active_layer_request(2, 0, true),
            Some(DocProperty::ActiveLayer(2)),
            "a click on a non-active row must request it"
        );
        assert_eq!(
            active_layer_request(0, 0, true),
            None,
            "a click on the already-active row must request nothing"
        );
        assert_eq!(
            active_layer_request(2, 0, false),
            None,
            "no click must request nothing"
        );
    }

    /// A no-input render must produce a true no-op outcome — proves rendering alone never requests
    /// a mutation, mirroring `gascii-anim::timeline`'s own `body_renders_a_thumbnail_strip_click_
    /// as_a_set_active_frame_request` no-input contract.
    #[test]
    fn body_on_a_no_input_render_requests_nothing() {
        let doc = doc_with_layers(3);
        let ctx = egui::Context::default();
        let mut outcome = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            outcome = Some(body(ui, &doc, 26.0, 24.0));
        });
        let outcome = outcome.unwrap();
        assert!(outcome.edits.is_empty());
        assert!(outcome.properties.is_empty());
        assert!(outcome.error.is_none());
    }

    /// The panel must reserve exactly `PANEL_W` from the parent — no more, no less. Pins the
    /// off-window slide failure: content wider than the panel's inner width (or a frame whose
    /// chrome exceeds the reserved size) makes egui resolve the overflow by anchoring the panel's
    /// right edge past the window, so the central panel painted over its left edge.
    #[test]
    fn show_reserves_exactly_panel_w_from_the_parent() {
        let doc = doc_with_layers(3);
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(1920.0, 1140.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(raw, |ui| {
            let _ = show(ui, &doc);
            let after = ui.available_rect_before_wrap();
            assert_eq!(
                after.max.x,
                1920.0 - PANEL_W,
                "the parent's remaining width must shrink by exactly PANEL_W — anything else means the panel slid or under-claimed"
            );
        });
    }

    /// The control rows must stay inside the panel's height even when the row list could fill it —
    /// pins the starved-controls failure: an unbounded vertical scroll area consumes all remaining
    /// height, pushing both button rows past the panel's bottom edge.
    #[test]
    fn body_keeps_the_control_rows_within_the_given_height_even_with_many_layers() {
        let doc = doc_with_layers(30);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let budget = egui::Vec2::new(198.0, 500.0);
            let used = ui
                .allocate_ui(budget, |ui| {
                    let _ = body(ui, &doc, 26.0, 24.0);
                    ui.min_rect().height()
                })
                .inner;
            assert!(
                used <= budget.y + 0.5,
                "body used {used}px of a {}px height budget — the scroll list must shrink so the control rows stay visible",
                budget.y
            );
        });
    }

    /// `body` must render every layer's row without panicking, including the single-layer
    /// (Delete-disabled) and multi-layer boundary cases.
    #[test]
    fn body_renders_without_panicking_at_one_and_several_layers() {
        for n in [1, 2, 5] {
            let doc = doc_with_layers(n);
            let ctx = egui::Context::default();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = body(ui, &doc, 26.0, 24.0);
            });
        }
    }
}
