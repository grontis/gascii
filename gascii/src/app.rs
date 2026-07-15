use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use gascii_core::{
    builtin_pages, builtin_ramps, export_text, load_str, resize_document,
    save_string, BrushShape, CellPatch, DensityBrush, DensityMode, Document,
    Eraser, Fixed, FloodFill, History, Line, Page, Pencil, PlaneMask, Ramp, Rectangle, ResizeError,
    Rgba, SelectionTool, TextTool, Tool, ToolEvent, ToolResponse, WidthReject, MAX_TOOL_SIZE,
};

use crate::canvas::{self, CanvasRenderer, NaiveRenderer};
use crate::fonts;
use crate::png_export;
use crate::viewport::Viewport;

/// PNG export cell-scale presets offered to the user, in pixels per cell.
const PNG_SCALE_PRESETS: [u32; 5] = [8, 16, 24, 32, 48];

/// Whether a pasted `Event::Paste` text is still the app's own copy: the OS clipboard is "ours"
/// exactly when `internal`'s own flattening still matches what came back on paste. Pulled out of
/// `paste_text` as a pure function so the copy/paste reconciliation decision is unit-testable
/// without constructing a full `GasciiApp`.
fn is_own_clipboard_text(text: &str, internal: Option<&CellPatch>) -> bool {
    internal.is_some_and(|p| p.to_text() == text)
}

/// Whether the document has changed since the last save/load: true whenever the undo stack's
/// current top-edit id doesn't match the id recorded at that save/load. Pulled out as a pure
/// function, mirroring `is_own_clipboard_text`, so the comparison is unit-testable without a live
/// `GasciiApp`; `GasciiApp::is_dirty` is the thin method wrapping it.
fn edit_marker_differs(current: Option<u64>, saved: Option<u64>) -> bool {
    current != saved
}

/// How many glyphs the RECENT row remembers, per spec §5.
pub(crate) const RECENT_GLYPHS: usize = 6;

/// Pushes `ch` to the front of a most-recent-first list, de-duplicated and capped.
///
/// Pure, so the ordering rule is testable without a `GasciiApp`: re-using a glyph already in the
/// list must move it to the front rather than add a second copy, or the row fills with duplicates
/// and stops being six *distinct* recent glyphs.
fn push_recent(recent: &mut Vec<char>, ch: char) {
    recent.retain(|&c| c != ch);
    recent.insert(0, ch);
    recent.truncate(RECENT_GLYPHS);
}

/// The binding a pasted float lands in: whichever is already bound to Selection (L wins if both),
/// else L — rebound, matching the pre-slot behavior.
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

/// The order the two bindings commit, given which one (if any) the pointer is currently driving.
///
/// Overlay order *is* commit order: an overlay is a promise about the document's final state, and
/// the last committer wins any overlapped cell — so the last committer must paint on top. A slot
/// mid-gesture commits at its imminent release, before any idle slot's session reaches its next
/// structural trigger; so the gesturing slot goes first, and underneath.
///
/// Pure, mirroring `is_own_clipboard_text` and `edit_marker_differs`, so the rule is testable
/// without a live `GasciiApp` — and so `flush_all` and the painter cannot disagree about it.
fn order_for(gesture: Option<Binding>) -> [Binding; 2] {
    match gesture {
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
    /// The transient tool instance a secondary-button stroke drives, `Some` only while that
    /// stroke is in flight. `self.slots[0].tool` (and any pending text burst or floating selection) stays
    /// untouched underneath it.
    /// Which slot's tool the pointer is currently driving, if any. Gesture ownership is one
    /// question, so it is one field — which is what let the press/drag/release paths collapse to a
    /// single parameterized call site. At most one gesture is live across both buttons.
    pub(crate) gesture: Option<Binding>,
    pub(crate) space_pan_active: bool,
    /// Which slot's tool receives keystrokes. There is one keyboard and both slots can be bound to
    /// keyboard-driven tools, so ownership is explicit state rather than something derived: it is
    /// acquired by a canvas press on a Text/Selection slot (or by paste), and released when that
    /// slot's session ends or its binding changes.
    ///
    /// Deliberately not derived from tool state. Escape ends a text session while `TextTool` keeps
    /// its cursor placed, so "has a caret" and "is accepting keys" genuinely differ. It also gates
    /// every single-letter tool-select key, so typing never switches tools.
    pub(crate) keyboard_owner: Option<Binding>,
    /// Previous frame's window-focus state, for edge-detecting focus loss.
    pub(crate) was_focused: bool,
    /// The last region copied via Ctrl+C, kept alongside the plain text written to the OS
    /// clipboard. A paste whose `Event::Paste` text still matches this patch's own flattening
    /// pastes the colored version; otherwise it's treated as external plain text.
    pub(crate) internal_clipboard: Option<CellPatch>,
    pub(crate) pages: Vec<Page>,
    pub(crate) active_page: usize,
    /// The last [`RECENT_GLYPHS`] glyphs used, most recent first. Fed by picking a swatch and by
    /// actually drawing with one, per workflow W4.
    pub(crate) recent_glyphs: Vec<char>,
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
    pub(crate) last_error: Option<String>,
    /// The undo-stack edit id (`History::top_edit_id`) at the moment of the last successful save
    /// or load — `None` matches a fresh `History`'s own sentinel. `is_dirty` is a pure comparison
    /// against `self.history.top_edit_id()`; nothing else needs to know about this field.
    saved_marker: Option<u64>,
    /// The close-confirm dialog (Save / Don't Save / Cancel) is showing. `pub(crate)` because
    /// `canvas.rs`'s modality guard reads it directly (see `canvas::show`).
    pub(crate) close_dialog_open: bool,
    /// Single-use: lets the very next `close_requested` frame through unconditionally, then resets
    /// itself. Set by `close_now` so "Save" and "Don't Save" can re-request a real close without
    /// re-triggering the veto they just cleared.
    force_close: bool,
    /// The title last pushed to the OS, so it is only sent when it changes.
    shown_title: String,
    started: Instant,
    first_frame: bool,
}

impl GasciiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, started: Instant) -> Self {
        fonts::install_fonts(&cc.egui_ctx);
        crate::ui::theme::install(&cc.egui_ctx);
        Self::with_state(started)
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
            gesture: None,
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
            resize_dialog_open: false,
            resize_w: Document::DEFAULT_WIDTH,
            resize_h: Document::DEFAULT_HEIGHT,
            png_dialog_open: false,
            png_cell_px: PNG_SCALE_PRESETS[1], // 16px/cell default
            current_path: None,
            last_error: None,
            saved_marker: None,
            close_dialog_open: false,
            force_close: false,
            shown_title: String::new(),
            started,
            first_frame: true,
        }
    }

    /// Whether any pointer gesture — primary stroke or right-click stroke — currently owns the
    /// canvas.
    pub(crate) fn stroke_in_progress(&self) -> bool {
        self.gesture.is_some()
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
    /// Flushes the slot first, unconditionally — `flush_slot` is self-gating, and the instance is
    /// about to be replaced regardless of whether the kind actually changed. Without this,
    /// re-selecting Text/Selection while already active would silently discard the pending,
    /// uncommitted burst or float.
    fn set_tool(&mut self, b: Binding, kind: ToolKind) {
        if self.stroke_in_progress() {
            return;
        }
        self.flush_slot(b);
        self.slots[b.ix()].kind = kind;
        self.slots[b.ix()].tool = make_tool(kind);
        // Only this slot's claim on the keyboard is released — rebinding L must not silently mute a
        // live session on R.
        self.release_keyboard(b);
    }

    /// Releases `b`'s claim on the keyboard, if it holds one. A no-op for the other slot's claim.
    fn release_keyboard(&mut self, b: Binding) {
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

    /// Finalizes slot `b`'s pending cross-frame session (Text's burst, Selection's float) into one
    /// undo entry. A no-op for every other kind.
    ///
    /// The kind gate isn't correctness — every stroke tool's catch-all swallows `Commit`
    /// harmlessly — it avoids building a `ToolCtx`, which clones the active ramp's `Vec<char>`.
    pub(crate) fn flush_slot(&mut self, b: Binding) {
        if !matches!(self.slots[b.ix()].kind, ToolKind::Text | ToolKind::Selection) {
            return;
        }
        let tctx = crate::canvas::tool_ctx(self, b);
        if let ToolResponse::Commit(Some(edit)) =
            self.slots[b.ix()].tool.update(ToolEvent::Commit, &tctx, &self.doc)
        {
            self.apply_edit(edit, Some(b));
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
        order_for(self.gesture_owner())
    }

    /// Which slot the pointer is currently driving, if any.
    pub(crate) fn gesture_owner(&self) -> Option<Binding> {
        self.gesture
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
        // A pasted float is a session, and only one exists at a time.
        self.flush_slot(b.other());
        self.keyboard_owner = Some(b);
        self.slots[b.ix()].tool.accept_stamp(patch, anchor, &self.doc);
    }

    /// Discards (not commits) whatever cross-frame session the active tool has pending, replacing
    /// it with a fresh instance. Called when the document itself is about to be replaced (Open):
    /// a pending burst's or float's `before` values are pinned against the doc that's about to be
    /// discarded, so committing into the *new* doc would graft stale edits onto unrelated content.
    fn reset_cross_frame_tool(&mut self) {
        // Both slots: either may hold a session pinned against the document being discarded.
        for b in Binding::ALL {
            if matches!(self.slots[b.ix()].kind, ToolKind::Text | ToolKind::Selection) {
                self.slots[b.ix()].tool = make_tool(self.slots[b.ix()].kind);
            }
        }
        // An in-flight gesture's pending cells are pinned against the discarded doc too — drop the
        // ownership outright so a release after the swap can't graft the old document's stroke onto
        // the new one.
        self.gesture = None;
        self.keyboard_owner = None;
    }

    /// Tool-select (`P`/`E`/`I`/`T`/`F`/`R`/`L`/`S`), undo/redo, and Ctrl+C copy keys. Undo/redo/
    /// Copy are `Ctrl`-modified chords and stay global (they won't collide with typing into the
    /// color picker's hex field); the single-letter tool keys are guarded on no widget having
    /// focus *and* not being mid-text-edit so typing into that hex field, or into the canvas in
    /// text mode, doesn't get swallowed as a tool switch.
    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        let focused = ui.memory(|m| m.focused().is_some()) || self.keyboard_owner.is_some();
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
            self.set_tool(Binding::L, ToolKind::Pencil);
        }
        if eraser {
            self.set_tool(Binding::L, ToolKind::Eraser);
        }
        if eyedropper {
            self.set_tool(Binding::L, ToolKind::Eyedropper);
        }
        if text {
            self.set_tool(Binding::L, ToolKind::Text);
        }
        if fill {
            self.set_tool(Binding::L, ToolKind::Fill);
        }
        if rect {
            self.set_tool(Binding::L, ToolKind::Rectangle);
        }
        if line {
            self.set_tool(Binding::L, ToolKind::Line);
        }
        if select {
            self.set_tool(Binding::L, ToolKind::Selection);
        }
        if brush {
            self.set_tool(Binding::L, ToolKind::Brush);
        }
        if copy {
            self.copy_selection(ui.ctx());
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
                    self.flush_all();
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
                let can_copy = self
                    .selection_slot()
                    .and_then(|b| self.slot(b).tool.selection_overlay())
                    .and_then(|v| v.marquee)
                    .is_some();
                let copy = egui::Button::new("Copy Selection").shortcut_text("Ctrl+C");
                if ui.add_enabled(can_copy, copy).clicked() {
                    self.copy_selection(ui.ctx());
                }
                if ui.button("Copy All as Text").clicked() {
                    // Flush first: a pending text burst or floating selection lives only in
                    // `self.slots[0].tool`'s overlay until committed into `self.doc` — copying without
                    // flushing would silently drop just-typed or just-moved content from the
                    // whole-document clipboard contents.
                    self.flush_all();
                    ui.ctx().copy_text(export_text(&self.doc));
                }
                ui.separator();
                if ui.button("Resize Canvas…").clicked() {
                    // Reads self.doc for the current extent, which a pending burst/float doesn't
                    // change (extent is fixed regardless), but flushing keeps the dialog's initial
                    // W/H consistent with whatever's about to be committed anyway.
                    self.flush_all();
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
                    self.flush_all();
                    match resize_document(&self.doc, self.resize_w, self.resize_h) {
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
        self.flush_all();
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
                    // Read from the fresh History rather than hardcoding None, so this stays
                    // correct if History::new()'s starting state ever changes.
                    self.saved_marker = self.history.top_edit_id();
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
            }
            Err(e) => self.last_error = Some(format!("failed to save {}: {e}", path.display())),
        }
    }

    /// Exports composited plain text to a file. Does not touch `current_path` — that's reserved
    /// for the native `.gascii` file.
    fn export_text_file(&mut self) {
        // Flush first — see `save_file`'s comment; export reads `self.doc` the same way save does.
        self.flush_all();
        let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).save_file() else {
            return;
        };
        if let Err(e) = std::fs::write(&path, export_text(&self.doc)) {
            self.last_error = Some(format!("failed to export {}: {e}", path.display()));
        } else {
            self.last_error = None;
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
            self.close_dialog_open = true;
        }
        // Else: clean — don't cancel, eframe closes the window at the end of this frame.
    }

    /// Re-requests a real close after the confirm dialog resolves (Save succeeded, or Don't Save).
    /// `force_close` lets the very next `close_requested` frame through without re-triggering the
    /// veto this dialog just cleared.
    fn close_now(&mut self, ctx: &egui::Context) {
        self.force_close = true;
        self.close_dialog_open = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// The Save/Don't Save/Cancel modal shown while `close_dialog_open`. `canvas.rs` and
    /// `handle_keys` are both gated off while this is open — see their own guards — since this
    /// dialog is the only place a decision here (discarding unsaved work) is irreversible.
    fn close_confirm_dialog(&mut self, ctx: &egui::Context) {
        if !self.close_dialog_open {
            return;
        }
        let resp = egui::Modal::new(egui::Id::new("close_confirm")).show(ctx, |ui| {
            ui.label("This document has unsaved changes.");
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    self.save_file();
                    // `save_file` leaves last_error/saved_marker untouched on cancel or failure —
                    // is_dirty() staying true after the call *is* the "didn't actually save"
                    // signal, no separate success/failure plumbing needed.
                    if !self.is_dirty() {
                        self.close_now(ctx);
                    }
                }
                if ui.button("Don't Save").clicked() {
                    self.close_now(ctx);
                }
                if ui.button("Cancel").clicked() {
                    self.close_dialog_open = false;
                }
            });
        });
        if resp.should_close() {
            self.close_dialog_open = false; // backdrop click / Escape == Cancel
        }
    }

    /// The window title: `GASCII — <file>`, with a bullet while there are unsaved changes. This is
    /// where `current_path` lives now — the spec's status bar has no file slot.
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

impl eframe::App for GasciiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.first_frame {
            eprintln!("startup to first frame: {:?}", self.started.elapsed());
            self.first_frame = false;
        }
        let ctx = ui.ctx().clone();
        self.handle_close_request(&ctx);
        if !self.close_dialog_open {
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
        // window and must win over any widget sitting under it.
        crate::ui::titlebar::handle_resize(&ctx);

        // Panel stack per spec §4.
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
            .exact_size(26.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| self.menu_bar(ui));
            });
        egui::Panel::top("options")
            .frame(egui::Frame::new().fill(t.bg_chrome).inner_margin(egui::Margin::symmetric(12, 0)))
            .exact_size(crate::ui::options_bar::HEIGHT)
            .show(ui, |ui| crate::ui::options_bar::show(ui, self));
        // The status bar is claimed BEFORE the sidebar, so it spans the full window width as the
        // mockup has it. Panels take their slice in declaration order: sidebar-first would give the
        // left panel the whole remaining height and leave the status bar starting at x=208.
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(t.bg_panel).inner_margin(egui::Margin::symmetric(12, 0)))
            .exact_size(crate::ui::status_bar::HEIGHT)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| crate::ui::status_bar::show(ui, self));
            });
        egui::Panel::left("sidebar")
            .frame(egui::Frame::new().fill(t.bg_panel).inner_margin(egui::Margin::same(12)))
            .exact_size(crate::ui::sidebar::WIDTH)
            .resizable(false)
            .show(ui, |ui| crate::ui::sidebar::show(ui, self));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(t.bg_desk))
            .show(ui, |ui| {
                canvas::show(ui, self);
            });

        self.resize_dialog(&ctx);
        self.png_export_dialog(&ctx);
        self.close_confirm_dialog(&ctx);

        // Last, on the foreground layer: with the OS frame gone, nothing else draws the window's
        // own outline.
        crate::ui::titlebar::paint_window_edge(&ctx);
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

    /// Both bindings start bound, and to different tools — the spec's "exactly one tool is bound to
    /// L and one to R at all times" has no unbound state. Pins the migration of the old
    /// `right_click_tool: 0 // Eraser` default.
    #[test]
    fn default_bindings_are_pencil_on_l_and_eraser_on_r() {
        let slots = [ToolSlot::new(ToolKind::Pencil), ToolSlot::new(ToolKind::Eraser)];
        assert_eq!(slots[Binding::L.ix()].kind, ToolKind::Pencil);
        assert_eq!(slots[Binding::R.ix()].kind, ToolKind::Eraser);
    }

    /// The spec's named behavior: each binding keeps its own footprint memory, so sizing the
    /// right button's Eraser must not resize the left button's. Structural here — the two slots own
    /// separate arrays — but this pins it against a refactor that reintroduces a shared one.
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
    /// paints underneath the other slot's session, which commits later. Pure over the gesture
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
    /// Impossible to hit before this refactor, because only one slot could ever hold a session.
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

    /// Every kind is bindable to either button — the point of the refactor. Text, Selection and
    /// Eyedropper were previously left-only.
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
}
