//! The New/Resize/Export/Confirm modal dialogs and the `?` keyboard-shortcuts overlay, plus each
//! dialog's own scratch-state types and the Export pipeline (`run_export`/`run_export_dialog`) that
//! backs the Export dialog's "Export…" button.

use eframe::egui;
use gascii_core::{
    composite, export_text, export_text_frames, export_text_frames_with_trim, resize_document,
    AxisAnchor, Document, History, ResizeAnchor, ResizeError, Rgba,
};

use crate::anim_export;
use crate::chords;
use crate::fonts;
use crate::image_bg;
use crate::png_export;
use crate::ui::dialog::{self, DialogAction};

use super::{tools, GasciiApp, OpenDialog, PendingConfirm, PLUGINS};

/// PNG cell-px per export scale preset: `16 * {1, 2, 4}`.
const EXPORT_CELL_PX_BASE: u32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExportFormat {
    Text,
    Png,
    /// Animated GIF — offered only when `doc.frame_count() > 1`.
    Gif,
    /// PNG spritesheet, auto-tiled roughly square — offered only when `doc.frame_count() > 1`.
    SpriteSheet,
    /// Per-frame text dump, one file, frame-separated — offered only when `doc.frame_count() > 1`.
    TextFrames,
}

/// The Export dialog's remembered settings — persisted per-app (not per-document; `eframe::Storage`
/// has no per-document slot to hang this off without touching the file format).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ExportSettings {
    pub format: ExportFormat,
    /// Cell scale multiplier: 1, 2, or 4 (`cell_px = EXPORT_CELL_PX_BASE * scale`).
    pub scale: u8,
    pub transparent: bool,
    pub trim: bool,
}

impl Default for ExportSettings {
    fn default() -> Self {
        ExportSettings { format: ExportFormat::Text, scale: 1, transparent: true, trim: true }
    }
}

impl ExportSettings {
    pub(crate) fn cell_px(&self) -> u32 {
        EXPORT_CELL_PX_BASE * self.scale as u32
    }
}

/// The export preview's cache key: `ExportSettings` alone isn't enough once a background image can
/// affect the composite, since `ImageBackground` itself isn't `Copy` and can't live in the key
/// directly — `image_gen` (`GasciiApp::image_bg_gen`) stands in for "has the image (or its
/// opacity/gate) changed since this preview was built".
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct ExportPreviewKey {
    settings: ExportSettings,
    pub(super) image_gen: u64,
}

/// The Export dialog's "Trim trailing spaces" *unchecked* path: every row stays padded to
/// `doc.width` glyphs, unlike `export_text`'s trailing-whitespace trim (which stays the default,
/// matching the format's pre-existing behavior).
pub(super) fn export_text_untrimmed(doc: &Document) -> String {
    composite(doc)
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Export dialog's "Trim trailing spaces" *unchecked* path for `ExportFormat::TextFrames` —
/// mirrors `export_text_untrimmed`'s exact asymmetric core/app split (untrimmed variants have
/// always lived app-side); the header/frame-separator format itself is core's, via
/// `export_text_frames_with_trim`.
pub(super) fn export_text_frames_untrimmed(doc: &Document) -> String {
    export_text_frames_with_trim(doc, false)
}

/// Outcome of one [`run_export_dialog`] round trip.
enum ExportOutcome {
    /// The user closed the save dialog without picking a path — `run_export`'s no-op case.
    Cancelled,
    Written,
    /// Carries the message to surface via `last_error`, already phrased for the format that failed.
    Failed(String),
}

/// Shared dialog -> produce-bytes -> atomic-write pipeline behind every `ExportFormat` arm of
/// `run_export`. `produce`'s `Err` is used verbatim as the failure message (each format's own
/// encode-failure wording); a `write_atomic` failure is phrased `"failed to {write_err_verb}
/// {path}: {e}"` — `"export"` for the plain-text formats, `"write"` for the binary ones, matching
/// each format's pre-existing wording. A free function, not a method, so `produce` is free to
/// borrow `self`'s fields without fighting a `&mut self` receiver here.
fn run_export_dialog(
    filter_name: &str,
    extensions: &[&str],
    write_err_verb: &str,
    produce: impl FnOnce() -> Result<Vec<u8>, String>,
) -> ExportOutcome {
    let Some(path) = rfd::FileDialog::new().add_filter(filter_name, extensions).save_file() else {
        return ExportOutcome::Cancelled;
    };
    let bytes = match produce() {
        Ok(bytes) => bytes,
        Err(e) => return ExportOutcome::Failed(e),
    };
    match super::write_atomic(&path, &bytes) {
        Ok(()) => ExportOutcome::Written,
        Err(e) => ExportOutcome::Failed(format!("failed to {write_err_verb} {}: {e}", path.display())),
    }
}

/// A document that dropped to one frame while the dialog was closed (or between opens) must not
/// reopen on a multi-frame-only format that's no longer offered — snaps back to `Text` in that
/// case, a no-op otherwise. Pure, mirroring `export_dialog_formats`'s own testability rationale.
pub(super) fn snap_unavailable_export_format(format: ExportFormat, frame_count: usize) -> ExportFormat {
    if frame_count == 1 && matches!(format, ExportFormat::Gif | ExportFormat::SpriteSheet | ExportFormat::TextFrames) {
        ExportFormat::Text
    } else {
        format
    }
}

/// The Export dialog's offered format list: Text/PNG always, with the three multi-frame formats
/// (Gif/SpriteSheet/TextFrames) appended only when `doc.frame_count() > 1` — a single-frame
/// document's list is byte-identical to what the dialog offered before this format ever existed.
/// Pulled out as a pure function, mirroring `is_own_clipboard_text`/`edit_marker_differs`, so the
/// gating is unit-testable without driving the dialog's own `egui::Context`-backed UI.
pub(super) fn export_dialog_formats(doc: &Document) -> Vec<(ExportFormat, &'static str)> {
    let mut formats = vec![(ExportFormat::Text, "Text (.txt)"), (ExportFormat::Png, "PNG")];
    if doc.frame_count() > 1 {
        formats.push((ExportFormat::Gif, "Animated GIF"));
        formats.push((ExportFormat::SpriteSheet, "PNG Spritesheet"));
        formats.push((ExportFormat::TextFrames, "Text Frames (.txt)"));
    }
    formats
}

/// The Resize dialog's 3x3 anchor picker: nine 24px cells laid out like mini tool-cells (selected
/// inverts), each bound to one `(AxisAnchor, AxisAnchor)` combination. Glyphs read as a compass —
/// arrows toward the edge/corner the anchor pins, a dot at dead center.
fn anchor_grid(ui: &mut egui::Ui, anchor: &mut ResizeAnchor) {
    use eframe::egui::{Align2, Rect, Sense, Vec2};
    const CELL: f32 = 24.0;
    let axes = [AxisAnchor::Start, AxisAnchor::Center, AxisAnchor::End];
    let glyphs = [["↖", "↑", "↗"], ["←", "·", "→"], ["↙", "↓", "↘"]];
    let t = crate::ui::theme::current(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(CELL * 3.0), Sense::hover());
    let painter = ui.painter().clone();
    for (row, &v) in axes.iter().enumerate() {
        for (col, &h) in axes.iter().enumerate() {
            let cell_rect = Rect::from_min_size(
                rect.min + Vec2::new(col as f32 * CELL, row as f32 * CELL),
                Vec2::splat(CELL),
            );
            let selected = anchor.h == h && anchor.v == v;
            let resp = ui.interact(cell_rect, ui.id().with(("anchor", row, col)), Sense::click());
            let (fill, fg) = if selected {
                (t.bg_inverse, t.fg_inverse)
            } else if resp.hovered() {
                (t.bg_hover, t.fg_text)
            } else {
                (eframe::egui::Color32::TRANSPARENT, t.fg_text)
            };
            painter.rect_filled(cell_rect, 0.0, fill);
            painter.rect_stroke(cell_rect, 0.0, eframe::egui::Stroke::new(1.0, t.border_soft), eframe::egui::StrokeKind::Inside);
            painter.text(cell_rect.center(), Align2::CENTER_CENTER, glyphs[row][col], fonts::mono_id(fonts::size::CONTROL), fg);
            if resp.clicked() {
                anchor.h = h;
                anchor.v = v;
            }
        }
    }
    painter.rect_stroke(rect, 0.0, eframe::egui::Stroke::new(1.0, t.border_strong), eframe::egui::StrokeKind::Inside);
}

/// One row of the `?` overlay: a fixed-width key label, then the action it fires.
fn help_overlay_row(ui: &mut egui::Ui, t: &crate::ui::theme::Tokens, key_label: &str, name: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(
            egui::Vec2::new(90.0, 16.0),
            egui::Label::new(egui::RichText::new(key_label).font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_text)),
        );
        ui.label(egui::RichText::new(name).font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_secondary));
    });
}

impl GasciiApp {
    /// The `?` keyboard-shortcuts overlay: every tool's own letter shortcut (`tools()`) plus every
    /// host chord (`chords::chord_rows()`), read-only. Built on the same `dialog::modal` surface as
    /// every other dialog, so it inherits Escape/backdrop-click/close-box dismissal for free and
    /// needs no bespoke Cancel/Confirm row of its own. A plugin-registered `tick`-driven shortcut
    /// (e.g. `gascii-anim`'s `Space`/`,`/`.`/`Shift+D`) has no enforced way to surface a label here
    /// today — see `gascii_plugin_api::Plugin::tick`'s own doc comment for that limitation.
    pub(super) fn help_overlay(&mut self, ctx: &egui::Context) {
        if self.open_dialog != Some(OpenDialog::Help) {
            return;
        }
        let t = crate::ui::theme::current(ctx);
        let resp = dialog::modal(ctx, "help_overlay", "Keyboard Shortcuts", |ui| {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                ui.label(
                    egui::RichText::new("TOOLS").font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_secondary),
                );
                for def in tools() {
                    help_overlay_row(ui, &t, def.key.name(), def.name);
                }
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("COMMANDS").font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_secondary),
                );
                for (name, label) in chords::chord_rows() {
                    help_overlay_row(ui, &t, label, name);
                }
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("PLUGINS").font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_secondary),
                );
                for descriptor in PLUGINS {
                    for shortcut in (descriptor.shortcuts)() {
                        help_overlay_row(ui, &t, shortcut.label, shortcut.name);
                    }
                }
            });
        });
        if resp.dismissed {
            self.open_dialog = None;
        }
    }

    /// New Document dialog: width/height steppers, a preset segment, and a background well.
    pub(super) fn new_dialog(&mut self, ctx: &egui::Context) {
        if self.open_dialog != Some(OpenDialog::New) {
            return;
        }
        #[derive(Clone, Copy, PartialEq)]
        enum Preset {
            Small,
            Large,
            Custom,
        }
        let resp = dialog::modal(ctx, "new_document", "New Document", |ui| {
            let mut preset = if (self.new_w, self.new_h) == (80, 25) {
                Preset::Small
            } else if (self.new_w, self.new_h) == (120, 40) {
                Preset::Large
            } else {
                Preset::Custom
            };
            let opts = [(Preset::Small, "80×25"), (Preset::Large, "120×40"), (Preset::Custom, "Custom")];
            if crate::ui::widgets::segmented(ui, &mut preset, &opts, false) {
                match preset {
                    Preset::Small => (self.new_w, self.new_h) = (80, 25),
                    Preset::Large => (self.new_w, self.new_h) = (120, 40),
                    Preset::Custom => {}
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Width");
                crate::ui::widgets::stepper(ui, &mut self.new_w, 1, Document::MAX_WIDTH, crate::ui::widgets::STEPPER_H);
                ui.add_space(12.0);
                ui.label("Height");
                crate::ui::widgets::stepper(ui, &mut self.new_h, 1, Document::MAX_HEIGHT, crate::ui::widgets::STEPPER_H);
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Background");
                let mut arr = [self.new_bg.0, self.new_bg.1, self.new_bg.2, self.new_bg.3];
                if ui.color_edit_button_srgba_unmultiplied(&mut arr).changed() {
                    self.new_bg = Rgba(arr[0], arr[1], arr[2], arr[3]);
                }
            });
            ui.add_space(12.0);
            dialog::buttons(ui, "Cancel", "Create")
        });
        match resp.inner {
            DialogAction::Confirm => self.create_new_document(),
            DialogAction::Cancel => self.open_dialog = None,
            DialogAction::None => {
                if resp.dismissed {
                    self.open_dialog = None;
                }
            }
        }
    }

    /// Resize dialog, rebuilt on the shared modal framework: W/H steppers, a 9-way anchor grid,
    /// and the same `resize_document` confirm path as before (now anchor-aware).
    pub(super) fn resize_dialog(&mut self, ctx: &egui::Context) {
        if self.open_dialog != Some(OpenDialog::Resize) {
            return;
        }
        let resp = dialog::modal(ctx, "resize_canvas", "Resize Canvas", |ui| {
            ui.label(format!("current: {}×{}", self.doc.width, self.doc.height));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Width");
                crate::ui::widgets::stepper(ui, &mut self.resize_w, 1, Document::MAX_WIDTH, crate::ui::widgets::STEPPER_H);
                ui.add_space(12.0);
                ui.label("Height");
                crate::ui::widgets::stepper(ui, &mut self.resize_h, 1, Document::MAX_HEIGHT, crate::ui::widgets::STEPPER_H);
            });
            ui.add_space(8.0);
            anchor_grid(ui, &mut self.resize_anchor);
            let t = crate::ui::theme::current(ui.ctx());
            ui.label(
                egui::RichText::new("Existing art keeps this position; new cells fill with background.")
                    .font(fonts::mono_id(fonts::size::LABEL))
                    .color(t.fg_secondary),
            );
            if let Some(err) = &self.last_error {
                ui.label(egui::RichText::new(err.clone()).color(t.fg_error));
            }
            ui.add_space(12.0);
            dialog::buttons(ui, "Cancel", "Resize")
        });
        match resp.inner {
            DialogAction::Confirm => {
                // Resize reads/replaces self.doc directly — flush any pending burst/float
                // into the pre-resize document first, same trigger-table discipline as
                // Save/Export/Copy.
                self.flush_all();
                match resize_document(&self.doc, self.resize_w, self.resize_h, self.resize_anchor) {
                    Ok(Some(edit)) => {
                        self.apply_edit(edit, None);
                        self.last_error = None;
                        self.open_dialog = None;
                    }
                    Ok(None) => self.open_dialog = None, // same extent: silent close
                    Err(ResizeError::ZeroExtent) => {
                        self.last_error = Some("resize: width and height must be at least 1".to_string());
                    }
                    Err(ResizeError::TooLarge { max_width, max_height, .. }) => {
                        self.last_error =
                            Some(format!("resize: exceeds the {max_width}x{max_height} maximum"));
                    }
                    Err(ResizeError::TotalCellBudgetExceeded { .. }) => {
                        self.last_error = Some("resize: exceeds the maximum total cell budget".to_string());
                    }
                }
            }
            DialogAction::Cancel => self.open_dialog = None,
            DialogAction::None => {
                if resp.dismissed {
                    self.open_dialog = None;
                }
            }
        }
    }

    /// Rebuilds `self.export_preview` from the current document + export settings, if it isn't
    /// already current. Dropped (not just left stale) whenever the dialog is closed, so the
    /// texture's GPU memory isn't held open between uses.
    pub(super) fn refresh_export_preview(&mut self, ctx: &egui::Context) {
        if !matches!(self.export.format, ExportFormat::Png | ExportFormat::Gif | ExportFormat::SpriteSheet) {
            self.export_preview = None;
            self.export_preview_key = None;
            return;
        }
        let key = ExportPreviewKey { settings: self.export, image_gen: self.image_bg_gen };
        if self.export_preview_key == Some(key) {
            return;
        }
        let opaque_bg = (!self.export.transparent).then_some(self.doc.background);
        let bg_image = self.image_bg.as_ref().filter(|b| b.use_in_export).map(|b| (&b.pixels, b.export_opacity));
        // A small, fixed preview scale — independent of the export's own cell_px, which can be up
        // to 4x the base and would make an oversized in-dialog thumbnail.
        if let Ok((w, h, pixels)) = png_export::rasterize_rgba8(&self.doc, 4, opaque_bg, bg_image) {
            let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
            self.export_preview =
                Some(ctx.load_texture("export_preview", image, egui::TextureOptions::NEAREST));
        }
        self.export_preview_key = Some(key);
    }

    /// Unified Export dialog: Text/PNG/(multi-frame docs only: GIF/Spritesheet/Text Frames)
    /// format, PNG/GIF/Spritesheet scale + transparency, Text/Text Frames trim, a live preview, and
    /// a pixel/char readout.
    pub(super) fn export_dialog(&mut self, ctx: &egui::Context) {
        if self.open_dialog != Some(OpenDialog::Export) {
            return;
        }
        self.export.format = snap_unavailable_export_format(self.export.format, self.doc.frame_count());
        self.refresh_export_preview(ctx);
        let doc = &self.doc;
        let preview = self.export_preview.clone();
        enum BgAction {
            None,
            Load,
            Clear,
        }
        let mut bg_action = BgAction::None;
        let resp = dialog::modal(ctx, "export", "Export", |ui| {
            let formats = export_dialog_formats(doc);
            crate::ui::widgets::segmented(ui, &mut self.export.format, &formats, false);
            ui.add_space(8.0);

            match self.export.format {
                ExportFormat::Png | ExportFormat::Gif | ExportFormat::SpriteSheet => {
                    ui.horizontal(|ui| {
                        ui.label("Scale");
                        let scales = [(1u8, "1×"), (2, "2×"), (4, "4×")];
                        crate::ui::widgets::segmented(ui, &mut self.export.scale, &scales, false);
                    });
                    ui.add_space(6.0);
                    crate::ui::widgets::checkbox(ui, &mut self.export.transparent, "Transparent background");
                    ui.add_space(10.0);

                    // Background image: the same loaded ImageBackground the TRACE section uses,
                    // composited beneath the art in the exported PNG (Cover fit — fills the frame,
                    // crops the overflow). Load…/Clear here also make the image available as a
                    // trace, and vice versa — one shared image, two independent opacities/gates.
                    let bg_theme = crate::ui::theme::current(ui.ctx());
                    ui.label(
                        egui::RichText::new("Background image")
                            .font(fonts::mono_id(fonts::size::LABEL))
                            .color(bg_theme.fg_secondary),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if crate::ui::widgets::button(ui, "Load…", false, true).clicked() {
                            bg_action = BgAction::Load;
                        }
                        if crate::ui::widgets::button(ui, "Clear", false, self.image_bg.is_some()).clicked() {
                            bg_action = BgAction::Clear;
                        }
                    });
                    if let Some(bg) = self.image_bg.as_mut() {
                        let mut changed =
                            crate::ui::widgets::checkbox(ui, &mut bg.use_in_export, "Use as background");
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let slider = ui.add_sized(
                                egui::Vec2::new(100.0, 20.0),
                                egui::Slider::new(&mut bg.export_opacity, 0.0..=1.0).show_value(false),
                            );
                            // Not `slider.changed()` alone: every bump invalidates
                            // `ExportPreviewKey`, which re-rasterizes the whole document and
                            // re-uploads a texture — per mid-drag frame, a reproducible stutter
                            // on large documents. The preview refreshes when the drag ends; a
                            // discrete click/keyboard change refreshes immediately; the % readout
                            // tracks live either way.
                            changed |= slider.drag_stopped() || (slider.changed() && !slider.dragged());
                            ui.label(
                                egui::RichText::new(format!("{:.0}%", bg.export_opacity * 100.0))
                                    .font(fonts::mono_id(fonts::size::LABEL))
                                    .color(bg_theme.fg_secondary),
                            );
                        });
                        if changed {
                            self.image_bg_gen += 1;
                        }
                    }
                }
                ExportFormat::Text | ExportFormat::TextFrames => {
                    crate::ui::widgets::checkbox(ui, &mut self.export.trim, "Trim trailing spaces");
                }
            }
            ui.add_space(10.0);

            let (preview_rect, _) =
                ui.allocate_exact_size(egui::Vec2::new(ui.available_width(), 120.0), egui::Sense::hover());
            let t = crate::ui::theme::current(ui.ctx());
            ui.painter().rect_filled(preview_rect, 0.0, t.bg_chrome);
            ui.painter().rect_stroke(preview_rect, 0.0, egui::Stroke::new(1.0, t.border_soft), egui::StrokeKind::Inside);
            match self.export.format {
                // Gif/SpriteSheet reuse the same active-frame raster preview PNG builds — a
                // deliberate simplification, not an oversight: no live GIF playback or tiled
                // spritesheet layout is rendered in the dialog, only the active frame's still.
                ExportFormat::Png | ExportFormat::Gif | ExportFormat::SpriteSheet => {
                    if let Some(tex) = &preview {
                        let size = tex.size_vec2();
                        let fit = (size * (preview_rect.size() / size).min_elem()).min(size);
                        let img_rect = egui::Rect::from_center_size(preview_rect.center(), fit);
                        ui.painter().image(
                            tex.id(),
                            img_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                }
                ExportFormat::Text | ExportFormat::TextFrames => {
                    let text = if self.export.format == ExportFormat::Text { export_text(doc) } else { export_text_frames(doc) };
                    let preview_text: String = text.lines().take(6).collect::<Vec<_>>().join("\n");
                    ui.painter().text(
                        preview_rect.left_top() + egui::Vec2::new(6.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        preview_text,
                        crate::fonts::canvas_font_id(fonts::size::CAPTION),
                        t.fg_text,
                    );
                }
            }

            ui.add_space(6.0);
            let readout = match self.export.format {
                ExportFormat::Png | ExportFormat::Gif | ExportFormat::SpriteSheet => {
                    let px = self.export.cell_px();
                    format!(
                        "{}×{} px · {}× cell scale",
                        doc.width as u32 * px,
                        doc.height as u32 * px,
                        self.export.scale
                    )
                }
                ExportFormat::Text => format!("{}×{} chars", doc.width, doc.height),
                ExportFormat::TextFrames => {
                    format!("{}×{} chars × {} frames", doc.width, doc.height, doc.frame_count())
                }
            };
            ui.label(egui::RichText::new(readout).font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_secondary));

            if let Some(err) = &self.last_error {
                ui.label(egui::RichText::new(err.clone()).color(t.fg_error));
            }

            ui.add_space(12.0);
            dialog::buttons(ui, "Cancel", "Export…")
        });
        match bg_action {
            BgAction::Load => self.load_trace_image(ctx),
            BgAction::Clear => self.clear_image_bg(),
            BgAction::None => {}
        }
        match resp.inner {
            DialogAction::Confirm => self.run_export(),
            DialogAction::Cancel => self.close_export_dialog(),
            DialogAction::None => {
                if resp.dismissed {
                    self.close_export_dialog();
                }
            }
        }
    }

    fn close_export_dialog(&mut self) {
        self.open_dialog = None;
        self.export_preview = None;
        self.export_preview_key = None;
    }

    /// Opens a native picker filtered to png/jpg/jpeg, decodes the chosen file, and uploads it as
    /// the (single) image background — replacing whatever was loaded before. A failed pick is a
    /// silent no-op (matches `open_file`); a failed read/decode is non-fatal (`last_error`, current
    /// image left unchanged), never a panic.
    pub(crate) fn load_trace_image(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg"]).pick_file() else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.last_error = Some(format!("failed to load image: {e}"));
                return;
            }
        };
        match image_bg::decode_image(&bytes) {
            Ok(rgba) => {
                let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                let color_image = egui::ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
                let texture = ctx.load_texture("trace_bg", color_image, egui::TextureOptions::LINEAR);
                self.image_bg = Some(image_bg::ImageBackground::new(rgba, Some(texture), Some(path)));
                self.image_bg_gen += 1;
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("failed to load image: {e}")),
        }
    }

    /// Drops the loaded image background entirely, freeing its texture's GPU memory.
    pub(crate) fn clear_image_bg(&mut self) {
        self.image_bg = None;
        self.image_bg_gen += 1;
    }

    /// Flushes, opens a native save dialog filtered by the current format, and writes the result.
    /// Reads `self.doc` directly, so it re-flushes even though the dialog-open path already did —
    /// the dialog stays open across frames and its own "Export…" click is the read that matters.
    fn run_export(&mut self) {
        self.flush_all();
        // Shared raster inputs for the three image-based formats below (Png/Gif/SpriteSheet); each
        // format's own export fn rasterizes from these exactly once per call.
        let opaque_bg = (!self.export.transparent).then_some(self.doc.background);
        let bg_image = self.image_bg.as_ref().filter(|b| b.use_in_export).map(|b| (&b.pixels, b.export_opacity));
        let outcome = match self.export.format {
            ExportFormat::Text => {
                let trim = self.export.trim;
                run_export_dialog("Text", &["txt"], "export", || {
                    let text = if trim { export_text(&self.doc) } else { export_text_untrimmed(&self.doc) };
                    Ok(text.into_bytes())
                })
            }
            ExportFormat::Png => run_export_dialog("PNG", &["png"], "write", || {
                png_export::export_png(&self.doc, self.export.cell_px(), opaque_bg, bg_image)
                    .map_err(|e| format!("PNG export failed: {e}"))
            }),
            ExportFormat::Gif => run_export_dialog("GIF", &["gif"], "write", || {
                anim_export::export_gif(&self.doc, self.export.cell_px(), opaque_bg, bg_image)
                    .map_err(|e| format!("GIF export failed: {e}"))
            }),
            ExportFormat::SpriteSheet => run_export_dialog("PNG", &["png"], "write", || {
                anim_export::export_spritesheet(&self.doc, self.export.cell_px(), opaque_bg, bg_image)
                    .map_err(|e| format!("spritesheet export failed: {e}"))
            }),
            ExportFormat::TextFrames => {
                let trim = self.export.trim;
                run_export_dialog("Text", &["txt"], "export", || {
                    let text = if trim { export_text_frames(&self.doc) } else { export_text_frames_untrimmed(&self.doc) };
                    Ok(text.into_bytes())
                })
            }
        };
        match outcome {
            ExportOutcome::Cancelled => {}
            ExportOutcome::Written => {
                self.last_error = None;
                self.close_export_dialog();
            }
            ExportOutcome::Failed(e) => self.last_error = Some(e),
        }
    }

    /// Resets New-dialog state to defaults and opens it. Shared by File ▸ New…'s clean path and the
    /// confirm dialog's `NewDocument` resolution.
    pub(super) fn open_new_dialog(&mut self) {
        self.new_w = Document::DEFAULT_WIDTH;
        self.new_h = Document::DEFAULT_HEIGHT;
        self.new_bg = Rgba(0, 0, 0, 255);
        self.open_dialog = Some(OpenDialog::New);
    }

    /// Creates a fresh document from the New dialog's current settings, discarding the old one
    /// (the confirm flow above is what makes that safe to do unconditionally here).
    pub(super) fn create_new_document(&mut self) {
        self.reset_cross_frame_tool();
        self.doc = Document::new(self.new_w, self.new_h);
        self.doc.background = self.new_bg;
        self.history = History::new();
        self.saved_marker = self.history.top_edit_id();
        self.saved_loop_playback = self.doc.loop_playback;
        self.saved_frame_duration_ms = self.doc.frame_duration_ms;
        self.current_path = None;
        self.pending_fit = true;
        self.open_dialog = None;
    }

    /// The Save/Don't Save/Cancel modal shown while `self.confirm` is set. `canvas.rs` and
    /// `handle_keys` are both gated off while any modal is open (`modal_open()`) — this is the only
    /// place a decision here (discarding unsaved work) is irreversible.
    pub(super) fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(target) = self.confirm else { return };
        let resp = dialog::modal(ctx, "confirm_unsaved", "Unsaved Changes", |ui| {
            ui.label("This document has unsaved changes.");
            ui.add_space(12.0);
            let mut dont_save = false;
            let mut decided = DialogAction::None;
            ui.horizontal(|ui| {
                if crate::ui::widgets::button(ui, "Don't Save", false, true).clicked() {
                    dont_save = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    decided = dialog::buttons(ui, "Cancel", "Save");
                });
            });
            (dont_save, decided)
        });

        let (dont_save, decided) = resp.inner;
        if dont_save {
            match target {
                PendingConfirm::CloseApp => self.close_now(ctx),
                PendingConfirm::NewDocument => {
                    self.confirm = None;
                    self.open_new_dialog(); // the current doc's fate is settled; now pick the new one's W/H/bg
                }
            }
        } else if decided == DialogAction::Confirm {
            self.save_file();
            // `save_file` leaves last_error/saved_marker untouched on cancel or failure —
            // is_dirty() staying true after the call *is* the "didn't actually save" signal, no
            // separate success/failure plumbing needed.
            if !self.is_dirty() {
                match target {
                    PendingConfirm::CloseApp => self.close_now(ctx),
                    PendingConfirm::NewDocument => {
                        self.confirm = None;
                        self.open_new_dialog();
                    }
                }
            }
        } else if decided == DialogAction::Cancel || resp.dismissed {
            self.confirm = None;
        }
    }
}
