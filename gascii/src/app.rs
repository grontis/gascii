use std::time::Instant;

use eframe::egui;
use gascii_core::{
    builtin_pages, Document, Eraser, History, Page, Pencil, PlaneMask, Rgba, Tool,
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
    pages: Vec<Page>,
    active_page: usize,
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
            pages: builtin_pages(),
            active_page: 0,
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
        self.tool_kind = kind;
        match kind {
            ToolKind::Pencil => self.tool = Box::new(Pencil::new()),
            ToolKind::Eraser => self.tool = Box::new(Eraser::new()),
            // No Tool object needed: canvas.rs branches around `self.tool` entirely in
            // Eyedropper mode (it produces no Edit).
            ToolKind::Eyedropper => {}
        }
    }

    /// Tool-select (`P`/`E`/`I`) and undo/redo keys. Undo/redo are `Ctrl`-modified chords and stay
    /// global (they won't collide with typing into the color picker's hex field); the
    /// single-letter tool keys are guarded on no widget having focus so typing into that hex
    /// field doesn't get swallowed as a tool switch.
    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        let focused = ui.memory(|m| m.focused().is_some());
        let (redo_shift, undo, redo_y, pencil, eraser, eyedropper) = ui.input_mut(|i| {
            // Cmd/Ctrl+Shift+Z must be consumed before the plain Cmd/Ctrl+Z pattern, since
            // `matches_logically` ignores extra Shift/Alt — checking undo first would swallow
            // the redo shortcut's Z key press.
            let redo_shift = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
            let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            let redo_y = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
            let pencil = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::P);
            let eraser = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::E);
            let eyedropper = !focused && i.consume_key(egui::Modifiers::NONE, egui::Key::I);
            (redo_shift, undo, redo_y, pencil, eraser, eyedropper)
        });

        if redo_shift || redo_y {
            self.history.redo(&mut self.doc);
        } else if undo {
            self.history.undo(&mut self.doc);
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
            ui.separator();

            if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo")).clicked() {
                self.history.undo(&mut self.doc);
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo")).clicked() {
                self.history.redo(&mut self.doc);
            }
        });
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
        });
    }
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
