use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use gascii_core::{
    builtin_pages, builtin_ramps, export_text, load_str, resize_document,
    save_string, BrushShape, Buildup, CellPatch, DensityBrush, DensityMode, Document,
    Eraser, Fixed, FloodFill, History, Line, Page, Pencil, PlaneMask, Ramp, Rectangle, ResizeError,
    Rgba, SelectionTool, TextTool, Tool, ToolEvent, ToolResponse, WidthReject, MAX_TOOL_SIZE,
};

use crate::canvas::{self, CanvasRenderer, NaiveRenderer};
use crate::fonts;
use crate::png_export;
use crate::viewport::Viewport;

/// PNG export cell-scale presets offered to the user, in pixels per cell.
const PNG_SCALE_PRESETS: [u32; 5] = [8, 16, 24, 32, 48];

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

/// Whether a pasted `Event::Paste` text is still the app's own copy: the OS clipboard is "ours"
/// exactly when `internal`'s own flattening still matches what came back on paste. Pulled out of
/// `paste_text` as a pure function so the copy/paste reconciliation decision is unit-testable
/// without constructing a full `GasciiApp`.
fn is_own_clipboard_text(text: &str, internal: Option<&CellPatch>) -> bool {
    internal.is_some_and(|p| p.to_text() == text)
}

/// Size drag-value + Square/Circle picker for one `StampSettings`. Appears twice in the tool row
/// (active tool and right-click tool), hence the `id_salt` scope.
fn stamp_controls(ui: &mut egui::Ui, stamp: &mut StampSettings, id_salt: &str, hover: &str) {
    ui.push_id(id_salt, |ui| {
        ui.label("Size");
        ui.add(egui::DragValue::new(&mut stamp.size).range(1..=MAX_TOOL_SIZE))
            .on_hover_text(hover);
        ui.selectable_value(&mut stamp.shape, BrushShape::Square, "Square");
        ui.selectable_value(&mut stamp.shape, BrushShape::Circle, "Circle");
    });
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
    Fill,
    Rectangle,
    Line,
    Selection,
    Brush,
}

/// Footprint settings one sized tool remembers: its stamp width and shape. Every sized tool —
/// and every right-click tool option — keeps its own copy, so switching tools never drags a
/// surprising size along.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StampSettings {
    pub size: u16,
    pub shape: BrushShape,
}

impl Default for StampSettings {
    fn default() -> Self {
        StampSettings { size: 1, shape: BrushShape::Square }
    }
}

/// Slot in `GasciiApp::tool_stamps` for a sized tool; `None` for tools without a footprint.
pub(crate) fn sized_slot(kind: ToolKind) -> Option<usize> {
    match kind {
        ToolKind::Pencil => Some(0),
        ToolKind::Eraser => Some(1),
        ToolKind::Line => Some(2),
        ToolKind::Brush => Some(3),
        _ => None,
    }
}

/// Number of sized tools — `tool_stamps`' length.
pub(crate) const SIZED_TOOL_COUNT: usize = 4;

/// Tools whose stamp obeys the size/shape footprint controls.
pub(crate) fn tool_is_sized(kind: ToolKind) -> bool {
    sized_slot(kind).is_some()
}

/// Tools that get a hover marker previewing exactly which cell(s) their next application lands
/// on. Selection is excluded — its press starts a marquee/move gesture, not a cell stamp, and a
/// stamp-shaped marker would promise the wrong semantics.
pub(crate) fn tool_shows_hover(kind: ToolKind) -> bool {
    !matches!(kind, ToolKind::Selection)
}

/// One entry of `STROKE_TOOLS`: kind, display name, and constructor in a single row, so the
/// three can never drift apart.
pub(crate) struct StrokeTool {
    pub kind: ToolKind,
    pub name: &'static str,
    pub make: fn() -> Box<dyn Tool>,
}

/// The plain pointer-stroke tools — the single source of truth for their names and constructors,
/// used by `set_tool` and by the right-click selector (index 0, Eraser, is the right-click
/// default). All qualify for right-click driving because their `Press` never returns an edit, so
/// the generic press/drag/release lifecycle fits every one of them. Text/Selection/Eyedropper
/// need bespoke routing and stay primary-button-only.
pub(crate) const STROKE_TOOLS: [StrokeTool; 6] = [
    StrokeTool { kind: ToolKind::Eraser, name: "Eraser", make: || Box::new(Eraser::new()) },
    StrokeTool { kind: ToolKind::Pencil, name: "Pencil", make: || Box::new(Pencil::new()) },
    StrokeTool { kind: ToolKind::Fill, name: "Fill", make: || Box::new(FloodFill::new()) },
    StrokeTool { kind: ToolKind::Line, name: "Line", make: || Box::new(Line::new()) },
    StrokeTool { kind: ToolKind::Rectangle, name: "Rectangle", make: || Box::new(Rectangle::new()) },
    StrokeTool { kind: ToolKind::Brush, name: "Brush", make: || Box::new(DensityBrush::new()) },
];

pub struct GasciiApp {
    pub(crate) doc: Document,
    pub(crate) viewport: Viewport,
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
    /// Per-tool footprint settings for the sized primary tools, indexed by `sized_slot` — each
    /// tool remembers its own size/shape across switches.
    pub(crate) tool_stamps: [StampSettings; SIZED_TOOL_COUNT],
    /// Per-option footprint settings for the right-click tool, indexed like `right_click_tool`.
    /// Fully independent of `tool_stamps`: sizing the right-click Eraser never resizes the
    /// primary Eraser, and vice versa.
    pub(crate) rc_stamps: [StampSettings; STROKE_TOOLS.len()],
    /// Index into `STROKE_TOOLS`: what a secondary-button stroke draws with. An index (not a
    /// `ToolKind`) so a non-stroke kind is unrepresentable here by construction.
    pub(crate) right_click_tool: usize,
    /// The transient tool instance a secondary-button stroke drives, `Some` only while that
    /// stroke is in flight. `self.tool` (and any pending text burst or floating selection) stays
    /// untouched underneath it.
    pub(crate) rc_tool: Option<Box<dyn Tool>>,
    pub(crate) stroke_active: bool,
    pub(crate) space_pan_active: bool,
    /// True once `TextTool` has an active click-placed cursor — gates every single-letter
    /// tool-select key so typing while composing text doesn't switch tools.
    pub(crate) text_editing: bool,
    /// Previous frame's window-focus state, for edge-detecting focus loss.
    pub(crate) was_focused: bool,
    /// The last region copied via Ctrl+C, kept alongside the plain text written to the OS
    /// clipboard. A paste whose `Event::Paste` text still matches this patch's own flattening
    /// pastes the colored version; otherwise it's treated as external plain text.
    pub(crate) internal_clipboard: Option<CellPatch>,
    pages: Vec<Page>,
    active_page: usize,
    /// Built-in Ramps, populated at startup — the density brush's glyph sources.
    pub(crate) ramps: Vec<Ramp>,
    /// Index into `ramps`: the brush's currently active ramp.
    pub(crate) active_ramp: usize,
    /// The brush's active intensity source (Fixed level or Buildup).
    pub(crate) density_mode: DensityMode,
    resize_dialog_open: bool,
    resize_w: u16,
    resize_h: u16,
    png_dialog_open: bool,
    png_cell_px: u32,
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
            tool_stamps: [StampSettings::default(); SIZED_TOOL_COUNT],
            rc_stamps: [StampSettings::default(); STROKE_TOOLS.len()],
            right_click_tool: 0, // STROKE_TOOLS[0]: Eraser
            rc_tool: None,
            stroke_active: false,
            space_pan_active: false,
            text_editing: false,
            was_focused: true,
            internal_clipboard: None,
            pages: builtin_pages(),
            active_page: 0,
            ramps: builtin_ramps(),
            active_ramp: 0,
            density_mode: DensityMode::Fixed(Fixed(1.0)),
            resize_dialog_open: false,
            resize_w: Document::DEFAULT_WIDTH,
            resize_h: Document::DEFAULT_HEIGHT,
            png_dialog_open: false,
            png_cell_px: PNG_SCALE_PRESETS[1], // 16px/cell default
            current_path: None,
            last_error: None,
            started,
            first_frame: true,
        }
    }

    /// Whether any pointer gesture — primary stroke or right-click stroke — currently owns the
    /// canvas.
    pub(crate) fn stroke_in_progress(&self) -> bool {
        self.stroke_active || self.rc_tool.is_some()
    }

    /// The `STROKE_TOOLS` entry the right-click gesture draws with. The min-clamp is pure
    /// robustness — only the selector combo writes the index, always in range.
    pub(crate) fn right_click_entry(&self) -> &'static StrokeTool {
        &STROKE_TOOLS[self.rc_index()]
    }

    fn rc_index(&self) -> usize {
        self.right_click_tool.min(STROKE_TOOLS.len() - 1)
    }

    /// The active tool's own footprint settings (the identity default for unsized tools).
    pub(crate) fn active_stamp(&self) -> StampSettings {
        sized_slot(self.tool_kind).map(|i| self.tool_stamps[i]).unwrap_or_default()
    }

    /// The right-click tool's own footprint settings — independent of the primary tools'.
    pub(crate) fn rc_stamp(&self) -> StampSettings {
        self.rc_stamps[self.rc_index()]
    }

    /// Rebuilds `self.tool` for the new kind. A no-op while a stroke is active: the pointer is
    /// captured by the in-progress gesture, so tool switching is suppressed mid-stroke.
    fn set_tool(&mut self, kind: ToolKind) {
        if self.stroke_in_progress() {
            return;
        }
        // Flush whenever we're leaving a cross-frame tool's (Text or Selection) session behind —
        // including re-selecting the same tool while already active, which unconditionally
        // replaces `self.tool` with a brand-new instance below. Without this, re-clicking the
        // toolbar's "Text"/"Selection" button mid-session would silently discard the pending,
        // uncommitted burst or float. A no-op flush if nothing is pending.
        if matches!(self.tool_kind, ToolKind::Text | ToolKind::Selection) {
            self.flush_active_tool();
        }
        self.tool_kind = kind;
        match kind {
            // No Tool object needed: canvas.rs branches around `self.tool` entirely in
            // Eyedropper mode (it produces no Edit).
            ToolKind::Eyedropper => {}
            ToolKind::Text => self.tool = Box::new(TextTool::new()),
            ToolKind::Selection => self.tool = Box::new(SelectionTool::new()),
            // Every remaining kind is a stroke tool; `stroke_tools_cover_every_stroke_kind`
            // pins that the lookup can't miss.
            _ => {
                if let Some(entry) = STROKE_TOOLS.iter().find(|e| e.kind == kind) {
                    self.tool = (entry.make)();
                }
            }
        }
        self.text_editing = false;
    }

    /// Surfaces a rejected typed character in the status bar. The rejection itself already
    /// happens inside the tool's entry validation — this is only the visible-warning half.
    pub(crate) fn warn_rejected_char(&mut self, ch: char, reject: WidthReject) {
        let why = match reject {
            WidthReject::Control => "control character",
            WidthReject::ZeroWidth => "zero-width character",
            WidthReject::DoubleWidth => "wider than one cell",
        };
        self.last_error = Some(format!("typed {ch:?} rejected: {why}"));
    }

    /// Finalizes whatever the active cross-frame tool (Text's burst, Selection's float) has
    /// pending into one undo entry. A no-op for every other tool kind — called on tool switch,
    /// Escape, Undo/Redo, save/export/copy, and OS focus loss, so a typing session or a floating
    /// stamp is never silently discarded.
    pub(crate) fn flush_active_tool(&mut self) {
        if !matches!(self.tool_kind, ToolKind::Text | ToolKind::Selection) {
            return;
        }
        let tctx = crate::canvas::tool_ctx(self);
        if let ToolResponse::Commit(Some(edit)) = self.tool.update(ToolEvent::Commit, &tctx, &self.doc) {
            self.history.apply(&mut self.doc, edit);
        }
        self.text_editing = false;
    }

    /// Commits any pending text burst or floating selection, then undoes the most recent edit.
    /// Flushing before undo is correct here: it turns "Undo mid-session" into "undo the very edit
    /// that was just committed" (the same edit the flush just committed), matching ordinary
    /// editor conventions.
    fn request_undo(&mut self) {
        self.flush_active_tool();
        self.history.undo(&mut self.doc);
    }

    /// Redoes the most recently undone edit. Deliberately does *not* flush a pending text burst or
    /// floating selection first when a redo is actually available: `History::apply` (which the
    /// flush would trigger via `flush_active_tool`) unconditionally clears the redo stack, so
    /// flushing before redo would empty the very stack this is about to pop from — silently
    /// turning every Redo press mid-session into a no-op. Skipping the flush in that case leaves
    /// the pending burst/float untouched (still active, not lost — it commits later at the next
    /// structural trigger) and lets the requested redo actually happen. If nothing is available to
    /// redo, flushing anyway is safe and correct: it preserves the "never silently discard
    /// in-progress work" invariant with no redo left to interfere with.
    ///
    /// A redo applied here mutates `self.doc` directly, bypassing the pending tool entirely — for
    /// `TextTool`, if the redone edit touches a cell the burst has already pinned a `before` value
    /// for, that pinned value goes stale relative to `doc`'s new actual state; `self.tool.resync`
    /// re-pins it. `SelectionTool` inherits the trait's default no-op `resync` — its drop reads
    /// `before` from the document at drop time, not lift time, so there is nothing to re-pin.
    fn request_redo(&mut self) {
        if self.history.can_redo() {
            self.history.redo(&mut self.doc);
            let layer = crate::canvas::tool_ctx(self).layer;
            self.tool.resync(&self.doc, layer);
        } else {
            self.flush_active_tool();
        }
    }

    /// Copies the active selection's cells to both the OS clipboard (plain text) and the app's
    /// colored internal clipboard. A no-op unless Selection is the active tool with a region
    /// defined — the "Copy as Text" toolbar button remains the way to copy the whole document.
    pub(crate) fn copy_selection(&mut self, ctx: &egui::Context) {
        if self.tool_kind != ToolKind::Selection {
            return;
        }
        // A dropped float's cells must be in `self.doc` before capturing the region.
        self.flush_active_tool();
        let Some(rect) = self.tool.selection_overlay().and_then(|v| v.marquee) else {
            return;
        };
        let patch = CellPatch::from_region(&self.doc, rect, 0);
        ctx.copy_text(patch.to_text());
        self.internal_clipboard = Some(patch);
    }

    /// Reconciles a pasted `Event::Paste` text against the internal clipboard: if it matches the
    /// internal patch's own flattening, the OS clipboard still holds our own colored copy, so that
    /// gets pasted; otherwise the text came from elsewhere and is treated as external plain text,
    /// width-validated per character. Either way, the result lands as a floating Selection stamp
    /// anchored at the hovered cell (or the origin if nothing is hovered).
    pub(crate) fn paste_text(&mut self, text: &str) {
        if self.stroke_in_progress() {
            // Another tool's pointer gesture (drag) owns the canvas right now. `set_tool` below
            // would refuse to switch to Selection while `stroke_active` is true, silently leaving
            // whatever tool is mid-gesture active — landing the pasted stamp on `accept_stamp`
            // would then hit that tool's default no-op and discard the clipboard content with no
            // trace. Skip the paste outright and say so, rather than silently losing it.
            self.last_error = Some("paste ignored: a drag is in progress".to_string());
            return;
        }
        self.flush_active_tool(); // drop any current float before reading self.doc / switching tools
        let patch = if is_own_clipboard_text(text, self.internal_clipboard.as_ref()) {
            self.internal_clipboard.clone().expect("is_own_clipboard_text implies Some")
        } else {
            let (patch, dropped) =
                CellPatch::from_external_text(text, self.active_fg, self.active_bg);
            if dropped > 0 {
                self.last_error = Some(format!("paste: {dropped} character(s) rejected"));
            }
            patch
        };
        if patch.width == 0 || patch.height == 0 {
            return; // empty clipboard / everything rejected: no float, warning already surfaced
        }
        let anchor = self.hovered_cell.unwrap_or((0, 0));
        if self.tool_kind != ToolKind::Selection {
            self.set_tool(ToolKind::Selection);
        }
        self.tool.accept_stamp(patch, anchor, &self.doc);
    }

    /// Discards (not commits) whatever cross-frame session the active tool has pending, replacing
    /// it with a fresh instance. Called when the document itself is about to be replaced (Open):
    /// a pending burst's or float's `before` values are pinned against the doc that's about to be
    /// discarded, so committing into the *new* doc would graft stale edits onto unrelated content.
    fn reset_cross_frame_tool(&mut self) {
        match self.tool_kind {
            ToolKind::Text => self.tool = Box::new(TextTool::new()),
            ToolKind::Selection => self.tool = Box::new(SelectionTool::new()),
            _ => {}
        }
        // A right-click stroke's pending cells are pinned against the doc being discarded too —
        // drop the transient tool outright so a release after the swap can't graft the old
        // document's stroke onto the new one.
        self.rc_tool = None;
        self.text_editing = false;
    }

    /// Tool-select (`P`/`E`/`I`/`T`/`F`/`R`/`L`/`S`), undo/redo, and Ctrl+C copy keys. Undo/redo/
    /// Copy are `Ctrl`-modified chords and stay global (they won't collide with typing into the
    /// color picker's hex field); the single-letter tool keys are guarded on no widget having
    /// focus *and* not being mid-text-edit so typing into that hex field, or into the canvas in
    /// text mode, doesn't get swallowed as a tool switch.
    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        let focused = ui.memory(|m| m.focused().is_some()) || self.text_editing;
        let (redo_shift, undo, redo_y, save, pencil, eraser, eyedropper, text, fill, rect, line, select, brush, copy) =
            ui.input_mut(|i| {
                // Cmd/Ctrl+Shift+Z must be consumed before the plain Cmd/Ctrl+Z pattern, since
                // `matches_logically` ignores extra Shift/Alt — checking undo first would swallow
                // the redo shortcut's Z key press.
                let redo_shift = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
                let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
                let redo_y = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
                let save = i.consume_key(egui::Modifiers::COMMAND, egui::Key::S);
                let pencil = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::P);
                let eraser = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::E);
                let eyedropper = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::I);
                let text = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::T);
                let fill = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::F);
                let rect = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::R);
                let line = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::L);
                let select = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::S);
                let brush = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::B);
                let copy = i.consume_key(egui::Modifiers::COMMAND, egui::Key::C);
                (redo_shift, undo, redo_y, save, pencil, eraser, eyedropper, text, fill, rect, line, select, brush, copy)
            });

        // Undo/redo mid-pointer-gesture would mutate the document under the stroke's pinned
        // `before` values — the eventual commit would write stale planes back over the undone
        // state. Ignored until the gesture ends; the menu items disable themselves the same way.
        if !self.stroke_in_progress() {
            if redo_shift || redo_y {
                self.request_redo();
            } else if undo {
                self.request_undo();
            }
        }
        if save {
            self.save_file();
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
        if fill {
            self.set_tool(ToolKind::Fill);
        }
        if rect {
            self.set_tool(ToolKind::Rectangle);
        }
        if line {
            self.set_tool(ToolKind::Line);
        }
        if select {
            self.set_tool(ToolKind::Selection);
        }
        if brush {
            self.set_tool(ToolKind::Brush);
        }
        if copy {
            self.copy_selection(ui.ctx());
        }
        if self.tool_kind == ToolKind::Brush && !focused {
            self.handle_brush_intensity_keys(ui);
        }
        // `[`/`]` adjust the ACTIVE tool's own stamp. The right-click tool's stamp is
        // deliberately not key-bound — it has its own control in the tool row, and one
        // unambiguous key target beats a mode-dependent one.
        if let Some(slot) = sized_slot(self.tool_kind) {
            if !focused {
                let (shrink, grow) = ui.input_mut(|i| {
                    (
                        i.consume_key(egui::Modifiers::NONE, egui::Key::OpenBracket),
                        i.consume_key(egui::Modifiers::NONE, egui::Key::CloseBracket),
                    )
                });
                let stamp = &mut self.tool_stamps[slot];
                if shrink {
                    stamp.size = stamp.size.saturating_sub(1).max(1);
                }
                if grow {
                    stamp.size = (stamp.size + 1).min(MAX_TOOL_SIZE);
                }
            }
        }
    }

    /// Number keys `1`-`9` -> Fixed intensity 0.1-0.9, `0` -> 1.0. Only consumed while Brush is
    /// the active tool and no widget has focus — pressing a digit implicitly switches into Fixed
    /// mode at that level even if Buildup was active, since reaching for a number key expresses
    /// "I want this exact intensity now."
    fn handle_brush_intensity_keys(&mut self, ui: &mut egui::Ui) {
        const DIGIT_KEYS: [(egui::Key, f32); 10] = [
            (egui::Key::Num1, 0.1),
            (egui::Key::Num2, 0.2),
            (egui::Key::Num3, 0.3),
            (egui::Key::Num4, 0.4),
            (egui::Key::Num5, 0.5),
            (egui::Key::Num6, 0.6),
            (egui::Key::Num7, 0.7),
            (egui::Key::Num8, 0.8),
            (egui::Key::Num9, 0.9),
            (egui::Key::Num0, 1.0),
        ];
        let level = ui.input_mut(|i| {
            DIGIT_KEYS
                .iter()
                .find(|&&(key, _)| i.consume_key(egui::Modifiers::NONE, key))
                .map(|&(_, level)| level)
        });
        if let Some(level) = level {
            self.density_mode = DensityMode::Fixed(Fixed(level));
        }
    }

    fn palette_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Palette");
        ui.horizontal_wrapped(|ui| {
            for i in 0..self.pages.len() {
                let name = self.pages[i].name;
                ui.selectable_value(&mut self.active_page, i, name);
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
        ui.checkbox(&mut self.mask.bg, "Background");

        if self.tool_kind == ToolKind::Brush {
            ui.separator();
            self.brush_panel(ui);
        }
    }

    /// Ramp picker, Fixed/Buildup mode selector, and intensity slider — visible only while the
    /// density brush is the active tool.
    fn brush_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Brush");
        egui::ComboBox::from_label("Ramp")
            .selected_text(self.ramps[self.active_ramp].name)
            .show_ui(ui, |ui| {
                for i in 0..self.ramps.len() {
                    ui.selectable_value(&mut self.active_ramp, i, self.ramps[i].name);
                }
            });

        let mut is_buildup = matches!(self.density_mode, DensityMode::Buildup(_));
        ui.horizontal(|ui| {
            if ui.selectable_label(!is_buildup, "Fixed").clicked() {
                is_buildup = false;
            }
            if ui.selectable_label(is_buildup, "Buildup").clicked() {
                is_buildup = true;
            }
        });
        if is_buildup {
            self.density_mode = DensityMode::Buildup(Buildup);
        } else {
            let mut level = match self.density_mode {
                DensityMode::Fixed(Fixed(level)) => level,
                DensityMode::Buildup(_) => 1.0,
            };
            ui.add(egui::Slider::new(&mut level, 0.0..=1.0).text("Intensity"));
            self.density_mode = DensityMode::Fixed(Fixed(level));
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open…").clicked() {
                    self.open_file();
                }
                ui.separator();
                if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    self.save_file();
                }
                if ui.button("Save As…").clicked() {
                    self.save_file_as();
                }
                ui.separator();
                if ui.button("Export Text…").clicked() {
                    self.export_text_file();
                }
                if ui.button("Export PNG…").clicked() {
                    // Not the authoritative flush — harmless dialog-open convenience only. The
                    // dialog is non-modal, so more edits can happen while it's open;
                    // `export_png_file` (fired by the dialog's own "Export…" button) re-flushes
                    // immediately before reading `self.doc`, which is the flush that matters.
                    self.flush_active_tool();
                    self.png_dialog_open = true;
                }
            });
            ui.menu_button("Edit", |ui| {
                // Disabled mid-gesture for the same reason handle_keys ignores Ctrl+Z/Y then:
                // an undo under an in-flight stroke's pinned `before` values commits stale cells.
                let no_stroke = !self.stroke_in_progress();
                let undo = egui::Button::new("Undo").shortcut_text("Ctrl+Z");
                if ui.add_enabled(self.history.can_undo() && no_stroke, undo).clicked() {
                    self.request_undo();
                }
                let redo = egui::Button::new("Redo").shortcut_text("Ctrl+Y");
                if ui.add_enabled(self.history.can_redo() && no_stroke, redo).clicked() {
                    self.request_redo();
                }
                ui.separator();
                let can_copy = self.tool_kind == ToolKind::Selection
                    && self.tool.selection_overlay().and_then(|v| v.marquee).is_some();
                let copy = egui::Button::new("Copy Selection").shortcut_text("Ctrl+C");
                if ui.add_enabled(can_copy, copy).clicked() {
                    self.copy_selection(ui.ctx());
                }
                if ui.button("Copy All as Text").clicked() {
                    // Flush first: a pending text burst or floating selection lives only in
                    // `self.tool`'s overlay until committed into `self.doc` — copying without
                    // flushing would silently drop just-typed or just-moved content from the
                    // whole-document clipboard contents.
                    self.flush_active_tool();
                    ui.ctx().copy_text(export_text(&self.doc));
                }
                ui.separator();
                if ui.button("Resize Canvas…").clicked() {
                    // Reads self.doc for the current extent, which a pending burst/float doesn't
                    // change (extent is fixed regardless), but flushing keeps the dialog's initial
                    // W/H consistent with whatever's about to be committed anyway.
                    self.flush_active_tool();
                    self.resize_w = self.doc.width;
                    self.resize_h = self.doc.height;
                    self.resize_dialog_open = true;
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Fit to Window").clicked() {
                    self.pending_fit = true;
                }
            });
        });
    }

    fn tool_row(&mut self, ui: &mut egui::Ui) {
        const TOOLS: [(ToolKind, &str, &str); 9] = [
            (ToolKind::Pencil, "Pencil (P)", "Draw the active glyph"),
            (ToolKind::Eraser, "Eraser (E)", "Erase cells to blank"),
            (
                ToolKind::Eyedropper,
                "Eyedropper (I)",
                "Click a cell to pick up its text and background colors",
            ),
            (ToolKind::Text, "Text (T)", "Click to place a cursor, then type"),
            (ToolKind::Fill, "Fill (F)", "Flood-fill a connected region"),
            (ToolKind::Rectangle, "Rectangle (R)", "Drag a box outline; joins box-drawing art"),
            (ToolKind::Line, "Line (L)", "Drag a straight line; joins box-drawing art"),
            (ToolKind::Selection, "Selection (S)", "Drag a region to move, copy, or delete"),
            (ToolKind::Brush, "Brush (B)", "Paint density ramps"),
        ];
        ui.horizontal(|ui| {
            ui.label("Left Click Tool:");
            for (kind, label, tip) in TOOLS {
                if ui.selectable_label(self.tool_kind == kind, label).on_hover_text(tip).clicked() {
                    self.set_tool(kind);
                }
            }
            ui.separator();

            // The active tool's own footprint controls. Each sized tool remembers its own
            // size/shape, so these always show and edit exactly the stamp the next primary
            // stroke will use.
            if let Some(slot) = sized_slot(self.tool_kind) {
                stamp_controls(
                    ui,
                    &mut self.tool_stamps[slot],
                    "salt_active",
                    "Stamp width in cells for the active tool ([ and ] to adjust)",
                );
                ui.separator();
            }

            ui.label("Right Click Tool:");
            let rc_idx = self.right_click_tool.min(STROKE_TOOLS.len() - 1);
            egui::ComboBox::from_id_salt("right_click_tool")
                .selected_text(STROKE_TOOLS[rc_idx].name)
                .show_ui(ui, |ui| {
                    for (i, entry) in STROKE_TOOLS.iter().enumerate() {
                        ui.selectable_value(&mut self.right_click_tool, i, entry.name);
                    }
                });
            // The right-click tool's own footprint controls, independent of the primary tools' —
            // visible whenever the right-click tool is sized, so a right-click stroke never
            // draws with an invisible footprint.
            if tool_is_sized(STROKE_TOOLS[rc_idx].kind) {
                stamp_controls(
                    ui,
                    &mut self.rc_stamps[rc_idx],
                    "salt_rc",
                    "Stamp width in cells for the right-click tool",
                );
            }
        });
    }

    /// Bounded W/H entry, top-left anchored (grow pads Blank, shrink crops bottom/right); Apply
    /// pushes exactly one undo entry via `resize_document`, Cancel closes without touching the
    /// document.
    fn resize_dialog(&mut self, ctx: &egui::Context) {
        if !self.resize_dialog_open {
            return;
        }
        let mut open = self.resize_dialog_open;
        egui::Window::new("Resize").open(&mut open).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Width");
                ui.add(egui::DragValue::new(&mut self.resize_w).range(1..=Document::MAX_WIDTH));
            });
            ui.horizontal(|ui| {
                ui.label("Height");
                ui.add(egui::DragValue::new(&mut self.resize_h).range(1..=Document::MAX_HEIGHT));
            });
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    // Resize reads/replaces self.doc directly — flush any pending burst/float
                    // into the pre-resize document first, same trigger-table discipline as
                    // Save/Export/Copy.
                    self.flush_active_tool();
                    match resize_document(&self.doc, self.resize_w, self.resize_h) {
                        Ok(Some(edit)) => {
                            self.history.apply(&mut self.doc, edit);
                            self.last_error = None;
                            self.resize_dialog_open = false;
                        }
                        Ok(None) => self.resize_dialog_open = false, // same extent: silent close
                        Err(ResizeError::ZeroExtent) => {
                            self.last_error = Some("resize: width and height must be at least 1".to_string());
                        }
                        Err(ResizeError::TooLarge { max_width, max_height, .. }) => {
                            self.last_error =
                                Some(format!("resize: exceeds the {max_width}x{max_height} maximum"));
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.resize_dialog_open = false;
                }
            });
        });
        self.resize_dialog_open &= open;
    }

    /// A cell-scale preset picker; on confirm, opens a native save dialog and writes the
    /// rasterized PNG bytes.
    fn png_export_dialog(&mut self, ctx: &egui::Context) {
        if !self.png_dialog_open {
            return;
        }
        let mut open = self.png_dialog_open;
        let mut do_export = false;
        egui::Window::new("Export PNG").open(&mut open).show(ctx, |ui| {
            ui.label("Pixels per cell:");
            ui.horizontal(|ui| {
                for &scale in &PNG_SCALE_PRESETS {
                    ui.selectable_value(&mut self.png_cell_px, scale, format!("{scale}px"));
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Export…").clicked() {
                    do_export = true;
                }
                if ui.button("Cancel").clicked() {
                    self.png_dialog_open = false;
                }
            });
        });
        self.png_dialog_open &= open;
        if do_export {
            self.export_png_file();
            self.png_dialog_open = false;
        }
    }

    /// Rasterizes and writes the current document to a user-picked `.png` file at
    /// `self.png_cell_px` pixels per cell. Reads `self.doc` directly, so it flushes any pending
    /// burst/float first — the dialog is a non-modal `egui::Window`, so the canvas stays
    /// interactive while it's open and a Text/Selection edit can still be in flight at the moment
    /// "Export…" is actually clicked, not just when the dialog was opened.
    fn export_png_file(&mut self) {
        self.flush_active_tool();
        let Some(path) = rfd::FileDialog::new().add_filter("PNG", &["png"]).save_file() else {
            return;
        };
        match png_export::export_png(&self.doc, self.png_cell_px) {
            Ok(bytes) => match std::fs::write(&path, bytes) {
                Ok(()) => self.last_error = None,
                Err(e) => self.last_error = Some(format!("failed to write {}: {e}", path.display())),
            },
            Err(e) => self.last_error = Some(format!("PNG export failed: {e}")),
        }
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
                    // Cancel, not flush: the old `self.doc` the active tool's pending burst/float
                    // `before` values were pinned against is about to be discarded, so committing
                    // into it is pointless — and carrying the same tool instance (and
                    // `text_editing`) forward would let it later graft edits, and stale pre-edit
                    // `before` values on Undo, from the discarded document onto the newly loaded
                    // one. Only Text and Selection have cross-frame pending state to strand.
                    self.reset_cross_frame_tool();
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
        // burst's just-typed characters or a floating selection's move until a commit trigger
        // fires. Also covers the `save_file_as` delegation below (a no-op double-flush if already
        // flushed).
        self.flush_active_tool();
        match self.current_path.clone() {
            Some(path) => self.write_gascii(&path),
            None => self.save_file_as(),
        }
    }

    fn save_file_as(&mut self) {
        // Flush first — see `save_file`'s comment. Also reachable directly via the "Save As"
        // toolbar button, not only through `save_file`'s delegation.
        self.flush_active_tool();
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
        self.flush_active_tool();
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

        egui::Panel::top("toolbar").show(ui, |ui| {
            self.menu_bar(ui);
            self.tool_row(ui);
        });
        egui::Panel::left("palette").show(ui, |ui| self.palette_panel(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            canvas::show(ui, self);
        });

        let ctx = ui.ctx().clone();
        self.resize_dialog(&ctx);
        self.png_export_dialog(&ctx);
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

    fn cell(ch: char) -> gascii_core::Cell {
        gascii_core::Cell { ch, fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }
    }

    /// Pins the `sized_slot` mapping: sized kinds get distinct in-range slots, unsized get none —
    /// a duplicated or out-of-range slot would silently alias two tools' stamp settings.
    #[test]
    fn sized_slots_are_distinct_and_in_range() {
        let sized = [ToolKind::Pencil, ToolKind::Eraser, ToolKind::Line, ToolKind::Brush];
        let mut seen = std::collections::HashSet::new();
        for kind in sized {
            let slot = sized_slot(kind).expect("sized kind must have a slot");
            assert!(slot < SIZED_TOOL_COUNT);
            assert!(seen.insert(slot), "slot {slot} assigned twice");
        }
        for kind in [
            ToolKind::Eyedropper,
            ToolKind::Text,
            ToolKind::Fill,
            ToolKind::Rectangle,
            ToolKind::Selection,
        ] {
            assert_eq!(sized_slot(kind), None, "{kind:?} must not have a stamp slot");
        }
    }

    /// Pins `set_tool`'s `_` arm: every ToolKind that isn't special-cased (Text/Selection/
    /// Eyedropper) must have a STROKE_TOOLS entry, or selecting it would silently keep the old
    /// tool. Also pins the right-click default at index 0.
    #[test]
    fn stroke_tools_cover_every_stroke_kind() {
        let all = [
            ToolKind::Pencil,
            ToolKind::Eraser,
            ToolKind::Eyedropper,
            ToolKind::Text,
            ToolKind::Fill,
            ToolKind::Rectangle,
            ToolKind::Line,
            ToolKind::Selection,
            ToolKind::Brush,
        ];
        for kind in all {
            let special = matches!(kind, ToolKind::Text | ToolKind::Selection | ToolKind::Eyedropper);
            assert_eq!(
                STROKE_TOOLS.iter().any(|e| e.kind == kind),
                !special,
                "{kind:?} must appear in STROKE_TOOLS exactly when it isn't special-cased"
            );
        }
        assert_eq!(STROKE_TOOLS[0].kind, ToolKind::Eraser, "right-click default");
    }

    #[test]
    fn paste_text_matching_the_internal_clipboards_own_flattening_is_recognized_as_own() {
        let patch = CellPatch { width: 2, height: 1, cells: vec![cell('a'), cell('b')] };
        let text = patch.to_text();
        assert!(is_own_clipboard_text(&text, Some(&patch)));
    }

    #[test]
    fn paste_text_differing_from_the_internal_clipboard_is_treated_as_external() {
        let patch = CellPatch { width: 2, height: 1, cells: vec![cell('a'), cell('b')] };
        assert!(!is_own_clipboard_text("something else entirely", Some(&patch)));
    }

    #[test]
    fn paste_text_with_no_internal_clipboard_is_always_external() {
        assert!(!is_own_clipboard_text("anything", None));
        assert!(!is_own_clipboard_text("", None));
    }
}
