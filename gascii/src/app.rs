use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use gascii_core::{
    builtin_pages, export_text, load_str, page_available, save_string, Document, Eraser,
    History, Page, Pencil, PlaneMask, Rgba, TextTool, Tool, ToolEvent, ToolResponse,
};

use crate::canvas::{self, CanvasRenderer, NaiveRenderer};
use crate::fonts;
use crate::viewport::Viewport;

/// ANSI 16-color presets offered as a picking aid alongside the truecolor picker.
const ANSI16: [(&str, Rgba); 16] = [
    ("Black", Rgba(0, 0, 0, 255)),
    ("Red", Rgba(205, 49, 49, 255)),
    ("Green", Rgba(13, 188, 121, 255)),
    ("Yellow", Rgba(229, 229, 16, 255)),
    ("Blue", Rgba(36, 114, 200, 255)),
    ("Magenta", Rgba(188, 63, 188, 255)),
    ("Cyan", Rgba(17, 168, 205, 255)),
    ("White", Rgba(229, 229, 229, 255)),
    ("Bright Black", Rgba(102, 102, 102, 255)),
    ("Bright Red", Rgba(241, 76, 76, 255)),
    ("Bright Green", Rgba(35, 209, 139, 255)),
    ("Bright Yellow", Rgba(245, 245, 67, 255)),
    ("Bright Blue", Rgba(59, 142, 234, 255)),
    ("Bright Magenta", Rgba(214, 112, 214, 255)),
    ("Bright Cyan", Rgba(41, 184, 219, 255)),
    ("Bright White", Rgba(255, 255, 255, 255)),
];

fn color32(c: Rgba) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c.0, c.1, c.2, c.3)
}

/// A clickable color swatch; clicking opens a popup with ANSI-16 presets plus a full truecolor
/// picker. Colors are always stored truecolor — presets are a picking aid, not a constraint.
fn color_swatch_button(ui: &mut egui::Ui, label: &str, color: &mut Rgba) {
    ui.label(label);
    let btn = ui.add(
        egui::Button::new("")
            .fill(color32(*color))
            .min_size(egui::vec2(28.0, 20.0)),
    );
    egui::Popup::from_toggle_button_response(&btn).show(|ui| {
        ui.label("ANSI 16");
        ui.horizontal_wrapped(|ui| {
            for (name, preset) in ANSI16.iter() {
                let resp = ui.add(
                    egui::Button::new("")
                        .fill(color32(*preset))
                        .min_size(egui::vec2(18.0, 16.0)),
                );
                if resp.on_hover_text(*name).clicked() {
                    *color = *preset;
                }
            }
        });
        ui.separator();
        ui.label("Custom");
        let mut arr = [color.0, color.1, color.2, color.3];
        if ui.color_edit_button_srgba_unmultiplied(&mut arr).changed() {
            *color = Rgba(arr[0], arr[1], arr[2], arr[3]);
        }
    });
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ToolKind {
    Pencil,
    Eraser,
    /// Not a `Tool`: it produces no `Edit`, only app-side color state, so it deliberately
    /// doesn't route through the `Tool` trait.
    Eyedropper,
    Text,
}

pub struct GasciiApp {
    pub(crate) doc: Document,
    pub(crate) viewport: Viewport,
    pub(crate) cursor: (u16, u16),
    pub(crate) hovered_cell: Option<(u16, u16)>,
    pub(crate) renderer: Box<dyn CanvasRenderer>,
    pub(crate) pending_fit: bool,
    pub(crate) history: History,
    pub(crate) active_glyph: char,
    pub(crate) active_fg: Rgba,
    pub(crate) active_bg: Rgba,
    pub(crate) mask: PlaneMask,
    pub(crate) tool_kind: ToolKind,
    pub(crate) tool: Box<dyn Tool>,
    pub(crate) stroke_active: bool,
    pub(crate) space_pan_active: bool,
    /// True once `TextTool` has an active click-placed cursor — gates the single-letter
    /// tool-select keys so typing `'p'`/`'e'`/`'i'`/`'t'` while composing text doesn't switch
    /// tools.
    pub(crate) text_editing: bool,
    /// Previous frame's window-focus state, for edge-detecting focus loss.
    pub(crate) was_focused: bool,
    pages: Vec<Page>,
    active_page: usize,
    current_path: Option<PathBuf>,
    last_error: Option<String>,
    started: Instant,
    first_frame: bool,
}

impl GasciiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, started: Instant) -> Self {
        fonts::install_canvas_font(&cc.egui_ctx);
        Self {
            doc: Document::default_document(),
            viewport: Viewport::default(),
            cursor: (0, 0),
            hovered_cell: None,
            renderer: Box::new(NaiveRenderer),
            pending_fit: false,
            history: History::new(),
            active_glyph: '#',
            active_fg: Rgba::WHITE,
            active_bg: Rgba::TRANSPARENT,
            mask: PlaneMask::default(),
            tool_kind: ToolKind::Pencil,
            tool: Box::new(Pencil::new()),
            stroke_active: false,
            space_pan_active: false,
            text_editing: false,
            was_focused: true,
            pages: builtin_pages(),
            active_page: 0,
            current_path: None,
            last_error: None,
            started,
            first_frame: true,
        }
    }

    /// Rebuilds `self.tool` for the new kind. A no-op while a stroke is active: the pointer is
    /// captured by the in-progress gesture, so tool switching is suppressed mid-stroke.
    fn set_tool(&mut self, kind: ToolKind) {
        if self.stroke_active {
            return;
        }
        // Flush whenever we're leaving Text mode's old TextTool behind — including re-selecting
        // Text while already in Text mode, which unconditionally replaces `self.tool` with a
        // brand-new, empty TextTool below. Without this, re-clicking the toolbar's "Text" button
        // mid-sentence would silently discard the pending, uncommitted burst. A no-op flush if
        // nothing is pending (`TextBurst::finish` returns `None` for an empty burst).
        if self.tool_kind == ToolKind::Text {
            self.flush_text_tool();
        }
        self.tool_kind = kind;
        match kind {
            ToolKind::Pencil => self.tool = Box::new(Pencil::new()),
            ToolKind::Eraser => self.tool = Box::new(Eraser::new()),
            // No Tool object needed: canvas.rs branches around `self.tool` entirely in
            // Eyedropper mode (it produces no Edit).
            ToolKind::Eyedropper => {}
            ToolKind::Text => self.tool = Box::new(TextTool::new()),
        }
        self.text_editing = false;
    }

    /// Finalizes a pending text-mode burst into one undo entry. A no-op unless text mode has an
    /// active cursor — called on tool switch, Escape, Undo/Redo, and OS focus loss, so a typing
    /// session is never silently discarded.
    pub(crate) fn flush_text_tool(&mut self) {
        if self.tool_kind != ToolKind::Text {
            return;
        }
        let tctx = crate::canvas::tool_ctx(self);
        if let ToolResponse::Commit(Some(edit)) = self.tool.update(ToolEvent::Commit, &tctx, &self.doc) {
            self.history.apply(&mut self.doc, edit);
        }
        self.text_editing = false;
    }

    /// Commits any pending text burst, then undoes the most recent edit. Flushing before undo is
    /// correct here: it turns "Undo while mid-sentence" into "undo the very edit that was just
    /// typed" (the same edit the flush just committed), matching ordinary editor conventions.
    fn request_undo(&mut self) {
        self.flush_text_tool();
        self.history.undo(&mut self.doc);
    }

    /// Redoes the most recently undone edit. Deliberately does *not* flush a pending text burst
    /// first when a redo is actually available: `History::apply` (which the flush would trigger
    /// via `flush_text_tool`) unconditionally clears the redo stack, so flushing before redo
    /// would empty the very stack this is about to pop from — silently turning every Redo press
    /// mid-sentence into a no-op. Skipping the flush in that case leaves the pending burst
    /// untouched (still composing, not lost — it commits later at the next structural trigger)
    /// and lets the requested redo actually happen. If nothing is available to redo, flushing
    /// anyway is safe and correct: it preserves the "never silently discard typed text" invariant
    /// with no redo left to interfere with.
    ///
    /// A redo applied here mutates `self.doc` directly, bypassing the pending burst entirely — if
    /// the redone edit touches a cell the burst has already pinned a `before` value for, that
    /// pinned value goes stale relative to `doc`'s new actual state. `self.tool.resync` re-pins
    /// every already-touched cell to `doc`'s current value immediately after, so the burst's
    /// eventual flush produces a `before` that matches `doc`'s real pre-flush state, keeping
    /// `History`'s invariant intact.
    fn request_redo(&mut self) {
        if self.history.can_redo() {
            self.history.redo(&mut self.doc);
            let layer = crate::canvas::tool_ctx(self).layer;
            self.tool.resync(&self.doc, layer);
        } else {
            self.flush_text_tool();
        }
    }

    /// Tool-select (`P`/`E`/`I`/`T`) and undo/redo keys. Undo/redo are `Ctrl`-modified chords and
    /// stay global (they won't collide with typing into the color picker's hex field); the
    /// single-letter tool keys are guarded on no widget having focus *and* not being mid-text-edit
    /// so typing into that hex field, or into the canvas in text mode, doesn't get swallowed as a
    /// tool switch.
    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        let focused = ui.memory(|m| m.focused().is_some()) || self.text_editing;
        let (redo_shift, undo, redo_y, pencil, eraser, eyedropper, text) = ui.input_mut(|i| {
            // Cmd/Ctrl+Shift+Z must be consumed before the plain Cmd/Ctrl+Z pattern, since
            // `matches_logically` ignores extra Shift/Alt — checking undo first would swallow
            // the redo shortcut's Z key press.
            let redo_shift = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
            let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            let redo_y = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
            let pencil = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::P);
            let eraser = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::E);
            let eyedropper = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::I);
            let text = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::T);
            (redo_shift, undo, redo_y, pencil, eraser, eyedropper, text)
        });

        if redo_shift || redo_y {
            self.request_redo();
        } else if undo {
            self.request_undo();
        }
        if pencil {
            self.set_tool(ToolKind::Pencil);
        }
        if eraser {
            self.set_tool(ToolKind::Eraser);
        }
        if eyedropper {
            self.set_tool(ToolKind::Eyedropper);
        }
        if text {
            self.set_tool(ToolKind::Text);
        }
    }

    fn palette_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Palette");
        if ui.checkbox(&mut self.doc.settings.strict_ascii, "Strict ASCII").changed()
            && !page_available(&self.pages[self.active_page], &self.doc.settings)
        {
            if let Some(ascii_index) = self.pages.iter().position(|p| p.ascii) {
                self.active_page = ascii_index;
            }
        }
        ui.horizontal_wrapped(|ui| {
            for i in 0..self.pages.len() {
                let name = self.pages[i].name;
                let available = page_available(&self.pages[i], &self.doc.settings);
                ui.add_enabled_ui(available, |ui| {
                    ui.selectable_value(&mut self.active_page, i, name);
                });
            }
        });
        ui.separator();

        let font_id = fonts::canvas_font_id(18.0);
        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let glyph_count = self.pages[self.active_page].glyphs.len();
                for gi in 0..glyph_count {
                    let ch = self.pages[self.active_page].glyphs[gi];
                    let selected = self.active_glyph == ch;
                    let text = egui::RichText::new(ch.to_string()).font(font_id.clone());
                    if ui.selectable_label(selected, text).clicked() {
                        self.active_glyph = ch;
                    }
                }
            });
        });
        ui.separator();

        color_swatch_button(ui, "Text Color", &mut self.active_fg);
        color_swatch_button(ui, "Background", &mut self.active_bg);
        ui.separator();

        ui.label("Write:");
        ui.checkbox(&mut self.mask.glyph, "Glyph");
        ui.checkbox(&mut self.mask.fg, "Text Color");
        ui.checkbox(&mut self.mask.bg, "Background");
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Fit to Window").clicked() {
                self.pending_fit = true;
            }
            ui.separator();

            if ui.selectable_label(self.tool_kind == ToolKind::Pencil, "Pencil (P)").clicked() {
                self.set_tool(ToolKind::Pencil);
            }
            if ui.selectable_label(self.tool_kind == ToolKind::Eraser, "Eraser (E)").clicked() {
                self.set_tool(ToolKind::Eraser);
            }
            if ui
                .selectable_label(self.tool_kind == ToolKind::Eyedropper, "Eyedropper (I)")
                .clicked()
            {
                self.set_tool(ToolKind::Eyedropper);
            }
            if ui.selectable_label(self.tool_kind == ToolKind::Text, "Text (T)").clicked() {
                self.set_tool(ToolKind::Text);
            }
            ui.separator();

            if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo")).clicked() {
                self.request_undo();
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo")).clicked() {
                self.request_redo();
            }
            ui.separator();

            if ui.button("Open").clicked() {
                self.open_file();
            }
            if ui.button("Save").clicked() {
                self.save_file();
            }
            if ui.button("Save As").clicked() {
                self.save_file_as();
            }
            if ui.button("Export Text").clicked() {
                self.export_text_file();
            }
            if ui.button("Copy as Text").clicked() {
                // Flush first: a pending text burst lives only in `self.tool`'s overlay until
                // committed into `self.doc` — copying without flushing would silently drop the
                // just-typed, uncommitted characters from the clipboard contents.
                self.flush_text_tool();
                ui.ctx().copy_text(export_text(&self.doc));
            }
        });
    }

    /// Reads and parses a `.gascii` file picked via a native dialog. A freshly loaded document
    /// starts with an empty undo history — there is no `before` state for its cells prior to the
    /// load.
    fn open_file(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("GASCII", &["gascii"]).pick_file() else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => match load_str(&contents) {
                Ok(doc) => {
                    // Cancel, not flush: the old `self.doc` this burst's `before` values were
                    // pinned against is about to be discarded, so committing into it is pointless
                    // — and carrying the same `TextTool` instance (and `text_editing`) forward
                    // would let it later graft edits, and stale pre-edit `before` values on
                    // Undo, from the discarded document onto the newly loaded one. Only relevant
                    // in Text mode; other tools have no cross-frame pending state to strand.
                    if self.tool_kind == ToolKind::Text {
                        self.tool = Box::new(TextTool::new());
                    }
                    self.text_editing = false;
                    self.doc = doc;
                    self.history = History::new();
                    self.current_path = Some(path);
                    self.last_error = None;
                }
                Err(e) => self.last_error = Some(format!("failed to load {}: {e}", path.display())),
            },
            Err(e) => self.last_error = Some(format!("failed to read {}: {e}", path.display())),
        }
    }

    fn save_file(&mut self) {
        // Flush first: Save reads `self.doc` directly, which does not yet contain a pending text
        // burst's just-typed characters until a commit trigger fires. Also covers the
        // `save_file_as` delegation below (a no-op double-flush if already flushed).
        self.flush_text_tool();
        match self.current_path.clone() {
            Some(path) => self.write_gascii(&path),
            None => self.save_file_as(),
        }
    }

    fn save_file_as(&mut self) {
        // Flush first — see `save_file`'s comment. Also reachable directly via the "Save As"
        // toolbar button, not only through `save_file`'s delegation.
        self.flush_text_tool();
        let Some(path) = rfd::FileDialog::new().add_filter("GASCII", &["gascii"]).save_file() else {
            return;
        };
        self.write_gascii(&path);
    }

    fn write_gascii(&mut self, path: &std::path::Path) {
        match write_atomic(path, save_string(&self.doc).as_bytes()) {
            Ok(()) => {
                self.current_path = Some(path.to_path_buf());
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("failed to save {}: {e}", path.display())),
        }
    }

    /// Exports composited plain text to a file. Does not touch `current_path` — that's reserved
    /// for the native `.gascii` file.
    fn export_text_file(&mut self) {
        // Flush first — see `save_file`'s comment; export reads `self.doc` the same way save does.
        self.flush_text_tool();
        let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).save_file() else {
            return;
        };
        if let Err(e) = std::fs::write(&path, export_text(&self.doc)) {
            self.last_error = Some(format!("failed to export {}: {e}", path.display()));
        } else {
            self.last_error = None;
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let coord = self
                .hovered_cell
                .map(|(x, y)| format!("{x},{y}"))
                .unwrap_or_else(|| "-".to_owned());
            ui.label(format!("cell: {coord}"));
            ui.separator();
            ui.label(format!("zoom: {:.0}%", self.viewport.scale() * 100.0));
            ui.separator();
            ui.label(format!("doc: {}x{}", self.doc.width, self.doc.height));
            if let Some(path) = &self.current_path {
                ui.separator();
                ui.label(format!("file: {}", path.display()));
            }
            if let Some(err) = &self.last_error {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), err);
            }
        });
    }
}

/// Writes `contents` to `path` via write-to-a-sibling-temp-file-then-rename, rather than a direct
/// `std::fs::write`. An interrupted write (disk full, power loss, crash mid-write) to `path`
/// directly can leave a truncated/corrupt file behind, clobbering a previously-good save with no
/// way back; writing to a temp file first and only renaming it into place once the write fully
/// succeeds means `path` either keeps its old contents or gets the new ones, never something
/// in-between. The temp file lives next to `path` (same directory) so the final rename is a
/// same-filesystem move, not a copy.
fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = dir.join(tmp_name);
    std::fs::write(&tmp_path, contents)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

impl eframe::App for GasciiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.first_frame {
            eprintln!("startup to first frame: {:?}", self.started.elapsed());
            self.first_frame = false;
        }
        self.handle_keys(ui);

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::left("palette").show(ui, |ui| self.palette_panel(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            canvas::show(ui, self);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own throwaway directory under the OS temp dir so parallel test runs
    /// (and repeat local runs) never collide or race on the same path.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gascii_write_atomic_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_atomic_creates_a_new_file_with_exact_contents() {
        let dir = scratch_dir("create");
        let path = dir.join("out.gascii");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_overwrites_an_existing_file_and_leaves_no_temp_file_behind() {
        let dir = scratch_dir("overwrite");
        let path = dir.join("out.gascii");
        std::fs::write(&path, b"old contents").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!dir.join("out.gascii.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
