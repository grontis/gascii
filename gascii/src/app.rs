use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use gascii_core::{
    builtin_pages, builtin_ramps, composite, export_text, load_str, resize_document,
    save_string, AxisAnchor, BrushShape, CellPatch, DensityBrush, DensityMode, Document,
    Eraser, Fixed, FloodFill, History, Line, Page, Pencil, PlaneMask, Ramp, Rectangle, ResizeAnchor,
    ResizeError, Rgba, SelectionTool, TextTool, Tool, ToolEvent, ToolResponse, WidthReject, MAX_TOOL_SIZE,
};

use crate::canvas::{self, CanvasRenderer, NaiveRenderer};
use crate::fonts;
use crate::png_export;
use crate::prefs;
use crate::ui::dialog::{self, DialogAction};
use crate::viewport::Viewport;

/// PNG cell-px per export scale preset: `16 * {1, 2, 4}`.
const EXPORT_CELL_PX_BASE: u32 = 16;

/// Whether a pasted `Event::Paste` text is still the app's own copy: the OS clipboard is "ours"
/// exactly when `internal`'s own flattening still matches what came back on paste. Pulled out of
/// `paste_text` as a pure function so the copy/paste reconciliation decision is unit-testable
/// without constructing a full `GasciiApp`.
fn is_own_clipboard_text(text: &str, internal: Option<&CellPatch>) -> bool {
    internal.is_some_and(|p| p.to_text() == text)
}

/// The Export dialog's "Trim trailing spaces" *unchecked* path: every row stays padded to
/// `doc.width` glyphs, unlike `export_text`'s trailing-whitespace trim (which stays the default,
/// matching the format's pre-existing behavior).
fn export_text_untrimmed(doc: &Document) -> String {
    composite(doc)
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the document has changed since the last save/load: true whenever the undo stack's
/// current top-edit id doesn't match the id recorded at that save/load. Pulled out as a pure
/// function, mirroring `is_own_clipboard_text`, so the comparison is unit-testable without a live
/// `GasciiApp`; `GasciiApp::is_dirty` is the thin method wrapping it.
fn edit_marker_differs(current: Option<u64>, saved: Option<u64>) -> bool {
    current != saved
}

/// How many glyphs the RECENT row remembers.
pub(crate) const RECENT_GLYPHS: usize = 6;

/// Pushes `ch` to the front of a most-recent-first list, de-duplicated and capped.
///
/// Pure, so the ordering rule is testable without a `GasciiApp`: re-using a glyph already in the
/// list must move it to the front rather than add a second copy, or the row fills with duplicates
/// and stops being six *distinct* recent glyphs.
pub(crate) fn push_recent(recent: &mut Vec<char>, ch: char) {
    recent.retain(|&c| c != ch);
    recent.insert(0, ch);
    recent.truncate(RECENT_GLYPHS);
}

/// The binding a pasted float lands in: whichever is already bound to Selection (L wins if both),
/// else L, rebound.
///
/// Never R by default: a paste is a keyboard command, the keyboard's tool is L's, and silently
/// rebinding the right button out from under the user is worse than rebinding the left. Pure, so the
/// choice is testable without a `GasciiApp` (following `is_own_clipboard_text`'s precedent).
fn paste_target(l: ToolKind, r: ToolKind) -> Binding {
    if l == ToolKind::Selection {
        Binding::L
    } else if r == ToolKind::Selection {
        Binding::R
    } else {
        Binding::L
    }
}

/// Whether typed single-letter keys should be swallowed as tool-select shortcuts rather than
/// routed to the keyboard-owning slot's tool. True only while that slot is Text: Text is the only
/// kind whose `Tool::update` consumes a bare `Char` event as content — `SelectionTool`'s `Char`
/// falls through to its catch-all no-op — so suppressing shortcuts for any other owning kind
/// makes the shortcuts dead weight for no correctness benefit.
fn suppresses_tool_shortcuts(owner_kind: Option<ToolKind>) -> bool {
    matches!(owner_kind, Some(ToolKind::Text))
}

/// Whether this kind can hold a cross-frame Session (uncommitted work outliving a single stroke —
/// a Text burst, a floating stamp). The one place that fact lives: `flush_slot`, `end_session`,
/// the document-swap reset, and the takeover in `begin_gesture` all consult it, so a future
/// session-holding kind is a one-line change here rather than a four-site hunt.
pub(crate) fn holds_session(kind: ToolKind) -> bool {
    matches!(kind, ToolKind::Text | ToolKind::Selection)
}

/// The order the two bindings commit, given which one (if any) the pointer is currently driving.
///
/// Overlay order *is* commit order: an overlay is a promise about the document's final state, and
/// the last committer wins any overlapped cell — so the last committer must paint on top. A slot
/// mid-gesture commits at its imminent release, before any idle slot's session reaches its next
/// structural trigger; so the gesturing slot goes first, and underneath.
///
/// Pure, mirroring `is_own_clipboard_text` and `edit_marker_differs`, so the rule is testable
/// without a live `GasciiApp` — and so `flush_all` and the painter cannot disagree about it.
fn order_for(stroke_owner: Option<Binding>) -> [Binding; 2] {
    match stroke_owner {
        Some(b) => [b, b.other()],
        None => [Binding::L, Binding::R],
    }
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

/// Which mouse button drives a tool. Named for what the UI says — the options bar's segment and
/// the toolbox badges read "L" and "R" — rather than Left/Right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Binding {
    L = 0,
    R = 1,
}

impl Binding {
    pub(crate) const ALL: [Binding; 2] = [Binding::L, Binding::R];

    pub(crate) fn other(self) -> Binding {
        match self {
            Binding::L => Binding::R,
            Binding::R => Binding::L,
        }
    }

    /// Index into `GasciiApp::slots`. Hot paths index the field directly rather than going through
    /// a `&mut self` accessor, which would borrow all of `self` and collide with the `&self.doc`
    /// every `Tool::update` also needs.
    pub(crate) fn ix(self) -> usize {
        self as usize
    }
}

/// One mouse button's persistent tool: what it's bound to, the live instance (which may hold a
/// session outliving any single gesture), and that binding's own per-kind footprint memory.
/// Nothing here knows which button it belongs to — that is the whole symmetry.
pub(crate) struct ToolSlot {
    pub kind: ToolKind,
    pub tool: Box<dyn Tool>,
    /// Per-kind footprint memory, indexed by `sized_slot`. Private to this slot, so L's Eraser size
    /// and R's Eraser size are independent by construction rather than by two parallel arrays.
    pub stamps: [StampSettings; SIZED_TOOL_COUNT],
}

impl ToolSlot {
    fn new(kind: ToolKind) -> Self {
        ToolSlot { kind, tool: make_tool(kind), stamps: [StampSettings::default(); SIZED_TOOL_COUNT] }
    }

    /// This slot's footprint for whatever it is currently bound to (the identity default for
    /// unsized kinds, which ignore it).
    pub fn stamp(&self) -> StampSettings {
        sized_slot(self.kind).map(|i| self.stamps[i]).unwrap_or_default()
    }
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
        StampSettings { size: 1, shape: BrushShape::default() }
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

/// Placeholder `Tool` for `ToolKind::Eyedropper`, the one kind that isn't one: it yields app color
/// state rather than an `Edit`, and `ToolResponse` has no variant to carry a picked color. It
/// exists so a binding's tool is never `Option` — every generic path (pending, resync, caret,
/// selection overlay, flush) reads it uniformly and gets the trait's own "nothing here" answers.
/// The actual sampling stays in `canvas.rs`'s press branch.
struct InertTool;

impl Tool for InertTool {
    fn update(&mut self, _ev: ToolEvent, _ctx: &gascii_core::ToolCtx, _doc: &Document) -> ToolResponse {
        ToolResponse::Idle
    }

    fn pending(&self) -> &[gascii_core::PendingCell] {
        &[]
    }
}

/// One tool: kind, display name, shortcut, hint, and constructor in a single row.
pub(crate) struct ToolDef {
    pub kind: ToolKind,
    pub name: &'static str,
    pub key: egui::Key,
    pub tip: &'static str,
    pub make: fn() -> Box<dyn Tool>,
}

/// The nine tools, and the single source of truth for their names, shortcuts, hints, and
/// constructors. The toolbox, the shortcut handler, the options bar, and both bindings all read
/// this one table, so a tool cannot be added to the UI and forgotten in the constructor.
pub(crate) const TOOLS: [ToolDef; 9] = [
    ToolDef {
        kind: ToolKind::Pencil,
        name: "Pencil",
        key: egui::Key::P,
        tip: "Draw the active glyph",
        make: || Box::new(Pencil::new()),
    },
    ToolDef {
        kind: ToolKind::Eraser,
        name: "Eraser",
        key: egui::Key::E,
        tip: "Erase cells to blank",
        make: || Box::new(Eraser::new()),
    },
    ToolDef {
        kind: ToolKind::Eyedropper,
        name: "Eyedropper",
        key: egui::Key::I,
        tip: "Click a cell to pick up its text and background colors",
        make: || Box::new(InertTool),
    },
    ToolDef {
        kind: ToolKind::Text,
        name: "Text",
        key: egui::Key::T,
        tip: "Click to place a cursor, then type",
        make: || Box::new(TextTool::new()),
    },
    ToolDef {
        kind: ToolKind::Fill,
        name: "Fill",
        key: egui::Key::F,
        tip: "Flood-fill a connected region",
        make: || Box::new(FloodFill::new()),
    },
    ToolDef {
        kind: ToolKind::Rectangle,
        name: "Rectangle",
        key: egui::Key::R,
        tip: "Drag a box outline; joins box-drawing art",
        make: || Box::new(Rectangle::new()),
    },
    ToolDef {
        kind: ToolKind::Line,
        name: "Line",
        key: egui::Key::L,
        tip: "Drag a straight line; joins box-drawing art",
        make: || Box::new(Line::new()),
    },
    ToolDef {
        kind: ToolKind::Selection,
        name: "Selection",
        key: egui::Key::S,
        tip: "Drag a region to move, copy, or delete",
        make: || Box::new(SelectionTool::new()),
    },
    ToolDef {
        kind: ToolKind::Brush,
        name: "Brush",
        key: egui::Key::B,
        tip: "Paint density ramps",
        make: || Box::new(DensityBrush::new()),
    },
];

pub(crate) fn tool_def(kind: ToolKind) -> &'static ToolDef {
    TOOLS.iter().find(|d| d.kind == kind).expect("TOOLS covers every ToolKind")
}

/// Builds a fresh instance for `kind`. Total over `ToolKind` — `tools_table_lists_every_kind_
/// exactly_once` pins that the lookup cannot miss.
pub(crate) fn make_tool(kind: ToolKind) -> Box<dyn Tool> {
    (tool_def(kind).make)()
}


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
    /// The two bindings, indexed by `Binding::ix`. Exactly one tool is bound to each at all times.
    pub(crate) slots: [ToolSlot; 2],
    /// Which binding the options bar edits. A gesture on either button selects that button's
    /// segment.
    pub(crate) options_focus: Binding,
    /// Which slot's tool the pointer is currently driving, if any. Stroke ownership is one
    /// question, so it is one field — which is what let the press/drag/release paths collapse to a
    /// single parameterized call site. At most one stroke is live across both buttons.
    pub(crate) stroke_owner: Option<Binding>,
    pub(crate) space_pan_active: bool,
    /// Which slot's tool receives keystrokes. There is one keyboard and both slots can be bound to
    /// keyboard-driven tools, so ownership is explicit state rather than something derived: it is
    /// acquired by a canvas press on a Text/Selection slot (or by paste), and released when that
    /// slot's session ends or its binding changes.
    ///
    /// Deliberately not derived from tool state. Escape ends a text session while `TextTool` keeps
    /// its cursor placed, so "has a caret" and "is accepting keys" genuinely differ. It also gates
    /// every single-letter tool-select key, so typing never switches tools — though which keys
    /// actually get suppressed is `suppresses_tool_shortcuts`'s call, not merely "is this `Some`".
    ///
    /// Private: `canvas.rs` cannot write this field directly. `keyboard_owner()`/`acquire_keyboard`/
    /// `release_keyboard`/`end_session` are the only ways to read or mutate it from outside this
    /// module — see `end_session` for the composite "this binding's session is over" operation, and
    /// `flush_slot` for why committing pending work deliberately does NOT release this on its own.
    keyboard_owner: Option<Binding>,
    /// Previous frame's window-focus state, for edge-detecting focus loss.
    pub(crate) was_focused: bool,
    /// The last region copied via Ctrl+C, kept alongside the plain text written to the OS
    /// clipboard. A paste whose `Event::Paste` text still matches this patch's own flattening
    /// pastes the colored version; otherwise it's treated as external plain text.
    pub(crate) internal_clipboard: Option<CellPatch>,
    pub(crate) pages: Vec<Page>,
    pub(crate) active_page: usize,
    /// The last [`RECENT_GLYPHS`] glyphs used, most recent first. Fed by picking a swatch
    /// (`pick_glyph`) and by a committed stroke that stamped the active glyph (`note_glyph_drawn`).
    pub(crate) recent_glyphs: Vec<char>,
    /// Built-in Ramps, populated at startup — the density brush's glyph sources.
    pub(crate) ramps: Vec<Ramp>,
    /// Index into `ramps`: the brush's currently active ramp.
    pub(crate) active_ramp: usize,
    /// The brush's active intensity source (Fixed level or Buildup).
    pub(crate) density_mode: DensityMode,
    /// The chosen theme preference (persisted). Applied to the `egui::Context` once at startup
    /// (`GasciiApp::new`) and again on every change from the View ▸ Theme menu — never read back
    /// from the `Context` itself, so `Prefs::from_app`/`App::save` need no `Context` at all.
    pub(crate) theme_pref: egui::ThemePreference,
    /// Whether the canvas cell-grid overlay is drawn. Persisted, off by default.
    pub(crate) show_grid: bool,
    resize_dialog_open: bool,
    resize_w: u16,
    resize_h: u16,
    /// The 3x3 anchor the Resize dialog is currently set to. Remembered for the session (not
    /// persisted across restarts) — each resize starts from whatever the last one used.
    pub(crate) resize_anchor: ResizeAnchor,
    new_dialog_open: bool,
    new_w: u16,
    new_h: u16,
    new_bg: Rgba,
    export_dialog_open: bool,
    pub(crate) export: ExportSettings,
    export_preview: Option<egui::TextureHandle>,
    /// The settings the current `export_preview` texture was generated from — regenerated only
    /// when this stops matching `self.export`, or on dialog open (`None` after close).
    export_preview_key: Option<ExportSettings>,
    current_path: Option<PathBuf>,
    /// Up to 8 most-recently-opened/saved paths, most recent first. A failed re-open drops its
    /// entry rather than leaving a dead path in the list.
    pub(crate) recent_files: Vec<PathBuf>,
    pub(crate) last_error: Option<String>,
    /// The undo-stack edit id (`History::top_edit_id`) at the moment of the last successful save
    /// or load — `None` matches a fresh `History`'s own sentinel. `is_dirty` is a pure comparison
    /// against `self.history.top_edit_id()`; nothing else needs to know about this field.
    saved_marker: Option<u64>,
    /// Which unsaved-changes confirmation is pending, if any — closing the app, or replacing the
    /// document via File ▸ New…. `pub(crate)` because `canvas.rs`'s modality guard reads it
    /// through `modal_open()`.
    pub(crate) confirm: Option<PendingConfirm>,
    /// Single-use: lets the very next `close_requested` frame through unconditionally, then resets
    /// itself. Set by `close_now` so "Save" and "Don't Save" can re-request a real close without
    /// re-triggering the veto they just cleared.
    force_close: bool,
    /// The title last pushed to the OS, so it is only sent when it changes.
    shown_title: String,
    started: Instant,
    first_frame: bool,
}

/// Which unsaved-changes confirmation is in flight. Both share the same Save/Don't Save/Cancel
/// dialog body; only what happens after Save/Don't-Save resolves differs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PendingConfirm {
    CloseApp,
    NewDocument,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExportFormat {
    Text,
    Png,
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

impl GasciiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, started: Instant) -> Self {
        fonts::install_fonts(&cc.egui_ctx);
        crate::ui::theme::install(&cc.egui_ctx);
        let mut app = Self::with_state(started);
        prefs::load(cc.storage, &mut app);
        cc.egui_ctx.set_theme(app.theme_pref);
        app
    }

    /// A `GasciiApp` with no egui context attached. The context is needed only to register fonts and
    /// themes; every field below is plain data. Splitting it out lets tests drive the real
    /// flush/commit/resync machinery — the parts that are about coordination between the two
    /// bindings and so cannot be reached by the pure-function tests.
    #[cfg(test)]
    pub(crate) fn headless() -> Self {
        Self::with_state(Instant::now())
    }

    fn with_state(started: Instant) -> Self {
        Self {
            doc: Document::default_document(),
            viewport: Viewport::default(),
            hovered_cell: None,
            renderer: Box::new(NaiveRenderer),
            // Fit on the first frame: a document pinned to the top-left corner of the desk is not
            // "the star", and the viewport's default pan of zero puts it there.
            pending_fit: true,
            history: History::new(),
            active_glyph: '#',
            active_fg: Rgba::WHITE,
            active_bg: Rgba::TRANSPARENT,
            mask: PlaneMask::default(),
            slots: [ToolSlot::new(ToolKind::Pencil), ToolSlot::new(ToolKind::Eraser)],
            options_focus: Binding::L,
            stroke_owner: None,
            space_pan_active: false,
            keyboard_owner: None,
            was_focused: true,
            internal_clipboard: None,
            pages: builtin_pages(),
            active_page: 0,
            recent_glyphs: Vec::new(),
            ramps: builtin_ramps(),
            active_ramp: 0,
            density_mode: DensityMode::Fixed(Fixed(1.0)),
            theme_pref: egui::ThemePreference::System,
            show_grid: false,
            resize_dialog_open: false,
            resize_w: Document::DEFAULT_WIDTH,
            resize_h: Document::DEFAULT_HEIGHT,
            resize_anchor: ResizeAnchor::default(),
            new_dialog_open: false,
            new_w: Document::DEFAULT_WIDTH,
            new_h: Document::DEFAULT_HEIGHT,
            new_bg: Rgba(0, 0, 0, 255),
            export_dialog_open: false,
            export: ExportSettings::default(),
            export_preview: None,
            export_preview_key: None,
            current_path: None,
            recent_files: Vec::new(),
            last_error: None,
            saved_marker: None,
            confirm: None,
            force_close: false,
            shown_title: String::new(),
            started,
            first_frame: true,
        }
    }

    /// True while any modal dialog is showing. `canvas.rs` polls raw pointer/keyboard state rather
    /// than using egui's occlusion system, so a modal's backdrop alone does not block it — every
    /// modal flag must be named here, and every raw-input-polling site in `canvas.rs`/`handle_keys`
    /// must gate on this rather than any single dialog's own flag.
    pub(crate) fn modal_open(&self) -> bool {
        self.confirm.is_some() || self.resize_dialog_open || self.export_dialog_open || self.new_dialog_open
    }

    /// Whether any pointer gesture — primary stroke or right-click stroke — currently owns the
    /// canvas.
    pub(crate) fn stroke_in_progress(&self) -> bool {
        self.stroke_owner.is_some()
    }

    pub(crate) fn slot(&self, b: Binding) -> &ToolSlot {
        &self.slots[b.ix()]
    }

    /// Prefer indexing `slots` directly in paths that also touch `self.doc` — this borrows all of
    /// `self` and will collide there.
    #[allow(dead_code)]
    pub(crate) fn slot_mut(&mut self, b: Binding) -> &mut ToolSlot {
        &mut self.slots[b.ix()]
    }

    /// Binds `kind` to `b`, replacing that slot's instance. A no-op while a gesture is active: the
    /// pointer is captured by it, so rebinding is suppressed mid-stroke.
    ///
    /// Ends the slot's session first, unconditionally — `end_session` is self-gating (via
    /// `flush_slot`), and the instance is about to be replaced regardless of whether the kind
    /// actually changed. Without this, re-selecting Text/Selection while already active would
    /// silently discard the pending, uncommitted burst or float — and only this slot's claim on the
    /// keyboard is released, so rebinding L must not silently mute a live session on R.
    fn set_tool(&mut self, b: Binding, kind: ToolKind) {
        if self.stroke_in_progress() {
            return;
        }
        self.end_session(b);
        self.slots[b.ix()].kind = kind;
        self.slots[b.ix()].tool = make_tool(kind);
        // The options bar (and the [/] size keys behind it) follows the binding the user just
        // acted on — the same rule a canvas gesture applies. Without this, picking a tool by
        // shortcut or toolbox click leaves the bar editing the OTHER binding's stamp.
        self.options_focus = b;
    }

    /// Which slot currently holds the keyboard, if any.
    pub(crate) fn keyboard_owner(&self) -> Option<Binding> {
        self.keyboard_owner
    }

    /// Gives `b` the keyboard, unconditionally. The only setter of `Some` — every acquisition
    /// (a canvas press on Text/Selection, a paste) routes through this.
    pub(crate) fn acquire_keyboard(&mut self, b: Binding) {
        self.keyboard_owner = Some(b);
    }

    /// Releases `b`'s claim on the keyboard, if it holds one. A no-op for the other slot's claim.
    pub(crate) fn release_keyboard(&mut self, b: Binding) {
        if self.keyboard_owner == Some(b) {
            self.keyboard_owner = None;
        }
    }

    /// Binds `kind` to `b`. The chrome's entry point to `set_tool`.
    pub(crate) fn bind(&mut self, b: Binding, kind: ToolKind) {
        self.set_tool(b, kind);
    }

    /// Selects `ch` for drawing and records it in RECENT.
    pub(crate) fn pick_glyph(&mut self, ch: char) {
        self.active_glyph = ch;
        push_recent(&mut self.recent_glyphs, ch);
    }

    /// Records the active glyph in RECENT after a committed stroke actually used it — the other
    /// half of RECENT's contract, alongside picking a swatch. Only kinds that stamp `ctx.glyph`
    /// count (the Brush writes ramp characters, the Eraser writes Blank), and only when the glyph
    /// plane was being written at all.
    pub(crate) fn note_glyph_drawn(&mut self, kind: ToolKind) {
        let stamps_glyph = matches!(
            kind,
            ToolKind::Pencil | ToolKind::Fill | ToolKind::Rectangle | ToolKind::Line
        );
        if stamps_glyph && self.mask.glyph {
            push_recent(&mut self.recent_glyphs, self.active_glyph);
        }
    }

    /// Swaps FG and BG (the `X` shortcut and the `⇄` control).
    pub(crate) fn swap_colors(&mut self) {
        std::mem::swap(&mut self.active_fg, &mut self.active_bg);
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

    /// Whether the document has unsaved changes: the undo stack's current top edit doesn't match
    /// the one recorded at the last successful save or load. A brand-new document is clean by
    /// construction — both sides start `None`.
    pub(crate) fn is_dirty(&self) -> bool {
        edit_marker_differs(self.history.top_edit_id(), self.saved_marker)
    }

    /// Applies `edit` and re-pins every other slot's pending session against the mutated document.
    /// The single choke point for every document mutation the app performs.
    ///
    /// `Tool::resync`'s contract is "the document changed underneath you by a path other than your
    /// own `update`". With two persistent slots, *any* mutation is underneath at least one of them,
    /// so this obligation exists at every `History::apply` site, not just the one. Routing them all
    /// through here is what keeps that from being six chances to forget.
    ///
    /// `origin` is the slot whose own `update` produced this edit — it has nothing to re-pin.
    /// `None` for app-level mutations (redo, resize).
    pub(crate) fn apply_edit(&mut self, edit: gascii_core::Edit, origin: Option<Binding>) {
        self.history.apply(&mut self.doc, edit);
        self.resync_slots(origin);
    }

    pub(crate) fn resync_slots(&mut self, except: Option<Binding>) {
        for b in Binding::ALL {
            if Some(b) != except {
                self.slots[b.ix()].tool.resync(&self.doc, 0);
            }
        }
    }

    /// Commits slot `b`'s pending cross-frame session (Text's burst, Selection's float) into one
    /// undo entry. A no-op for every other kind.
    ///
    /// Narrowed contract: commits pending work only. Never touches keyboard ownership or a tool's
    /// residual interactive state (a bare marquee, a placed caret) — see `end_session` for the
    /// operation that also clears those. A structural trigger (Ctrl+S, Ctrl+Z, opening a dialog,
    /// focus loss) must be able to commit in-flight work without silently killing an otherwise-idle
    /// marquee or caret's claim on the keyboard.
    ///
    /// Deliberately NOT gated on the binding being mid-stroke. Every flush caller either reads the
    /// document right after (save, the close-confirm dirty check, copy) or follows up with a
    /// `Cancel` (`end_session`, focus loss) — skipping the commit for an in-flight stroke would
    /// hand those callers a document missing work the user can see, or let the `Cancel` discard it
    /// outright. Committing a Text/Selection session mid-stroke is well-defined in core (the float
    /// drops at its current position, the burst commits, the remaining pointer motion goes inert
    /// until release): a prematurely-ended stroke is a startle, silently lost work is not.
    ///
    /// The kind gate isn't correctness — every stroke tool's catch-all swallows `Commit`
    /// harmlessly — it avoids building a `ToolCtx`, which clones the active ramp's `Vec<char>`.
    pub(crate) fn flush_slot(&mut self, b: Binding) {
        if !holds_session(self.slots[b.ix()].kind) {
            return;
        }
        let tctx = crate::canvas::tool_ctx(self, b);
        if let ToolResponse::Commit(Some(edit)) =
            self.slots[b.ix()].tool.update(ToolEvent::Commit, &tctx, &self.doc)
        {
            self.apply_edit(edit, Some(b));
        }
    }

    /// Fully ends slot `b`'s interactive session, right now: commits whatever is pending (never
    /// silently discarding it — see `flush_slot`), then clears the tool's residual interactive state
    /// (a bare marquee, a placed caret) via `ToolEvent::Cancel`, then releases the keyboard if `b`
    /// held it. The single choke point for "b's session is over" — as opposed to `flush_slot`, which
    /// deliberately leaves both residue and keyboard ownership alone so a structural trigger (Ctrl+S,
    /// Ctrl+Z, opening a dialog, focus loss) doesn't silently kill an otherwise-idle marquee or caret.
    pub(crate) fn end_session(&mut self, b: Binding) {
        self.flush_slot(b);
        if holds_session(self.slots[b.ix()].kind) {
            let tctx = crate::canvas::tool_ctx(self, b);
            self.slots[b.ix()].tool.update(ToolEvent::Cancel, &tctx, &self.doc);
        }
        self.release_keyboard(b);
    }

    /// Flushes both slots, in commit order.
    ///
    /// The order matters and the reason is subtle: the first slot's flush mutates the document,
    /// which leaves the second slot's session holding `before` values pinned against the *pre-flush*
    /// document. Committing those would write stale cells back over the first slot's. `flush_slot`
    /// routes through `apply_edit`, which resyncs the other slot — so the second flush sees the
    /// first's committed cells. Every trigger that reads or replaces `self.doc` calls this.
    pub(crate) fn flush_all(&mut self) {
        for b in self.commit_order() {
            self.flush_slot(b);
        }
    }

    /// The order the slots commit — and therefore the order their overlays paint (bottom first).
    pub(crate) fn commit_order(&self) -> [Binding; 2] {
        order_for(self.stroke_owner)
    }

    /// Commits any pending text burst or floating selection, then undoes the most recent edit.
    /// Flushing before undo is correct here: it turns "Undo mid-session" into "undo the very edit
    /// that was just committed" (the same edit the flush just committed), matching ordinary
    /// editor conventions.
    fn request_undo(&mut self) {
        self.flush_all();
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
    /// for, that pinned value goes stale relative to `doc`'s new actual state; `self.slots[0].tool.resync`
    /// re-pins it. `SelectionTool` inherits the trait's default no-op `resync` — its drop reads
    /// `before` from the document at drop time, not lift time, so there is nothing to re-pin.
    fn request_redo(&mut self) {
        if self.history.can_redo() {
            self.history.redo(&mut self.doc);
            // A redo mutates `self.doc` behind BOTH slots' backs, so both re-pin — there is no
            // originating slot to exempt.
            self.resync_slots(None);
        } else {
            self.flush_all();
        }
    }

    /// The slot holding the live Selection session — the app's answer to "the selection". At most
    /// one exists (a press starts a session and takes the keyboard, and starting one finishes the
    /// other slot's), so the singular language in `copy_selection` and the Edit menu stays honest.
    pub(crate) fn selection_slot(&self) -> Option<Binding> {
        self.keyboard_owner.filter(|&b| self.slot(b).kind == ToolKind::Selection)
    }

    /// The first binding holding `kind`, if either does. For controls over app-global state that a
    /// tool uses (the Brush's ramp and intensity), which stay live while either button holds it.
    pub(crate) fn bound_to(&self, kind: ToolKind) -> Option<Binding> {
        Binding::ALL.into_iter().find(|&b| self.slot(b).kind == kind)
    }

    /// Copies the active selection's cells to both the OS clipboard (plain text) and the app's
    /// colored internal clipboard. A no-op unless a Selection binding has a region defined —
    /// "Copy All as Text" remains the way to copy the whole document.
    pub(crate) fn copy_selection(&mut self, ctx: &egui::Context) {
        let Some(b) = self.selection_slot() else {
            return;
        };
        // A dropped float's cells must be in `self.doc` before capturing the region.
        self.flush_all();
        let Some(rect) = self.slots[b.ix()].tool.selection_overlay().and_then(|v| v.marquee) else {
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
        self.flush_all(); // drop any current float before reading self.doc / switching tools
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
        let b = paste_target(self.slot(Binding::L).kind, self.slot(Binding::R).kind);
        if self.slot(b).kind != ToolKind::Selection {
            self.set_tool(b, ToolKind::Selection);
        }
        // A pasted float is a session, and only one exists at a time. Focus follows the session,
        // exactly as a canvas press would set it.
        self.end_session(b.other());
        self.acquire_keyboard(b);
        self.options_focus = b;
        self.slots[b.ix()].tool.accept_stamp(patch, anchor, &self.doc);
    }

    /// Discards (not commits) all pending work: each session-holding slot's tool is replaced with
    /// a fresh instance, and any in-flight stroke is cancelled. Called when the document itself is
    /// about to be replaced (Open): pending `before` values are pinned against the doc that's
    /// about to be discarded, so committing into the *new* doc would graft stale edits onto
    /// unrelated content.
    fn reset_cross_frame_tool(&mut self) {
        // Both slots: either may hold a session pinned against the document being discarded.
        for b in Binding::ALL {
            if holds_session(self.slots[b.ix()].kind) {
                self.slots[b.ix()].tool = make_tool(self.slots[b.ix()].kind);
            }
        }
        // An in-flight stroke's pending cells are pinned against the discarded doc too — Cancel
        // them (dropping the ownership alone would leave them rendering as ghost overlay cells
        // over the new document until the next press), and drop the ownership so a release after
        // the swap can't graft the old document's stroke onto the new one.
        if let Some(b) = self.stroke_owner.take() {
            let tctx = crate::canvas::tool_ctx(self, b);
            self.slots[b.ix()].tool.update(ToolEvent::Cancel, &tctx, &self.doc);
        }
        self.keyboard_owner = None;
    }

    /// Tool-select (`P`/`E`/`I`/`T`/`F`/`R`/`L`/`S`), undo/redo, and Ctrl+C copy keys. Undo/redo/
    /// Copy are `Ctrl`-modified chords and stay global (they won't collide with typing into the
    /// color picker's hex field); the single-letter tool keys are guarded on no widget having
    /// focus *and* not being mid-text-edit so typing into that hex field, or into the canvas in
    /// text mode, doesn't get swallowed as a tool switch.
    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        let owner_kind = self.keyboard_owner().map(|b| self.slot(b).kind);
        let focused = ui.memory(|m| m.focused().is_some()) || suppresses_tool_shortcuts(owner_kind);
        let (redo_shift, undo, redo_y, save, copy_all, copy, export_dialog, fit) = ui.input_mut(|i| {
            // Cmd/Ctrl+Shift+Z must be consumed before the plain Cmd/Ctrl+Z pattern, since
            // `matches_logically` ignores extra Shift/Alt — checking undo first would swallow
            // the redo shortcut's Z key press. Same reasoning for Ctrl+Shift+C vs plain Ctrl+C.
            let redo_shift = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
            let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            let redo_y = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y);
            let save = i.consume_key(egui::Modifiers::COMMAND, egui::Key::S);
            let copy_all = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::C);
            let copy = i.consume_key(egui::Modifiers::COMMAND, egui::Key::C);
            let export_dialog = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::E);
            // Ctrl+0 is a distinct modifier pattern from the Brush's plain `0` (no modifiers) —
            // `matches_logically` requires ctrl/command to be entirely absent for a `NONE`
            // pattern, so the two can never collide regardless of which is consumed first; this
            // is simply where every global chord is consumed.
            let fit = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num0);
            (redo_shift, undo, redo_y, save, copy_all, copy, export_dialog, fit)
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
        if export_dialog {
            self.open_export_dialog();
        }
        if fit {
            self.pending_fit = true;
        }
        // The tool shortcuts come from the TOOLS table, so a tool and its key can never drift
        // apart. A shortcut always sets the L binding; right-clicking a toolbox cell is the only
        // way to set R.
        if !focused {
            let picked = ui.input_mut(|i| {
                TOOLS
                    .iter()
                    .find(|def| i.consume_key(egui::Modifiers::NONE, def.key))
                    .map(|def| def.kind)
            });
            if let Some(kind) = picked {
                self.set_tool(Binding::L, kind);
            }
        }
        if copy_all {
            self.flush_all();
            ui.ctx().copy_text(export_text(&self.doc));
        } else if copy {
            self.copy_selection(ui.ctx());
        }
        // `+`/`=`/`-`, no modifiers: the same zoom step the status bar's buttons and the View menu
        // use. Guarded like the tool-select keys so typing into a focused field never zooms.
        if !focused {
            let (zoom_in, zoom_out) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Plus)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::Equals),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Minus),
                )
            });
            if zoom_in {
                self.step_zoom(1);
            } else if zoom_out {
                self.step_zoom(-1);
            }
        }
        // Ramp/intensity are app-global shared state, so the digit keys apply whenever EITHER
        // binding is holding the Brush.
        if self.bound_to(ToolKind::Brush).is_some() && !focused {
            self.handle_brush_intensity_keys(ui);
        }
        // `[`/`]` adjust the stamp of whichever binding the options bar is showing. That segment is
        // what makes the target unambiguous — and a gesture on either button selects it, so the keys
        // follow the button you last drew with.
        let focus = self.options_focus;
        if let Some(slot) = sized_slot(self.slot(focus).kind) {
            if !focused {
                let (shrink, grow) = ui.input_mut(|i| {
                    (
                        i.consume_key(egui::Modifiers::NONE, egui::Key::OpenBracket),
                        i.consume_key(egui::Modifiers::NONE, egui::Key::CloseBracket),
                    )
                });
                let stamp = &mut self.slots[focus.ix()].stamps[slot];
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


    /// Records `path` at the front of the recent-files list, de-duplicated and capped at 8.
    pub(crate) fn note_recent_file(&mut self, path: &std::path::Path) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(8);
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New…").clicked() {
                    self.flush_all();
                    if self.is_dirty() {
                        self.confirm = Some(PendingConfirm::NewDocument);
                    } else {
                        self.open_new_dialog();
                    }
                }
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
                if ui.add(egui::Button::new("Export…").shortcut_text("Ctrl+Shift+E")).clicked() {
                    self.open_export_dialog();
                }
                ui.separator();
                ui.menu_button("Recent Files", |ui| {
                    if self.recent_files.is_empty() {
                        ui.weak("No recent files");
                    }
                    let mut pick = None;
                    for path in &self.recent_files {
                        let label = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        if ui.button(label).clicked() {
                            pick = Some(path.clone());
                        }
                    }
                    if let Some(path) = pick {
                        self.open_path(&path);
                    }
                });
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
                let can_copy = self
                    .selection_slot()
                    .and_then(|b| self.slot(b).tool.selection_overlay())
                    .and_then(|v| v.marquee)
                    .is_some();
                let copy = egui::Button::new("Copy Selection").shortcut_text("Ctrl+C");
                if ui.add_enabled(can_copy, copy).clicked() {
                    self.copy_selection(ui.ctx());
                }
                let copy_all = egui::Button::new("Copy All as Text").shortcut_text("Ctrl+Shift+C");
                if ui.add(copy_all).clicked() {
                    // Flush first: a pending text burst or floating selection lives only in
                    // `self.slots[0].tool`'s overlay until committed into `self.doc` — copying without
                    // flushing would silently drop just-typed or just-moved content from the
                    // whole-document clipboard contents.
                    self.flush_all();
                    ui.ctx().copy_text(export_text(&self.doc));
                }
                let paste = egui::Button::new("Paste").shortcut_text("Ctrl+V");
                if ui.add(paste).clicked() {
                    self.paste_from_os_clipboard();
                }
                ui.separator();
                if ui.button("Resize Canvas…").clicked() {
                    // Reads self.doc for the current extent, which a pending burst/float doesn't
                    // change (extent is fixed regardless), but flushing keeps the dialog's initial
                    // W/H consistent with whatever's about to be committed anyway.
                    self.flush_all();
                    self.resize_w = self.doc.width;
                    self.resize_h = self.doc.height;
                    // An unrelated error from a prior action (e.g. a dead Recent Files entry)
                    // must not read as if this fresh dialog already failed.
                    self.last_error = None;
                    self.resize_dialog_open = true;
                }
            });
            ui.menu_button("View", |ui| {
                if ui.add(egui::Button::new("Zoom In").shortcut_text("+")).clicked() {
                    self.step_zoom(1);
                }
                if ui.add(egui::Button::new("Zoom Out").shortcut_text("−")).clicked() {
                    self.step_zoom(-1);
                }
                if ui.add(egui::Button::new("Fit").shortcut_text("Ctrl+0")).clicked() {
                    self.pending_fit = true;
                }
                ui.separator();
                ui.checkbox(&mut self.show_grid, "Grid");
                ui.separator();
                ui.menu_button("Theme", |ui| {
                    let mut pref = self.theme_pref;
                    ui.radio_value(&mut pref, egui::ThemePreference::Light, "Light");
                    ui.radio_value(&mut pref, egui::ThemePreference::Dark, "Dark");
                    ui.radio_value(&mut pref, egui::ThemePreference::System, "System");
                    if pref != self.theme_pref {
                        self.theme_pref = pref;
                        ui.ctx().set_theme(pref);
                    }
                });
            });
        });
    }

    /// Reads the OS clipboard on demand (Edit ▸ Paste) via `arboard`. A real Ctrl+V keypress
    /// pastes through `egui::Event::Paste` instead (`canvas.rs`) — this menu item exists because a
    /// menu click is not itself a key event egui surfaces the clipboard on.
    fn paste_from_os_clipboard(&mut self) {
        match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            Ok(text) => self.paste_text(&text),
            Err(e) => self.last_error = Some(format!("paste: clipboard read failed: {e}")),
        }
    }

    /// Zooms by one step, keeping the viewport's own centring (there is no pointer to anchor to
    /// from a menu item, a keyboard chord, or the status bar's buttons — all three call this).
    pub(crate) fn step_zoom(&mut self, dir: i32) {
        let next = (self.viewport.zoom_step as i32 + dir)
            .clamp(0, crate::viewport::ZOOM_SCALES.len() as i32 - 1);
        self.viewport.zoom_step = next as usize;
    }

    fn open_export_dialog(&mut self) {
        // Not the authoritative flush — harmless dialog-open convenience only. The dialog reads
        // `self.doc` again (via the preview and the final "Export…" click), which is what matters.
        self.flush_all();
        self.export_preview = None;
        self.export_preview_key = None;
        // An unrelated prior error must not read as if this fresh dialog already failed.
        self.last_error = None;
        self.export_dialog_open = true;
    }

    /// New Document dialog: width/height steppers, a preset segment, and a background well.
    fn new_dialog(&mut self, ctx: &egui::Context) {
        if !self.new_dialog_open {
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
                crate::ui::widgets::stepper(ui, &mut self.new_w, 1, Document::MAX_WIDTH);
                ui.add_space(12.0);
                ui.label("Height");
                crate::ui::widgets::stepper(ui, &mut self.new_h, 1, Document::MAX_HEIGHT);
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
            DialogAction::Cancel => self.new_dialog_open = false,
            DialogAction::None => {
                if resp.dismissed {
                    self.new_dialog_open = false;
                }
            }
        }
    }

    /// Resize dialog, rebuilt on the shared modal framework: W/H steppers, a 9-way anchor grid,
    /// and the same `resize_document` confirm path as before (now anchor-aware).
    fn resize_dialog(&mut self, ctx: &egui::Context) {
        if !self.resize_dialog_open {
            return;
        }
        let resp = dialog::modal(ctx, "resize_canvas", "Resize Canvas", |ui| {
            ui.label(format!("current: {}×{}", self.doc.width, self.doc.height));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Width");
                crate::ui::widgets::stepper(ui, &mut self.resize_w, 1, Document::MAX_WIDTH);
                ui.add_space(12.0);
                ui.label("Height");
                crate::ui::widgets::stepper(ui, &mut self.resize_h, 1, Document::MAX_HEIGHT);
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
            DialogAction::Cancel => self.resize_dialog_open = false,
            DialogAction::None => {
                if resp.dismissed {
                    self.resize_dialog_open = false;
                }
            }
        }
    }

    /// Rebuilds `self.export_preview` from the current document + export settings, if it isn't
    /// already current. Dropped (not just left stale) whenever the dialog is closed, so the
    /// texture's GPU memory isn't held open between uses.
    fn refresh_export_preview(&mut self, ctx: &egui::Context) {
        if self.export.format != ExportFormat::Png {
            self.export_preview = None;
            self.export_preview_key = None;
            return;
        }
        if self.export_preview_key == Some(self.export) {
            return;
        }
        let opaque_bg = (!self.export.transparent).then_some(self.doc.background);
        // A small, fixed preview scale — independent of the export's own cell_px, which can be up
        // to 4x the base and would make an oversized in-dialog thumbnail.
        if let Ok((w, h, pixels)) = png_export::rasterize_rgba8(&self.doc, 4, opaque_bg) {
            let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
            self.export_preview =
                Some(ctx.load_texture("export_preview", image, egui::TextureOptions::NEAREST));
        }
        self.export_preview_key = Some(self.export);
    }

    /// Unified Export dialog: Text/PNG format, PNG scale + transparency, Text trim, a live
    /// preview, and a pixel/char readout.
    fn export_dialog(&mut self, ctx: &egui::Context) {
        if !self.export_dialog_open {
            return;
        }
        self.refresh_export_preview(ctx);
        let doc = &self.doc;
        let preview = self.export_preview.clone();
        let resp = dialog::modal(ctx, "export", "Export", |ui| {
            let formats = [(ExportFormat::Text, "Text (.txt)"), (ExportFormat::Png, "PNG")];
            crate::ui::widgets::segmented(ui, &mut self.export.format, &formats, false);
            ui.add_space(8.0);

            match self.export.format {
                ExportFormat::Png => {
                    ui.horizontal(|ui| {
                        ui.label("Scale");
                        let scales = [(1u8, "1×"), (2, "2×"), (4, "4×")];
                        crate::ui::widgets::segmented(ui, &mut self.export.scale, &scales, false);
                    });
                    ui.add_space(6.0);
                    crate::ui::widgets::checkbox(ui, &mut self.export.transparent, "Transparent background");
                }
                ExportFormat::Text => {
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
                ExportFormat::Png => {
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
                ExportFormat::Text => {
                    let text = export_text(doc);
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
                ExportFormat::Png => {
                    let px = self.export.cell_px();
                    format!(
                        "{}×{} px · {}× cell scale",
                        doc.width as u32 * px,
                        doc.height as u32 * px,
                        self.export.scale
                    )
                }
                ExportFormat::Text => format!("{}×{} chars", doc.width, doc.height),
            };
            ui.label(egui::RichText::new(readout).font(fonts::mono_id(fonts::size::LABEL)).color(t.fg_secondary));

            if let Some(err) = &self.last_error {
                ui.label(egui::RichText::new(err.clone()).color(t.fg_error));
            }

            ui.add_space(12.0);
            dialog::buttons(ui, "Cancel", "Export…")
        });
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
        self.export_dialog_open = false;
        self.export_preview = None;
        self.export_preview_key = None;
    }

    /// Flushes, opens a native save dialog filtered by the current format, and writes the result.
    /// Reads `self.doc` directly, so it re-flushes even though the dialog-open path already did —
    /// the dialog stays open across frames and its own "Export…" click is the read that matters.
    fn run_export(&mut self) {
        self.flush_all();
        match self.export.format {
            ExportFormat::Text => {
                let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).save_file() else {
                    return;
                };
                let text = if self.export.trim {
                    export_text(&self.doc)
                } else {
                    export_text_untrimmed(&self.doc)
                };
                match std::fs::write(&path, text) {
                    Ok(()) => {
                        self.last_error = None;
                        self.close_export_dialog();
                    }
                    Err(e) => self.last_error = Some(format!("failed to export {}: {e}", path.display())),
                }
            }
            ExportFormat::Png => {
                let Some(path) = rfd::FileDialog::new().add_filter("PNG", &["png"]).save_file() else {
                    return;
                };
                let opaque_bg = (!self.export.transparent).then_some(self.doc.background);
                match png_export::export_png(&self.doc, self.export.cell_px(), opaque_bg) {
                    Ok(bytes) => match std::fs::write(&path, bytes) {
                        Ok(()) => {
                            self.last_error = None;
                            self.close_export_dialog();
                        }
                        Err(e) => self.last_error = Some(format!("failed to write {}: {e}", path.display())),
                    },
                    Err(e) => self.last_error = Some(format!("PNG export failed: {e}")),
                }
            }
        }
    }

    /// Reads and parses a `.gascii` file picked via a native dialog.
    fn open_file(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("GASCII", &["gascii"]).pick_file() else {
            return;
        };
        self.open_path(&path);
    }

    /// Reads and parses a `.gascii` file at `path` (the native-dialog and Recent-Files entry
    /// points both funnel through here). A freshly loaded document starts with an empty undo
    /// history — there is no `before` state for its cells prior to the load. A failed open drops
    /// `path` from `recent_files` rather than leaving a dead entry behind.
    fn open_path(&mut self, path: &std::path::Path) {
        match std::fs::read_to_string(path) {
            Ok(contents) => match load_str(&contents) {
                Ok(doc) => {
                    // Cancel, not flush: the old `self.doc` that any pending work — a burst, a
                    // float, or an in-flight stroke — pinned its `before` values against is about
                    // to be discarded, so committing into it is pointless, and carrying the same
                    // tool instances forward would let them later graft edits, and stale pre-edit
                    // `before` values on Undo, from the discarded document onto the newly loaded
                    // one.
                    self.reset_cross_frame_tool();
                    self.doc = doc;
                    self.history = History::new();
                    // Read from the fresh History rather than hardcoding None, so this stays
                    // correct if History::new()'s starting state ever changes.
                    self.saved_marker = self.history.top_edit_id();
                    self.current_path = Some(path.to_path_buf());
                    self.last_error = None;
                    self.note_recent_file(path);
                }
                Err(e) => {
                    self.last_error = Some(format!("failed to load {}: {e}", path.display()));
                    self.recent_files.retain(|p| p != path);
                }
            },
            Err(e) => {
                self.last_error = Some(format!("failed to read {}: {e}", path.display()));
                self.recent_files.retain(|p| p != path);
            }
        }
    }

    fn save_file(&mut self) {
        // Flush first: Save reads `self.doc` directly, which does not yet contain a pending text
        // burst's just-typed characters or a floating selection's move until a commit trigger
        // fires. Also covers the `save_file_as` delegation below (a no-op double-flush if already
        // flushed).
        self.flush_all();
        match self.current_path.clone() {
            Some(path) => self.write_gascii(&path),
            None => self.save_file_as(),
        }
    }

    fn save_file_as(&mut self) {
        // Flush first — see `save_file`'s comment. Also reachable directly via the "Save As"
        // toolbar button, not only through `save_file`'s delegation.
        self.flush_all();
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
                self.saved_marker = self.history.top_edit_id();
                self.note_recent_file(path);
            }
            Err(e) => self.last_error = Some(format!("failed to save {}: {e}", path.display())),
        }
    }

    /// Runs once per frame near the top of `ui()`. Vetoes the root viewport's close request with a
    /// modal Save/Don't Save/Cancel dialog whenever the document is dirty; lets a clean close (or
    /// the one close this dialog just re-requested via `close_now`) proceed untouched.
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if self.force_close {
            self.force_close = false; // consumed — only this one attempt is exempt
            return; // no CancelClose sent: this close proceeds for real
        }
        // Turn a pending Text burst / floating Selection into a real edit before judging dirtiness
        // — never silently discard in-progress work.
        self.flush_all();
        if self.is_dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(PendingConfirm::CloseApp);
        }
        // Else: clean — don't cancel, eframe closes the window at the end of this frame.
    }

    /// Re-requests a real close after the confirm dialog resolves (Save succeeded, or Don't Save).
    /// `force_close` lets the very next `close_requested` frame through without re-triggering the
    /// veto this dialog just cleared.
    fn close_now(&mut self, ctx: &egui::Context) {
        self.force_close = true;
        self.confirm = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Resets New-dialog state to defaults and opens it. Shared by File ▸ New…'s clean path and the
    /// confirm dialog's `NewDocument` resolution.
    fn open_new_dialog(&mut self) {
        self.new_w = Document::DEFAULT_WIDTH;
        self.new_h = Document::DEFAULT_HEIGHT;
        self.new_bg = Rgba(0, 0, 0, 255);
        self.new_dialog_open = true;
    }

    /// Creates a fresh document from the New dialog's current settings, discarding the old one
    /// (the confirm flow above is what makes that safe to do unconditionally here).
    fn create_new_document(&mut self) {
        self.reset_cross_frame_tool();
        self.doc = Document::new(self.new_w, self.new_h);
        self.doc.background = self.new_bg;
        self.history = History::new();
        self.saved_marker = self.history.top_edit_id();
        self.current_path = None;
        self.pending_fit = true;
        self.new_dialog_open = false;
    }

    /// The Save/Don't Save/Cancel modal shown while `self.confirm` is set. `canvas.rs` and
    /// `handle_keys` are both gated off while any modal is open (`modal_open()`) — this is the only
    /// place a decision here (discarding unsaved work) is irreversible.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(target) = self.confirm else { return };
        let resp = dialog::modal(ctx, "confirm_unsaved", "Unsaved Changes", |ui| {
            ui.label("This document has unsaved changes.");
            ui.add_space(12.0);
            let mut dont_save = false;
            let mut decided = DialogAction::None;
            ui.horizontal(|ui| {
                if crate::ui::widgets::button(ui, "Don't Save", false).clicked() {
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

    /// The window title: `GASCII — <file>`, with a bullet while there are unsaved changes. The
    /// title bar is the only place the current file name is shown.
    pub(crate) fn window_title(&self) -> String {
        let name = self
            .current_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.gascii".to_owned());
        let dirty = if self.is_dirty() { " •" } else { "" };
        format!("GASCII — {name}{dirty}")
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

impl eframe::App for GasciiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.first_frame {
            eprintln!("startup to first frame: {:?}", self.started.elapsed());
            self.first_frame = false;
        }
        let ctx = ui.ctx().clone();
        self.handle_close_request(&ctx);
        if !self.modal_open() {
            self.handle_keys(ui);
        }

        // Only push the title when it actually changes: `SetWindowText` on every frame is a
        // needless syscall, and on Windows it can flicker the taskbar entry.
        let title = self.window_title();
        if title != self.shown_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.shown_title = title;
        }

        let t = crate::ui::theme::current(&ctx);
        // The window edge's resize grips, before the panels: the grip is a 5px ring around the whole
        // window and must win over any widget sitting under it. The canvas reads raw pointer state
        // rather than egui interactions, so it must be told explicitly — a press on the grip would
        // otherwise both begin an OS resize and stamp a stroke on the document in the same click.
        let pointer_on_resize_grip = crate::ui::titlebar::handle_resize(&ctx);

        egui::Panel::top("titlebar")
            .frame(
                egui::Frame::new()
                    .fill(t.bg_panel)
                    .inner_margin(egui::Margin::symmetric(0, 0))
                    .stroke(egui::Stroke::NONE),
            )
            .exact_size(crate::ui::titlebar::HEIGHT)
            .show(ui, |ui| crate::ui::titlebar::show(ui, self));
        egui::Panel::top("menubar")
            .frame(egui::Frame::new().fill(t.bg_panel).inner_margin(egui::Margin::symmetric(8, 0)))
            .exact_size(28.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| self.menu_bar(ui));
            });
        egui::Panel::top("options")
            .frame(egui::Frame::new().fill(t.bg_chrome).inner_margin(egui::Margin::symmetric(12, 0)))
            .exact_size(crate::ui::options_bar::HEIGHT)
            .show(ui, |ui| crate::ui::options_bar::show(ui, self));
        // The status bar is claimed BEFORE the sidebar, so it spans the full window width. Panels
        // take their slice in declaration order: sidebar-first would give the left panel the whole
        // remaining height and leave the status bar starting at x=208.
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(t.bg_panel).inner_margin(egui::Margin::symmetric(12, 0)))
            .exact_size(crate::ui::status_bar::HEIGHT)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| crate::ui::status_bar::show(ui, self));
            });
        egui::Panel::left("sidebar")
            .frame(egui::Frame::new().fill(t.bg_panel).inner_margin(egui::Margin::same(12)))
            .default_size(crate::ui::sidebar::DEFAULT_WIDTH)
            .size_range(crate::ui::sidebar::MIN_WIDTH..=crate::ui::sidebar::MAX_WIDTH)
            .resizable(true)
            .show(ui, |ui| crate::ui::sidebar::show(ui, self));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(t.bg_desk))
            .show(ui, |ui| {
                canvas::show(ui, self, pointer_on_resize_grip);
            });

        self.new_dialog(&ctx);
        self.resize_dialog(&ctx);
        self.export_dialog(&ctx);
        self.confirm_dialog(&ctx);

        // Last, on the foreground layer: with the OS frame gone, nothing else draws the window's
        // own outline.
        crate::ui::titlebar::paint_window_edge(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        prefs::save(storage, self);
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

    const ALL_KINDS: [ToolKind; 9] = [
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

    /// `TOOLS` is the single source of truth for names, shortcuts, hints and constructors. If a
    /// kind were missing, `make_tool`'s `expect` would fire; if one were listed twice, the toolbox
    /// would show a duplicate cell and the two entries could drift apart.
    #[test]
    fn tools_table_lists_every_kind_exactly_once() {
        assert_eq!(TOOLS.len(), ALL_KINDS.len());
        for kind in ALL_KINDS {
            let count = TOOLS.iter().filter(|d| d.kind == kind).count();
            assert_eq!(count, 1, "{kind:?} appears {count} times in TOOLS");
        }
    }

    /// Every kind must be constructible, including Eyedropper — which is not really a tool and is
    /// backed by `InertTool`. A kind that panicked or returned a stale instance here would take
    /// down a binding the moment it was selected.
    #[test]
    fn every_kind_builds_a_tool_with_an_empty_pending_overlay() {
        for kind in ALL_KINDS {
            let tool = make_tool(kind);
            assert!(tool.pending().is_empty(), "{kind:?} starts with a non-empty overlay");
        }
    }

    /// Shortcuts must be unique, or one tool would be unreachable from the keyboard: `handle_keys`
    /// consumes the first match and the loser would silently never fire.
    #[test]
    fn tool_shortcuts_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for def in TOOLS.iter() {
            assert!(seen.insert(def.key), "{:?} reuses shortcut {:?}", def.kind, def.key);
        }
    }

    /// Both bindings start bound, and to different tools — exactly one tool is bound to L and one
    /// to R at all times; there is no unbound state.
    #[test]
    fn default_bindings_are_pencil_on_l_and_eraser_on_r() {
        let slots = [ToolSlot::new(ToolKind::Pencil), ToolSlot::new(ToolKind::Eraser)];
        assert_eq!(slots[Binding::L.ix()].kind, ToolKind::Pencil);
        assert_eq!(slots[Binding::R.ix()].kind, ToolKind::Eraser);
    }

    /// Each binding keeps its own footprint memory, so sizing the right button's Eraser must not
    /// resize the left button's. Structural here — the two slots own separate arrays — but this
    /// pins it against a refactor that reintroduces a shared one.
    #[test]
    fn stamps_are_per_slot_so_sizing_rs_eraser_never_resizes_ls() {
        let mut slots = [ToolSlot::new(ToolKind::Eraser), ToolSlot::new(ToolKind::Eraser)];
        let eraser = sized_slot(ToolKind::Eraser).expect("Eraser is sized");
        slots[Binding::R.ix()].stamps[eraser].size = 9;
        assert_eq!(slots[Binding::R.ix()].stamp().size, 9);
        assert_eq!(slots[Binding::L.ix()].stamp().size, 1, "L's Eraser was resized by R's");
    }

    /// A slot's stamp follows whatever it is bound to, and unsized kinds fall back to the identity
    /// default rather than borrowing another tool's size.
    #[test]
    fn a_slots_stamp_tracks_its_own_kind() {
        let mut slot = ToolSlot::new(ToolKind::Pencil);
        slot.stamps[sized_slot(ToolKind::Pencil).unwrap()].size = 5;
        slot.stamps[sized_slot(ToolKind::Brush).unwrap()].size = 12;
        assert_eq!(slot.stamp().size, 5);
        slot.kind = ToolKind::Brush;
        assert_eq!(slot.stamp().size, 12);
        slot.kind = ToolKind::Fill; // unsized
        assert_eq!(slot.stamp().size, StampSettings::default().size);
    }

    /// Overlay order is commit order: a slot mid-gesture commits at its imminent release, so it
    /// paints underneath the other slot's session, which commits later. Pure over the stroke
    /// owner, so `flush_all` and the painter provably agree.
    #[test]
    fn commit_order_puts_the_gesture_slot_first() {
        assert_eq!(order_for(None), [Binding::L, Binding::R]);
        assert_eq!(order_for(Some(Binding::L)), [Binding::L, Binding::R]);
        assert_eq!(order_for(Some(Binding::R)), [Binding::R, Binding::L]);
    }

    /// A paste lands on a binding already holding Selection rather than rebinding one, and falls
    /// back to L (never R) when neither does — silently rebinding the right button out from under
    /// the user is worse than rebinding the left.
    #[test]
    fn paste_target_prefers_an_existing_selection_binding_over_rebinding() {
        use ToolKind::{Pencil, Selection};
        assert_eq!(paste_target(Selection, Pencil), Binding::L);
        assert_eq!(paste_target(Pencil, Selection), Binding::R, "should not clobber L's binding");
        assert_eq!(paste_target(Selection, Selection), Binding::L, "L wins when both qualify");
        assert_eq!(paste_target(Pencil, Pencil), Binding::L, "falls back to L, never R");
    }

    /// The reachable half of the two-slot resync obligation, and the reason `apply_edit` exists.
    ///
    /// A stroke on one binding commits straight into the document, underneath a *session* held by
    /// the other. That leaves the session's pinned `before` values describing a document state that
    /// no longer exists. Undo restores `before`, so a missed resync shows up as undo resurrecting
    /// pre-stroke content and silently destroying what the other binding drew.
    ///
    /// Here: R's Pencil draws '#' under L's live text burst, then the burst commits and is undone.
    /// Undo must restore R's '#', not the blank that was there when the burst started.
    #[test]
    fn a_strokes_commit_repins_the_other_bindings_live_session() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.keyboard_owner = Some(Binding::L);

        // L: place a caret at (0,0) and type — the burst pins `before` = Blank at (0,0).
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('A'), &l, &app.doc);

        // R: a pencil stroke commits '#' into (0,0), beneath the burst.
        app.active_glyph = '#';
        let r = crate::canvas::tool_ctx(&app, Binding::R);
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &r, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, '#', "the pencil stroke landed");

        // L's burst commits its 'A' over the top, then undo rolls it back.
        app.flush_slot(Binding::L);
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'A', "the burst committed");
        app.history.undo(&mut app.doc);

        assert_eq!(
            app.doc.cell(0, 0, 0).unwrap().ch,
            '#',
            "undo restored a stale pre-stroke `before`, destroying what the other binding drew"
        );
    }

    /// At most one cross-frame session exists across both bindings, so `flush_all`'s second flush
    /// has nothing to commit. Pins the invariant that makes two Selection bindings coherent (never
    /// two floats) and keeps `selection_slot` — hence "the selection" — singular.
    #[test]
    fn a_slot_holding_a_session_is_the_only_one() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Selection);

        // A press on L starts a marquee and claims the keyboard.
        crate::canvas::begin_gesture(&mut app, Binding::L, 1, 1);
        assert_eq!(app.keyboard_owner, Some(Binding::L));
        assert_eq!(app.selection_slot(), Some(Binding::L));

        // A press on R takes over: ownership moves, and it is still the only session.
        crate::canvas::begin_gesture(&mut app, Binding::R, 4, 4);
        assert_eq!(app.keyboard_owner, Some(Binding::R));
        assert_eq!(app.selection_slot(), Some(Binding::R), "two selections would be ambiguous");
    }

    /// Rebinding a slot releases only its own claim on the keyboard. Clearing the claim globally
    /// would mute a live session on the other binding, which nothing would then re-acquire.
    #[test]
    fn rebinding_releases_only_its_own_keyboard_claim() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Text);
        app.keyboard_owner = Some(Binding::R);

        app.set_tool(Binding::L, ToolKind::Fill);
        assert_eq!(app.keyboard_owner, Some(Binding::R), "rebinding L muted R's session");

        app.set_tool(Binding::R, ToolKind::Fill);
        assert_eq!(app.keyboard_owner, None, "rebinding R should release its own claim");
    }

    /// Every kind is bindable to either button — Text, Selection and Eyedropper included.
    #[test]
    fn every_kind_can_bind_to_either_button() {
        for kind in ALL_KINDS {
            for b in Binding::ALL {
                let mut app = GasciiApp::headless();
                app.set_tool(b, kind);
                assert_eq!(app.slot(b).kind, kind, "{kind:?} would not bind to {b:?}");
            }
        }
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

    #[test]
    fn edit_marker_differs_is_clean_when_both_markers_are_none() {
        assert!(!edit_marker_differs(None, None));
    }

    #[test]
    fn edit_marker_differs_is_clean_when_current_matches_saved() {
        assert!(!edit_marker_differs(Some(3), Some(3)));
    }

    #[test]
    fn edit_marker_differs_is_dirty_when_current_and_saved_diverge() {
        assert!(edit_marker_differs(Some(3), Some(4)));
    }

    #[test]
    fn edit_marker_differs_is_dirty_when_current_is_some_but_saved_is_none() {
        assert!(edit_marker_differs(Some(0), None));
    }

    /// Pure-function coverage over every `ToolKind` plus `None`: only a Text-owning keyboard
    /// suppresses tool-select shortcuts — `SelectionTool`'s `Char` arm falls through to a no-op, so
    /// every other owning kind (and no owner at all) must leave shortcuts live.
    #[test]
    fn suppresses_tool_shortcuts_is_true_only_for_text() {
        for kind in ALL_KINDS {
            let expected = kind == ToolKind::Text;
            assert_eq!(suppresses_tool_shortcuts(Some(kind)), expected, "{kind:?}");
        }
        assert!(!suppresses_tool_shortcuts(None));
    }

    /// `flush_slot` commits pending work but never releases the keyboard — that is `end_session`'s
    /// job. A flushed Text burst must still hold the keyboard, and its caret must still be placed,
    /// right after the flush.
    #[test]
    fn flush_slot_never_releases_keyboard_ownership() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.acquire_keyboard(Binding::L);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('a'), &tctx, &app.doc);

        app.flush_slot(Binding::L);

        assert_eq!(app.keyboard_owner(), Some(Binding::L), "flush must never release the keyboard");
        assert!(
            app.slots[Binding::L.ix()].tool.caret().is_some(),
            "the burst's cursor must still be placed after a flush"
        );
    }

    /// A flush commits the session's pending work even while its own binding is mid-stroke: every
    /// flush caller either reads the document right after (save, the close-confirm dirty check,
    /// copy) or follows up with a `Cancel` — a gated flush would hand them a document missing work
    /// the user can see, or let the `Cancel` discard it. The scenario: a pasted float is being
    /// dragged into place when the window is asked to close.
    #[test]
    fn a_mid_stroke_flush_commits_the_float_so_the_dirty_check_sees_it() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let patch = CellPatch { width: 1, height: 1, cells: vec![cell('x')] };
        app.slots[Binding::L.ix()].tool.accept_stamp(patch, (3, 3), &app.doc);
        app.acquire_keyboard(Binding::L);

        // Grab the float: the press starts a Move stroke and takes stroke ownership.
        crate::canvas::begin_gesture(&mut app, Binding::L, 3, 3);
        assert_eq!(app.stroke_owner, Some(Binding::L), "sanity: L is mid-stroke");
        assert!(!app.is_dirty(), "sanity: nothing committed yet");

        // Alt+F4 / Ctrl+S while the button is still held.
        app.flush_all();

        assert_eq!(app.doc.cell(0, 3, 3).unwrap().ch, 'x', "the float must commit at its current spot");
        assert!(app.is_dirty(), "the close-confirm dirty check must see the committed float");
    }

    /// `end_session` commits before it clears, even when the binding owns the in-flight stroke —
    /// Escape pressed while the pointer is still held must never discard what was typed during the
    /// hold.
    #[test]
    fn end_session_commits_pending_work_even_for_the_stroke_owning_binding() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0);
        assert_eq!(app.stroke_owner, Some(Binding::L), "sanity: the press is still held");
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &tctx, &app.doc);

        app.end_session(Binding::L); // Escape mid-hold

        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'h', "the held-press burst must commit, not vanish");
        assert_eq!(app.keyboard_owner(), None, "the session is over");
    }

    /// Ctrl+C internally calls `flush_all`, which must not silently drop the marquee or the
    /// keyboard claim — Delete right afterward must still see the selection and blank it, or the
    /// standard copy-then-delete cut workflow dies at its second step.
    #[test]
    fn ctrl_c_then_delete_workflow_survives_a_flush() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        app.doc.set_cell(0, 1, 1, cell('x'));
        app.doc.set_cell(0, 2, 2, cell('y'));

        let egui_ctx = egui::Context::default();
        app.copy_selection(&egui_ctx); // internally calls flush_all

        assert_eq!(
            app.selection_slot(),
            Some(Binding::L),
            "a flush triggered by copy must not clear the selection slot"
        );
        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().and_then(|v| v.marquee).is_some(),
            "the marquee must survive a structural flush"
        );

        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Delete, &tctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) = resp {
            app.apply_edit(edit, Some(Binding::L));
        }
        for y in 1..=2u16 {
            for x in 1..=2u16 {
                assert_eq!(app.doc.cell(0, x, y), Some(&gascii_core::Cell::BLANK));
            }
        }
    }

    /// A structural flush (Ctrl+S/Ctrl+Z) mid-burst must not release the keyboard, or the very
    /// next typed letter would be consumed as a tool-select shortcut instead of burst content.
    #[test]
    fn mid_typing_structural_flush_does_not_let_the_next_letter_rebind_the_tool() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('a'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        app.flush_all(); // simulates the Ctrl+S / Ctrl+Z structural-trigger path

        let owner_kind = app.keyboard_owner().map(|b| app.slot(b).kind);
        assert_eq!(owner_kind, Some(ToolKind::Text), "a structural flush must not release the keyboard mid-burst");
        assert!(
            suppresses_tool_shortcuts(owner_kind),
            "the very next 's' keypress must still be swallowed as burst content, not routed to set_tool"
        );
    }

    /// Starting a session on the other binding must fully clear the losing slot's marquee, not
    /// merely leave it behind to be masked by render/commit ordering — a lingering invisible
    /// marquee is what keyboard Delete would silently operate on.
    #[test]
    fn starting_a_selection_session_on_the_other_binding_clears_the_losing_slots_marquee() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Selection);

        // A press on L starts a marquee and claims the keyboard.
        crate::canvas::begin_gesture(&mut app, Binding::L, 1, 1);
        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().and_then(|v| v.marquee).is_some(),
            "sanity: L has a marquee"
        );

        // A press on R takes over: L's session must be fully ended, not just masked.
        crate::canvas::begin_gesture(&mut app, Binding::R, 4, 4);

        assert_eq!(app.keyboard_owner(), Some(Binding::R));
        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().is_none(),
            "the losing slot's marquee must be cleared, not merely masked by render order"
        );
    }

    /// A flush landing on the idle binding mid-stroke leaves the stroking binding holding pending
    /// cells composed against the pre-flush document; its own eventual commit must not revert the
    /// just-flushed content on a masked-off plane. The app-integration face of the resync
    /// contract (the tool-level pin lives in `gascii-core`).
    #[test]
    fn a_strokes_commit_mid_gesture_repins_a_flushed_idle_slots_masked_plane() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.acquire_keyboard(Binding::L);

        // L: place a caret at (0,0) and type — commits 'A' once flushed.
        app.mask = PlaneMask::ALL;
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('A'), &l, &app.doc);

        // R: a glyph-masked-off Pencil stroke touches (0,0) and keeps gesturing — no Release yet.
        app.mask = PlaneMask { glyph: false, bg: true };
        app.active_glyph = '#';
        app.stroke_owner = Some(Binding::R);
        let r = crate::canvas::tool_ctx(&app, Binding::R);
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &r, &app.doc);

        // A same-frame flush lands on L mid-R-stroke (Escape/Ctrl+C mid-R-stroke): commits 'A'.
        app.flush_slot(Binding::L);
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'A', "L's burst committed under R's live stroke");

        // R's stroke moves on WITHOUT revisiting (0,0). Deliberate: a revisit re-stamps the cell
        // and recomposes as a side effect, hiding a resync that fixed only future stamps — the
        // corruption lives precisely in the already-stamped, never-revisited pending cell.
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Drag { x: 2, y: 0 }, &r, &app.doc);

        app.stroke_owner = None;
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }

        assert_eq!(
            app.doc.cell(0, 0, 0).unwrap().ch,
            'A',
            "R's stroke must not silently revert L's committed glyph on the masked-off plane"
        );
    }

    /// The full copy-paste-drag-save cross-feature flow: a pasted float is mid-drag when Save
    /// fires. The save's flush must commit the float at its current (dragged) position, the saved
    /// file must reflect that position, and the session must stay coherent afterward — the
    /// keyboard claim survives (it's still residue, not a discard), and a further press starts a
    /// clean new marquee rather than getting stuck referencing the just-committed float.
    #[test]
    fn copy_paste_drag_then_save_mid_drag_commits_the_float_and_the_session_stays_interactive() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        app.doc.set_cell(0, 1, 1, cell('x'));

        // Select the single cell and copy it.
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        let egui_ctx = egui::Context::default();
        app.copy_selection(&egui_ctx);
        let copied_text = app.internal_clipboard.as_ref().unwrap().to_text();

        // Paste: lands as a floating stamp at the hovered cell (the origin — nothing is hovered).
        app.paste_text(&copied_text);
        assert_eq!(app.selection_slot(), Some(Binding::L));
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, ' ', "sanity: a paste floats, it doesn't write yet");

        // Grab the float and drag it.
        assert!(crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0), "the press on the float starts a drag");
        assert_eq!(app.stroke_owner, Some(Binding::L));
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &app.doc);

        // Ctrl+S while the button is still held.
        let dir = scratch_dir("mid_drag_save");
        let path = dir.join("out.gascii");
        app.current_path = Some(path.clone());
        app.save_file();

        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, 'x', "the float committed at its dragged position");
        let saved_doc = load_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved_doc.cell(0, 2, 2).unwrap().ch, 'x', "the saved file reflects the dragged position");

        // The session/keyboard state stays coherent afterward: still residue, not a discard.
        assert_eq!(app.keyboard_owner(), Some(Binding::L), "the flush must not release the keyboard mid-drag");

        // The physical button releases a beat later; interaction continues cleanly from there.
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.stroke_owner = None;
        let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &app.doc);
        assert!(matches!(resp, ToolResponse::Active), "a fresh press must start a clean marquee, not error");
        assert!(
            app.slots[Binding::L.ix()]
                .tool
                .selection_overlay()
                .and_then(|v| v.marquee)
                .is_some_and(|r| r.contains(5, 5)),
            "the new marquee must not still be referencing the committed float"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The full cut workflow end to end — select, copy (a structural flush), delete, undo, redo —
    /// with content asserted at every step, not just the final state.
    #[test]
    fn the_cut_workflow_copy_delete_undo_redo_preserves_content_at_every_step() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        app.doc.set_cell(0, 1, 1, cell('x'));
        app.doc.set_cell(0, 2, 2, cell('y'));

        let egui_ctx = egui::Context::default();
        app.copy_selection(&egui_ctx); // Ctrl+C: a structural flush must not disturb the marquee.
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "copy must not itself mutate the document");
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, 'y');

        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Delete, &tctx, &app.doc);
        let ToolResponse::Commit(Some(edit)) = resp else { panic!("Delete must produce a committed edit") };
        app.apply_edit(edit, Some(Binding::L));
        for (x, y) in [(1u16, 1u16), (2, 2)] {
            assert_eq!(app.doc.cell(0, x, y), Some(&gascii_core::Cell::BLANK), "cut must blank the region");
        }

        app.request_undo();
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "undo restores the cut content");
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, 'y', "undo restores the cut content");

        app.request_redo();
        for (x, y) in [(1u16, 1u16), (2, 2)] {
            assert_eq!(app.doc.cell(0, x, y), Some(&gascii_core::Cell::BLANK), "redo re-applies the cut");
        }
    }

    /// `request_redo` deliberately skips flushing first (see its own doc comment), so a live burst
    /// can still be pending when a Redo mutates the document out from under it on the *other*
    /// binding. The resync fan-out that follows must reach that live burst too, not just a flush's
    /// targets — and, on a masked-off plane, recompose its pending content, not merely re-pin
    /// `before`.
    #[test]
    fn redoing_the_other_bindings_stroke_resyncs_a_live_burst_preserving_its_masked_off_plane() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);

        // R draws a colored cell, full mask.
        app.mask = PlaneMask::ALL;
        app.active_glyph = '#';
        app.active_bg = Rgba(1, 2, 3, 255);
        let r = crate::canvas::tool_ctx(&app, Binding::R);
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &r, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().bg, Rgba(1, 2, 3, 255), "sanity: R's stroke landed");

        app.request_undo(); // Ctrl+Z: reverts R's stroke back to Blank.
        assert_eq!(app.doc.cell(0, 0, 0), Some(&gascii_core::Cell::BLANK), "sanity: undo reverted R's stroke");

        // L starts a burst at the now-blank cell, writing only the glyph plane — the bg plane
        // composes from whatever `before` turns out to be.
        app.mask = PlaneMask { glyph: true, bg: false };
        app.acquire_keyboard(Binding::L);
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('B'), &l, &app.doc);

        app.request_redo(); // Ctrl+Shift+Z: redoes R's stroke, without flushing L's live burst first.
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().bg, Rgba(1, 2, 3, 255), "sanity: redo restored R's stroke");

        app.flush_slot(Binding::L);
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'B', "the burst's glyph committed");
        assert_eq!(
            app.doc.cell(0, 0, 0).unwrap().bg,
            Rgba(1, 2, 3, 255),
            "the burst's masked-off bg plane must carry the redo's color, not a pre-redo stale value"
        );
    }

    /// Rebinding the OTHER binding through several kinds must never disturb a live burst — only
    /// rebinding the burst's OWN binding may touch it, and when it does, it must commit rather than
    /// discard.
    #[test]
    fn rebinding_the_other_binding_through_several_kinds_leaves_a_live_burst_untouched_then_rebinding_its_own_binding_commits_it(
    ) {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);

        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &l, &app.doc);
        app.acquire_keyboard(Binding::L);

        for kind in [ToolKind::Eraser, ToolKind::Fill, ToolKind::Selection, ToolKind::Brush, ToolKind::Line] {
            app.set_tool(Binding::R, kind);
            assert_eq!(app.slot(Binding::R).kind, kind, "R must actually rebind to {kind:?}");
            assert_eq!(app.keyboard_owner(), Some(Binding::L), "R's rebind must not touch L's session");
            assert!(
                app.slots[Binding::L.ix()].tool.caret().is_some(),
                "L's caret must survive R's rebind to {kind:?}"
            );
        }

        // Continue typing on L: the burst is unaffected by any of R's rebinds.
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('i'), &l, &app.doc);

        // Rebinding L itself must commit the burst, not discard it.
        app.set_tool(Binding::L, ToolKind::Pencil);
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'h', "rebinding L must commit, not discard, the burst");
        assert_eq!(app.keyboard_owner(), None, "L released its own claim");
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil);
    }

    /// Opening a file must strand neither a live Session (a Text burst) nor an in-flight Stroke (a
    /// Pencil drag still held) that exist simultaneously on the two bindings — nothing grafts onto
    /// the newly loaded document, and neither binding's ownership claim survives the swap.
    #[test]
    fn opening_a_file_strands_neither_a_live_burst_nor_an_in_flight_stroke_onto_the_new_document() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);

        // L: a live burst, pinned against the document that's about to be discarded.
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &l, &app.doc);
        app.acquire_keyboard(Binding::L);

        // R: a pencil stroke still physically held when Open fires.
        assert!(crate::canvas::begin_gesture(&mut app, Binding::R, 2, 2));
        let r = crate::canvas::tool_ctx(&app, Binding::R);
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Drag { x: 3, y: 2 }, &r, &app.doc);
        assert_eq!(app.stroke_owner, Some(Binding::R), "sanity: R is mid-stroke");

        // Open: Cancel (not flush) the pending tools, then swap the document — mirrors `open_file`
        // minus the native file dialog.
        let extent = app.doc.extent();
        app.reset_cross_frame_tool();
        app.doc = Document::new(extent.width, extent.height);
        app.history = History::new();

        assert_eq!(app.doc.cell(0, 0, 0), Some(&gascii_core::Cell::BLANK), "L's burst must not have committed");
        assert_eq!(app.doc.cell(0, 2, 2), Some(&gascii_core::Cell::BLANK), "R's in-flight stroke must not have committed");
        assert_eq!(app.stroke_owner, None, "R's in-flight stroke claim must not survive Open");
        assert_eq!(app.keyboard_owner(), None, "L's session claim must not survive Open");
        assert!(app.slots[Binding::L.ix()].tool.caret().is_none(), "L's caret must not survive Open");
        assert!(app.slots[Binding::R.ix()].tool.pending().is_empty(), "R's in-flight stroke cells must not survive Open");

        // A fresh press on the new document behaves like a clean start.
        let l2 = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &l2, &app.doc);
        assert!(app.slots[Binding::L.ix()].tool.caret().is_some(), "the new Text instance is interactive");
    }

    /// `note_recent_file` mirrors `push_recent`'s contract: most-recent-first, de-duplicated,
    /// capped — re-opening an already-listed path must move it to the front, not add a duplicate.
    #[test]
    fn note_recent_file_is_most_recent_first_deduplicated_and_capped_at_eight() {
        let mut app = GasciiApp::headless();
        for i in 0..10 {
            app.note_recent_file(&PathBuf::from(format!("{i}.gascii")));
        }
        assert_eq!(app.recent_files.len(), 8, "capped at 8 entries");
        assert_eq!(app.recent_files[0], PathBuf::from("9.gascii"), "most recent is first");
        assert_eq!(app.recent_files[7], PathBuf::from("2.gascii"), "oldest surviving entry");

        let reopened = PathBuf::from("5.gascii");
        app.note_recent_file(&reopened); // already present, mid-list
        assert_eq!(app.recent_files[0], reopened, "re-opening moves it to the front");
        assert_eq!(
            app.recent_files.iter().filter(|p| **p == reopened).count(),
            1,
            "must not duplicate an already-listed path"
        );
        assert_eq!(app.recent_files.len(), 8, "re-adding an existing entry does not grow the list");
    }

    /// A failed re-open (`open_path` reading a path that no longer exists) must drop that entry
    /// from `recent_files` rather than leaving a dead path the user can never successfully open.
    #[test]
    fn a_failed_reopen_drops_the_path_from_recent_files() {
        let mut app = GasciiApp::headless();
        let missing = std::env::temp_dir().join("gascii_definitely_missing_file.gascii");
        app.note_recent_file(&missing);
        assert!(app.recent_files.contains(&missing));

        app.open_path(&missing);

        assert!(!app.recent_files.contains(&missing), "a failed open must drop the dead entry");
        assert!(app.last_error.is_some());
    }

    /// The Export dialog's cell-px mapping: `16 * {1, 2, 4}` (D9), pinned so a future change to
    /// the base or the offered scales is a deliberate, visible edit here.
    #[test]
    fn export_cell_px_maps_scale_to_16x_32x_64x() {
        for (scale, expected) in [(1u8, 16u32), (2, 32), (4, 64)] {
            let settings = ExportSettings { scale, ..ExportSettings::default() };
            assert_eq!(settings.cell_px(), expected);
        }
    }

    /// `step_zoom` clamps at both ends of `ZOOM_SCALES` rather than panicking or wrapping —
    /// exercised via the same method the View menu, the keyboard chords, and the status bar's
    /// buttons all now share.
    #[test]
    fn step_zoom_clamps_at_both_ends_of_the_scale_list() {
        let mut app = GasciiApp::headless();
        app.viewport.zoom_step = 0;
        app.step_zoom(-1);
        assert_eq!(app.viewport.zoom_step, 0, "must not go below the smallest step");

        app.viewport.zoom_step = crate::viewport::ZOOM_SCALES.len() - 1;
        app.step_zoom(1);
        assert_eq!(app.viewport.zoom_step, crate::viewport::ZOOM_SCALES.len() - 1, "must not exceed the largest step");
    }

    /// `modal_open()` is the one gate `canvas.rs`'s raw-input polling relies on — it must report
    /// true for every dialog flag independently, and false only when none are set.
    #[test]
    fn modal_open_is_true_while_any_dialog_flag_is_set() {
        let mut app = GasciiApp::headless();
        assert!(!app.modal_open());

        app.confirm = Some(PendingConfirm::CloseApp);
        assert!(app.modal_open());
        app.confirm = None;

        app.resize_dialog_open = true;
        assert!(app.modal_open());
        app.resize_dialog_open = false;

        app.export_dialog_open = true;
        assert!(app.modal_open());
        app.export_dialog_open = false;

        app.new_dialog_open = true;
        assert!(app.modal_open());
        app.new_dialog_open = false;

        assert!(!app.modal_open());
    }

    /// The Export dialog's "Trim trailing spaces" checkbox toggles between two different text
    /// export functions (`export_text`, trimmed; `export_text_untrimmed`, padded) — this pins that
    /// the two genuinely diverge on a document with both a full-width row and a row with real
    /// trailing whitespace, so a future refactor that accidentally routes both dialog paths through
    /// the same function is caught here rather than only visually in the export preview.
    #[test]
    fn export_trim_checkbox_toggles_between_trimmed_and_full_width_padded_rows() {
        let mut doc = Document::new(5, 2);
        // Row 0: full-width content, no trailing blanks -- trim must be a no-op here.
        for x in 0..5u16 {
            doc.set_cell(0, x, 0, cell('#'));
        }
        // Row 1: content only in the first two columns, rest genuinely blank -- trim removes the
        // trailing three columns; untrimmed keeps the row padded to the full document width.
        doc.set_cell(0, 0, 1, cell('a'));
        doc.set_cell(0, 1, 1, cell('b'));

        let trimmed = export_text(&doc);
        let untrimmed = export_text_untrimmed(&doc);

        assert_eq!(trimmed, "#####\nab", "trim must drop row 1's trailing blanks but leave the full row untouched");
        assert_eq!(untrimmed, "#####\nab   ", "untrimmed must pad row 1 to the full document width");
        assert_ne!(trimmed, untrimmed, "the two export paths must genuinely diverge for this document");
    }

    /// The New dialog's background color well (`new_bg`) must land on the freshly created
    /// document's `background` field, not just sit as inert dialog state -- the one place this
    /// wiring is exercised outside a full GUI run.
    #[test]
    fn create_new_document_carries_the_dialog_background_onto_the_fresh_document() {
        let mut app = GasciiApp::headless();
        app.new_w = 12;
        app.new_h = 6;
        app.new_bg = Rgba(1, 2, 3, 255);
        app.create_new_document();

        assert_eq!((app.doc.width, app.doc.height), (12, 6));
        assert_eq!(app.doc.background, Rgba(1, 2, 3, 255));
        assert!(!app.new_dialog_open, "creating the document must close the dialog");
        assert!(!app.history.can_undo(), "a fresh document starts with empty history");
    }
}
