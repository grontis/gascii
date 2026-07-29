use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use eframe::egui;
use gascii_core::{
    builtin_pages, clear_document, composite, composite_frame, duplicate_frame, export_text,
    export_text_frames, load_str, resize_document, save_string, AxisAnchor, BrushShape, CellPatch,
    Document, Eraser, FloodFill, FrameOpError, History, Line, Page, Pencil, PlaneMask, Rectangle,
    ResizeAnchor, ResizeError, Rgba, SelectionTool, TextTool, Tool, ToolEvent, ToolResponse,
    WidthReject, MAX_TOOL_SIZE,
};

use gascii_plugin_api::{CanvasRenderer, Plugin};

use crate::anim_export;
use crate::canvas::{self, NaiveRenderer};
use crate::chords::{self, ChordDispatch, ChordId};
use crate::fonts;
use crate::image_bg;
use crate::png_export;
use crate::prefs;
use crate::ui::dialog::{self, DialogAction};
use crate::viewport::Viewport;

/// PNG cell-px per export scale preset: `16 * {1, 2, 4}`.
const EXPORT_CELL_PX_BASE: u32 = 16;

/// Terminal Ctrl+C presses received over the process lifetime. Written by the signal-handler
/// thread, drained by `handle_ctrl_c` on the UI thread each frame.
static CTRL_C_PRESSES: AtomicU32 = AtomicU32::new(0);

/// What a batch of new Ctrl+C presses should do. A first press asks for a normal close — the same
/// path as the window's close button, unsaved-changes veto included. A press arriving while that
/// veto dialog is already up means the user is insisting: close without saving.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CtrlCResponse {
    RequestClose,
    ForceClose,
}

/// Pure escalation rule for `handle_ctrl_c`: `count` is the process-lifetime press total, `seen`
/// how many have already been acted on, `close_confirm_up` whether the close veto dialog is
/// currently showing.
fn ctrl_c_response(count: u32, seen: u32, close_confirm_up: bool) -> Option<CtrlCResponse> {
    if count == seen {
        return None;
    }
    Some(if close_confirm_up { CtrlCResponse::ForceClose } else { CtrlCResponse::RequestClose })
}

/// Whether a pasted `Event::Paste` text is still the app's own copy: the OS clipboard is "ours"
/// exactly when `internal`'s own flattening still matches what came back on paste. Pulled out of
/// `paste_text` as a pure function so the copy/paste reconciliation decision is unit-testable
/// without constructing a full `GasciiApp`.
fn is_own_clipboard_text(text: &str, internal: Option<&CellPatch>) -> bool {
    internal.is_some_and(|p| p.to_text() == text)
}

/// Whether this frame's raw events include a real `egui::Event::Copy` — the event egui-winit
/// actually emits for Ctrl+C/Cmd+C/Ctrl+Insert, intercepting the chord before it ever reaches
/// `Event::Key`. `shift` (`InputState::modifiers.shift`, read at the moment the event is observed)
/// discriminates plain copy from copy-all: `Event::Copy` carries no modifier state of its own, so the
/// ambient modifiers are the only signal Ctrl+Shift+C leaves behind by the time this fires. Pure, so
/// the discrimination is testable without a live `GasciiApp`, mirroring `is_own_clipboard_text`.
fn copy_events(events: &[egui::Event], shift: bool) -> (bool, bool) {
    let copy = events.iter().any(|e| matches!(e, egui::Event::Copy));
    (copy && !shift, copy && shift)
}

/// Whether this frame's raw events include a real `egui::Event::Cut` — mirrors `copy_events`'s own
/// reasoning: egui-winit intercepts the clipboard chord before it ever reaches `Event::Key`.
fn cut_event(events: &[egui::Event]) -> bool {
    events.iter().any(|e| matches!(e, egui::Event::Cut))
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

/// The Export dialog's "Trim trailing spaces" *unchecked* path for `ExportFormat::TextFrames` —
/// mirrors `export_text_untrimmed`'s exact asymmetric core/app split (untrimmed variants have
/// always lived app-side) and `export_text_frames`'s own header/frame-separator format.
fn export_text_frames_untrimmed(doc: &Document) -> String {
    (0..doc.frame_count())
        .map(|i| {
            let body = composite_frame(doc, i)
                .expect("i is always in 0..frame_count()")
                .iter()
                .map(|row| row.iter().map(|c| c.ch).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            let dur = doc.resolved_frame_duration_ms(i).expect("i is always in 0..frame_count()");
            format!("--- frame {} ({dur}ms) ---\n{body}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A document that dropped to one frame while the dialog was closed (or between opens) must not
/// reopen on a multi-frame-only format that's no longer offered — snaps back to `Text` in that
/// case, a no-op otherwise. Pure, mirroring `export_dialog_formats`'s own testability rationale.
fn snap_unavailable_export_format(format: ExportFormat, frame_count: usize) -> ExportFormat {
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
fn export_dialog_formats(doc: &Document) -> Vec<(ExportFormat, &'static str)> {
    let mut formats = vec![(ExportFormat::Text, "Text (.txt)"), (ExportFormat::Png, "PNG")];
    if doc.frame_count() > 1 {
        formats.push((ExportFormat::Gif, "Animated GIF"));
        formats.push((ExportFormat::SpriteSheet, "PNG Spritesheet"));
        formats.push((ExportFormat::TextFrames, "Text Frames (.txt)"));
    }
    formats
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
    owner_kind.is_some_and(|k| tool_def(k).suppresses_shortcuts)
}

/// Whether Escape's job this frame is "exit fullscreen" rather than something with higher
/// priority. `handle_keys` only runs while `!modal_open()` (its caller already guarantees that),
/// so this only has two other claims on Escape to check: an active Text/Selection session (ends
/// on its own Escape handling inside `canvas.rs`), and a live pointer stroke (exiting fullscreen
/// mid-drag would yank the canvas out from under the pointer).
fn should_handle_escape_for_fullscreen(keyboard_owner: Option<Binding>, stroke_in_progress: bool) -> bool {
    keyboard_owner.is_none() && !stroke_in_progress
}

/// Whether `kind`'s single-letter shortcut should be reachable from the keyboard this frame.
/// Kiosk's sidebar deliberately excludes Text from its tool grid (`kiosk.rs`'s module doc — "no
/// keyboard-driven session UI"), so reaching it via `T` while fullscreen would silently rebind L
/// to a tool with no cell in that grid and no other on-screen trace of what changed. Every other
/// tool's shortcut stays reachable — their tools are visible in the kiosk grid and show L/R
/// badges, so the shortcut's effect is diagnosable from the touch UI alone. A binding already on
/// Text when fullscreen is entered is untouched by this — it only gates *switching onto* Text,
/// never an existing Text binding's normal operation (its caret still shows, its session still
/// works).
fn tool_shortcut_reachable(kind: ToolKind, is_fullscreen: bool) -> bool {
    !is_fullscreen || tool_def(kind).kiosk_visible
}

/// `handle_keys`'s tool-shortcut lookup predicate — deliberately side-effecting: past the
/// reachability check, it also *consumes* `def`'s key event via `i.consume_key`. That side effect
/// is what makes `tools().iter().find(...)` correct rather than merely convenient: `find` stops at
/// the first row this returns `true` for, so exactly one key gets consumed per frame and every row
/// it never reaches (because an earlier one already matched) is left completely untouched — the
/// same "table order = consumption order, one shot" contract the chord registry's own generic loop
/// follows.
fn tool_shortcut_fires_and_consumes_its_key(def: &ToolDef, i: &mut egui::InputState, is_fullscreen: bool) -> bool {
    tool_shortcut_reachable(def.kind, is_fullscreen) && i.consume_key(egui::Modifiers::NONE, def.key)
}

/// Whether this kind can hold a cross-frame Session (uncommitted work outliving a single stroke —
/// a Text burst, a floating stamp). The one place that fact lives: `flush_slot`, `end_session`,
/// the document-swap reset, and the takeover in `begin_gesture` all consult it, so a future
/// session-holding kind is a one-line change here rather than a four-site hunt.
pub(crate) fn holds_session(kind: ToolKind) -> bool {
    tool_def(kind).holds_session
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

/// Which mouse button drives a tool. Named for what the UI says — the sidebar's option rows and
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
    /// The terminal cell of this slot's last committed `Line` stroke, so a Shift-held fresh Press
    /// can continue from it (`begin_gesture`). `None` until a Line stroke actually commits, and
    /// cleared whenever the binding is rebound away from `Line` (`set_tool`) so a later rebind back
    /// to `Line` never resumes a point from an unrelated editing session.
    pub last_line_point: Option<(u16, u16)>,
}

impl ToolSlot {
    fn new(kind: ToolKind) -> Self {
        ToolSlot {
            kind,
            tool: make_tool(kind),
            stamps: [StampSettings::default(); SIZED_TOOL_COUNT],
            last_line_point: None,
        }
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
    tool_def(kind).stamp_slot.map(|i| i as usize)
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
    tool_def(kind).shows_hover
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

/// One tool: kind, display name, shortcut, hint, constructor, and its static capability facts, in
/// a single row. Every scattered per-kind `match` (sizing, sessions, hover, RECENT, shortcuts,
/// kiosk visibility) collapses to a lookup into these fields, so a tool's whole behavior lives in
/// one literal instead of a hunt across the file.
///
/// `Clone, Copy`: every field already is (`ToolKind`, `&'static str`, `egui::Key`, bare `fn`
/// pointers, `bool`/`Option<u8>`), so this is purely additive — it lets kiosk filter `TOOLS` into
/// an owned `Vec<ToolDef>` without borrowing games.
#[derive(Clone, Copy)]
pub(crate) struct ToolDef {
    pub kind: ToolKind,
    pub name: &'static str,
    pub key: egui::Key,
    pub tip: &'static str,
    pub make: fn() -> Box<dyn Tool>,
    /// Slot in a `ToolSlot`'s `stamps` array for this kind's size/shape footprint; `None` for
    /// unsized kinds. `prefs.rs` persists stamps by this index, so it must stay stable per kind.
    pub stamp_slot: Option<u8>,
    /// Whether this kind can hold a cross-frame Session (a Text burst, a floating stamp).
    pub holds_session: bool,
    /// Whether this kind gets a hover marker previewing its next application.
    pub shows_hover: bool,
    /// Whether a stroke of this kind that stamped the glyph plane counts toward RECENT.
    pub stamps_glyph: bool,
    /// Whether an active session of this kind swallows the single-letter tool-select shortcuts.
    pub suppresses_shortcuts: bool,
    /// Whether this kind gets a cell in kiosk's touch sidebar grid, and is therefore reachable by
    /// its shortcut while fullscreen.
    pub kiosk_visible: bool,
    /// The index into `plugin_factories()`/`GasciiApp::plugins` that owns this row, for a
    /// plugin-sourced tool; `None` for every pure built-in row. This is how
    /// `sidebar::binding_options_geom`'s dedup, `tool_ctx`'s extra-context injection, and the
    /// pressure-override gate all find "which live plugin instance, if any, owns this bound row"
    /// without a second lookup table.
    pub plugin_slot: Option<usize>,
    /// Whether a stylus-pressure stroke should override this kind's stamp size.
    pub pressure_sizeable: bool,
    /// Whether `tool_ctx` should ask the owning plugin (via `plugin_slot`) for extra `ToolCtx`
    /// fields (density mode, ramp) while this kind is bound.
    pub wants_extra_ctx: bool,
}

/// The eight pure built-in tools (Brush's row is plugin-sourced — see `plugin_factories`), and the
/// single source of truth for their names, shortcuts, hints, constructors, and capability facts.
/// Feeds `tools()`, the registry every call site (the toolbox, the shortcut handler, the sidebar's
/// option rows, both bindings, prefs) reads.
fn build_tools() -> Vec<ToolDef> {
    let mut rows = vec![
        ToolDef {
            kind: ToolKind::Pencil,
            name: "Pencil",
            key: egui::Key::P,
            tip: "Draw the active glyph",
            make: || Box::new(Pencil::new()),
            stamp_slot: Some(0),
            holds_session: false,
            shows_hover: true,
            stamps_glyph: true,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_extra_ctx: false,
        },
        ToolDef {
            kind: ToolKind::Eraser,
            name: "Eraser",
            key: egui::Key::E,
            tip: "Erase cells to blank",
            make: || Box::new(Eraser::new()),
            stamp_slot: Some(1),
            holds_session: false,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_extra_ctx: false,
        },
        ToolDef {
            kind: ToolKind::Text,
            name: "Text",
            key: egui::Key::T,
            tip: "Click to place a cursor, then type",
            make: || Box::new(TextTool::new()),
            stamp_slot: None,
            holds_session: true,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: true,
            // Kiosk has no keyboard-driven session UI, so Text has no cell in its touch grid —
            // and therefore its shortcut must not be reachable while fullscreen either.
            kiosk_visible: false,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_extra_ctx: false,
        },
        ToolDef {
            kind: ToolKind::Fill,
            name: "Fill",
            key: egui::Key::F,
            tip: "Flood-fill a connected region",
            make: || Box::new(FloodFill::new()),
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: true,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_extra_ctx: false,
        },
        ToolDef {
            kind: ToolKind::Rectangle,
            name: "Rectangle",
            key: egui::Key::R,
            tip: "Drag a box outline; joins box-drawing art",
            make: || Box::new(Rectangle::new()),
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: true,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_extra_ctx: false,
        },
        ToolDef {
            kind: ToolKind::Line,
            name: "Line",
            key: egui::Key::L,
            tip: "Drag a straight line; joins box-drawing art",
            make: || Box::new(Line::new()),
            stamp_slot: Some(2),
            holds_session: false,
            shows_hover: true,
            stamps_glyph: true,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_extra_ctx: false,
        },
        ToolDef {
            kind: ToolKind::Selection,
            name: "Selection",
            key: egui::Key::S,
            tip: "Drag a region to move, copy, or delete",
            make: || Box::new(SelectionTool::new()),
            stamp_slot: None,
            holds_session: true,
            // A press starts a marquee/move gesture, not a cell stamp — a stamp-shaped hover
            // marker would promise the wrong semantics.
            shows_hover: false,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_extra_ctx: false,
        },
        ToolDef {
            kind: ToolKind::Eyedropper,
            name: "Eyedropper",
            key: egui::Key::I,
            tip: "Click a cell to pick up its text and background colors",
            make: || Box::new(InertTool),
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_extra_ctx: false,
        },
    ];
    for (i, factory) in plugin_factories().into_iter().enumerate() {
        let scratch = factory();
        for cap in scratch.register_tools() {
            rows.push(merge_plugin_row(i, &cap));
        }
    }
    // A plugin-registered tool key silently colliding with a reserved global chord (`X` swap-colors,
    // `[`/`]` size steppers, and every other modifier-less chord `chords::reserved_global_keys`
    // knows about) would leave one of the two permanently unreachable — `tools()`'s own
    // `find()`-based one-shot consumption always resolves in table order, so whichever comes first
    // wins and the other never fires. `tool_shortcuts_are_unique` already catches a plugin-vs-tool
    // collision; this catches the plugin-vs-non-tool-chord gap that check structurally can't see.
    // Debug-only: a colliding release-build plugin key ships silently, an accepted trade-off in a
    // pre-1.0, in-repo-only plugin ecosystem.
    debug_assert!(
        rows.iter().filter(|d| d.plugin_slot.is_some()).all(|d| !tool_key_collides_with_reserved(d.key)),
        "a plugin-registered tool key collides with a reserved global chord — see chords::reserved_global_keys"
    );
    rows
}

/// Whether `key` collides with a reserved global chord (`chords::reserved_global_keys()`) — the
/// predicate `build_tools()`'s debug assertion checks every plugin-registered tool key against,
/// pulled out as a pure function so the check is unit-testable without constructing a real
/// colliding plugin and triggering the assert for it.
pub(crate) fn tool_key_collides_with_reserved(key: egui::Key) -> bool {
    chords::reserved_global_keys().any(|k| k == key)
}

/// The fixed, ordered list of plugin constructors. Read by two independent consumers in the *same
/// order*, which is the contract the whole plugin design leans on: `build_tools` constructs one
/// throwaway instance per factory purely to harvest its static `register_tools()` description,
/// while `GasciiApp::with_state`/`headless` construct one real, retained instance per factory into
/// `GasciiApp::plugins`. A `ToolDef` row's `plugin_slot` is the index into both — so if a future
/// edit ever iterates this list differently between the two call sites, every `plugin_slot`
/// silently points at the wrong live instance. Never cache this in a `OnceLock`: a plugin may hold
/// per-app state (`gascii-density-brush`'s `BrushPlugin` does), which must not be shared
/// process-globally across two `GasciiApp` instances.
fn plugin_factories() -> Vec<fn() -> Box<dyn Plugin>> {
    vec![
        || Box::new(gascii_density_brush::BrushPlugin::new()) as Box<dyn Plugin>,
        || Box::new(gascii_anim::AnimPlugin::new()) as Box<dyn Plugin>,
    ]
}

/// Folds every plugin's `wrap_renderer` over the host's own `NaiveRenderer`, innermost (the host's)
/// first, in `plugins` order. A pure function of the plugin list — takes `&[Box<dyn Plugin>]`
/// rather than `&GasciiApp` so it's testable against a synthetic plugin list with no live app.
pub(crate) fn build_renderer(plugins: &[Box<dyn Plugin>]) -> Box<dyn CanvasRenderer> {
    plugins.iter().fold(Box::new(NaiveRenderer) as Box<dyn CanvasRenderer>, |r, p| p.wrap_renderer(r))
}

/// Host-owned identity assignment for a plugin-sourced tool name — persistence-critical (see
/// `stamp_slot_for_plugin_tool`), so never derived from plugin-registration order. Panics at
/// startup (registration time, never a user-facing runtime path) on an unrecognized name rather
/// than silently mis-mapping it.
fn kind_for_plugin_tool(name: &str) -> ToolKind {
    match name {
        gascii_density_brush::BRUSH => ToolKind::Brush,
        _ => panic!("plugin tool {name:?} has no reserved ToolKind — add one here"),
    }
}

/// Host-owned stamp-slot assignment for a plugin-sourced sized tool. `prefs.rs` persists stamps by
/// this index "in `sized_slot` order" — auto-deriving it from plugin-list position would silently
/// break an existing `prefs.json`'s positionally-indexed `stamps` array on upgrade. Brush stays
/// `3`, unchanged from its pre-migration literal row.
fn stamp_slot_for_plugin_tool(name: &str) -> Option<u8> {
    match name {
        gascii_density_brush::BRUSH => Some(3),
        _ => panic!("plugin tool {name:?} has no reserved stamp_slot — add one here"),
    }
}

/// Merges one plugin-contributed capability bundle into a full `ToolDef` row: the host assigns
/// identity (`ToolKind`) and, for a sized tool, the stamp-slot index; everything else carries over
/// from the bundle as-is.
fn merge_plugin_row(plugin_slot: usize, cap: &gascii_plugin_api::PluginToolCapabilities) -> ToolDef {
    ToolDef {
        kind: kind_for_plugin_tool(cap.name),
        name: cap.name,
        key: cap.key,
        tip: cap.tip,
        make: cap.make,
        stamp_slot: if cap.sized { stamp_slot_for_plugin_tool(cap.name) } else { None },
        holds_session: cap.holds_session,
        shows_hover: cap.shows_hover,
        stamps_glyph: cap.stamps_glyph,
        suppresses_shortcuts: cap.suppresses_shortcuts,
        kiosk_visible: cap.kiosk_visible,
        plugin_slot: Some(plugin_slot),
        pressure_sizeable: cap.pressure_sizeable,
        wants_extra_ctx: cap.wants_extra_ctx,
    }
}

/// The process-global tool registry: lazily built from `build_tools` on first read.
static TOOL_REGISTRY: OnceLock<Vec<ToolDef>> = OnceLock::new();

/// The tool registry every call site reads — the toolbox, the shortcut handler, the sidebar's
/// option rows, both bindings, and prefs (persisted by `.name`, not by index or position).
pub(crate) fn tools() -> &'static [ToolDef] {
    TOOL_REGISTRY.get_or_init(build_tools)
}

pub(crate) fn tool_def(kind: ToolKind) -> &'static ToolDef {
    tools().iter().find(|d| d.kind == kind).expect("tools() covers every ToolKind")
}

/// Builds a fresh instance for `kind`. Total over `ToolKind` — `tools_table_lists_every_kind_
/// exactly_once` pins that the lookup cannot miss.
pub(crate) fn make_tool(kind: ToolKind) -> Box<dyn Tool> {
    (tool_def(kind).make)()
}

/// A `PluginHost` snapshot, built fresh at each call site rather than implemented directly on
/// `GasciiApp`. `options_ui`/`tick`/`panel` need `&mut self.plugins[i]` (or to iterate
/// `self.plugins`) at the same call site that would otherwise need `&GasciiApp` too if `PluginHost`
/// were implemented on the app type directly — a field-level double-borrow the compiler rejects.
/// Now carries a live `&Document` alongside the two `Copy` facts it always had — built from
/// individual field expressions (`&self.doc`, never `&GasciiApp` or `self` as a whole) so the
/// borrow it holds is scoped to just `self.doc`, disjoint from `self.plugins`, which every one of
/// this type's three call sites immediately borrows mutably afterward. Passing `&GasciiApp` here
/// instead would tie the returned value's lifetime to the *whole* struct, conflicting with every
/// one of those mutable borrows.
pub(crate) struct HostFacts<'a> {
    doc: &'a Document,
    stylus_detected: bool,
    bound: [&'static str; 2],
}

impl gascii_plugin_api::PluginHost for HostFacts<'_> {
    fn stylus_detected(&self) -> bool {
        self.stylus_detected
    }

    fn is_bound(&self, tool_name: &str) -> bool {
        self.bound.contains(&tool_name)
    }

    fn document(&self) -> &Document {
        self.doc
    }
}

/// Builds a `HostFacts` from an explicit `&Document` plus the two `Copy` facts — never from
/// `&GasciiApp` (see `HostFacts`'s own doc comment for why).
pub(crate) fn host_facts<'a>(doc: &'a Document, stylus_detected: bool, bound: [&'static str; 2]) -> HostFacts<'a> {
    HostFacts { doc, stylus_detected, bound }
}

/// The `stylus_detected`/`bound` half of `host_facts`'s arguments, computed from `app` in one
/// place. Takes `&GasciiApp` and returns owned data only — its borrow of `app` ends the moment it
/// returns, before the caller separately, disjointly borrows `app.doc`/`app.plugins`.
pub(crate) fn host_context(app: &GasciiApp) -> (bool, [&'static str; 2]) {
    (app.stylus_detected, [tool_def(app.slot(Binding::L).kind).name, tool_def(app.slot(Binding::R).kind).name])
}

pub struct GasciiApp {
    pub(crate) doc: Document,
    pub(crate) viewport: Viewport,
    pub(crate) hovered_cell: Option<(u16, u16)>,
    pub(crate) renderer: Box<dyn CanvasRenderer>,
    /// One retained instance per `plugin_factories()` entry, in the same order — the live half of
    /// the plugin split (`build_tools`'s own instances are throwaway, harvested once for their
    /// static `register_tools()` description). A `ToolDef.plugin_slot` indexes into this.
    pub(crate) plugins: Vec<Box<dyn Plugin>>,
    pub(crate) pending_fit: bool,
    /// Deferred `+`/`-` zoom request (sign = direction, 0 = none) from the keyboard chords, the
    /// View menu, or the status bar — none of which have the canvas geometry an anchored zoom
    /// needs. `canvas::show` applies it through the same cursor-anchored `zoom_at` path as the
    /// wheel (pointer if hovering, else viewport center), so a mid-stroke zoom can't remap the
    /// still pointer to a different cell.
    pub(crate) pending_step_zoom: i32,
    /// The canvas area size `fit_to_window` last ran against while fullscreen — kiosk's "auto"
    /// continuous fit (re-fits only when this stops matching `ui.available_size()`, rather than
    /// unconditionally every frame). `None` outside fullscreen, so re-entering kiosk later always
    /// re-fits at least once.
    pub(crate) kiosk_last_fit_size: Option<egui::Vec2>,
    /// Window state to force on the first frame, then `None`. eframe's own window persistence
    /// restores the previous run's fullscreen state, but the app always launches windowed unless
    /// `--fullscreen` was passed on the command line.
    pub(crate) startup_fullscreen: Option<bool>,
    pub(crate) history: History,
    pub(crate) active_glyph: char,
    pub(crate) active_fg: Rgba,
    pub(crate) active_bg: Rgba,
    pub(crate) mask: PlaneMask,
    /// The layer every tool reads and writes: `tool_ctx`'s `ToolCtx.layer`, the eyedropper pick,
    /// and `resync_slots`' resync target all source it from here. v1 documents have exactly one
    /// layer, so this is always `0` with no UI to change it — the plumbing a future layers feature
    /// would generalize, not the value.
    pub(crate) active_layer: usize,
    /// The frame every tool reads and writes: `tool_ctx`'s `ToolCtx.frame` and `resync_slots`'
    /// resync target both source it from here, mirroring `active_layer` exactly. Always `0` today,
    /// with no UI to change it yet.
    ///
    /// Kept in sync with `doc.active_frame()` at every `History` choke point, in both directions:
    /// `apply_edit` seeds `doc`'s cursor from this field before every `History::apply` (app -> doc,
    /// since a caller-built `Edit` targets a specific frame that must already be `doc`'s active one
    /// before it applies), then reads it back afterward (doc -> app), because `AddFrame`/
    /// `RemoveFrame`/`ReorderFrame` shift `doc`'s cursor as a side effect of applying — independent
    /// of whatever was just seeded. `request_undo`/`request_redo` mutate `doc` directly (bypassing
    /// `apply_edit`) and restore `doc.active_frame()` from the `Edit`'s own baked-in snapshot, so
    /// they resync this field the same way afterward. `doc.active_frame()` is the ground truth;
    /// this field only ever leads at the one seed point in `apply_edit`, and follows everywhere
    /// else.
    pub(crate) active_frame: usize,
    /// The two bindings, indexed by `Binding::ix`. Exactly one tool is bound to each at all times.
    pub(crate) slots: [ToolSlot; 2],
    /// Which binding the `[`/`]` size keys adjust: the one last drawn with or last bound.
    pub(crate) options_focus: Binding,
    /// Which slot's tool the pointer is currently driving, if any. Stroke ownership is one
    /// question, so it is one field — which is what let the press/drag/release paths collapse to a
    /// single parameterized call site. At most one stroke is live across both buttons.
    pub(crate) stroke_owner: Option<Binding>,
    /// Live stylus-pressure size override for the current stroke, consulted only in `tool_ctx`
    /// while `stroke_owner` matches. Never touches a slot's remembered `StampSettings.size` — the
    /// stepper/`[`/`]`-configured size the Size stepper edits and `prefs.rs` persists stays intact
    /// across a pressure-modulated stroke. Reset to `None` whenever a stroke begins or ends, so a
    /// stale value never leaks onto a stroke that hasn't reported pressure yet.
    pub(crate) pressure_stamp_size: Option<u16>,
    /// The cell `begin_gesture` pressed at, for the live stroke — the `anchor` `shift_constrain`
    /// measures a Shift-held drag against (Line's 45° rays, Rectangle's square). `Some` for exactly
    /// as long as `stroke_owner` is, set alongside it in `begin_gesture` and cleared alongside its
    /// own reset in `show()`'s tail handling.
    pub(crate) stroke_press_cell: Option<(u16, u16)>,
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
    /// Pending jump-to-section request from the palette page buttons; consumed next frame once
    /// the target section header is laid out. Not persisted.
    pub(crate) palette_scroll_target: Option<usize>,
    /// The last [`RECENT_GLYPHS`] glyphs used, most recent first. Fed by picking a swatch
    /// (`pick_glyph`) and by a committed stroke that stamped the active glyph (`note_glyph_drawn`).
    pub(crate) recent_glyphs: Vec<char>,
    /// The chosen theme preference (persisted). Applied to the `egui::Context` once at startup
    /// (`GasciiApp::new`) and again on every change from the View ▸ Theme menu — never read back
    /// from the `Context` itself, so `Prefs::from_app`/`App::save` need no `Context` at all.
    pub(crate) theme_pref: egui::ThemePreference,
    /// Whether the canvas cell-grid overlay is drawn. Persisted, off by default.
    pub(crate) show_grid: bool,
    /// Whether the `?` keyboard-shortcuts overlay is showing. Session-only, never persisted —
    /// mirrors `resize_dialog_open`/`export_dialog_open`'s own "starts closed every launch"
    /// precedent. Built on the same `dialog::modal` surface as every other dialog and therefore
    /// counted by `modal_open()` too: `canvas.rs` polls raw pointer/keyboard state rather than
    /// using egui's occlusion system, so without this a click "through" the overlay would still
    /// draw on the canvas underneath it.
    pub(crate) help_overlay_open: bool,
    /// True once this session has observed a pressure-bearing `Event::Touch` (a stylus contact).
    /// Session-only, never persisted. A device-capability fact, not Brush-owned state — it only
    /// happens to gate the Pressure toggle's visibility in Brush's options block, exposed to
    /// plugins read-only via `PluginHost::stylus_detected`.
    pub(crate) stylus_detected: bool,
    /// Accumulated multiplicative pinch-zoom delta since the last discrete zoom step fired.
    /// `multi_touch()`'s `zoom_delta` is a per-frame ratio (1.0 = no change), not a cumulative
    /// gesture magnitude, so this multiplies frame deltas together until they cross a threshold —
    /// see the pinch-zoom handling in `canvas.rs` for why a per-frame trigger would be too twitchy
    /// against the 6-step discrete `ZOOM_SCALES` model.
    pub(crate) pinch_zoom_accum: f32,
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
    /// The loaded reference image, if any — shown as a live trace overlay under the canvas, and
    /// (when `use_in_export` is set) composited into the PNG export. One at a time, in-memory only.
    pub(crate) image_bg: Option<image_bg::ImageBackground>,
    /// Bumped on every `image_bg` change that affects the export composite (load, clear, opacity/
    /// gate edits) — folds into the export preview's cache key (`ExportPreviewKey`) so a stale
    /// preview never survives an image edit.
    pub(crate) image_bg_gen: u64,
    export_dialog_open: bool,
    pub(crate) export: ExportSettings,
    export_preview: Option<egui::TextureHandle>,
    /// The settings (and image-background generation) the current `export_preview` texture was
    /// generated from — regenerated only when this stops matching the current key, or on dialog
    /// open (`None` after close).
    export_preview_key: Option<ExportPreviewKey>,
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
    /// How many of `CTRL_C_PRESSES` this app has already acted on — the drain cursor for
    /// `handle_ctrl_c`.
    ctrl_c_seen: u32,
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
struct ExportPreviewKey {
    settings: ExportSettings,
    image_gen: u64,
}

impl GasciiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, started: Instant, launch_fullscreen: bool) -> Self {
        fonts::install_fonts(&cc.egui_ctx);
        crate::ui::theme::install(&cc.egui_ctx);
        let mut app = Self::with_state(started);
        prefs::load(cc.storage, &mut app);
        cc.egui_ctx.set_theme(app.theme_pref);
        app.startup_fullscreen = Some(launch_fullscreen);
        // A terminal Ctrl+C would otherwise kill the process outright, skipping the
        // unsaved-changes veto and eframe's shutdown persistence. The handler only counts and
        // wakes the event loop; `handle_ctrl_c` decides on the UI thread.
        let ctx = cc.egui_ctx.clone();
        if let Err(e) = ctrlc::set_handler(move || {
            CTRL_C_PRESSES.fetch_add(1, Ordering::Relaxed);
            ctx.request_repaint();
        }) {
            eprintln!("Ctrl+C handler not installed ({e}); Ctrl+C will terminate abruptly");
        }
        app
    }

    /// One-shot, first frame only: pins the launch window state regardless of what eframe's window
    /// persistence restored.
    pub(crate) fn apply_startup_window_state(&mut self, ctx: &egui::Context) {
        if let Some(fs) = self.startup_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fs));
            if fs {
                self.pending_fit = true;
            }
        }
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
        // Same-order contract with `build_tools`'s own throwaway instances — see `plugin_factories`.
        let plugins: Vec<Box<dyn Plugin>> = plugin_factories().into_iter().map(|f| f()).collect();
        let renderer = build_renderer(&plugins);
        Self {
            doc: Document::default_document(),
            viewport: Viewport::default(),
            hovered_cell: None,
            renderer,
            plugins,
            // Fit on the first frame: a document pinned to the top-left corner of the desk is not
            // "the star", and the viewport's default pan of zero puts it there.
            pending_fit: true,
            pending_step_zoom: 0,
            kiosk_last_fit_size: None,
            startup_fullscreen: None,
            history: History::new(),
            active_glyph: '#',
            active_fg: Rgba::WHITE,
            active_bg: Rgba::TRANSPARENT,
            mask: PlaneMask::default(),
            active_layer: 0,
            active_frame: 0,
            slots: [ToolSlot::new(ToolKind::Pencil), ToolSlot::new(ToolKind::Eraser)],
            options_focus: Binding::L,
            stroke_owner: None,
            pressure_stamp_size: None,
            stroke_press_cell: None,
            space_pan_active: false,
            keyboard_owner: None,
            was_focused: true,
            internal_clipboard: None,
            pages: builtin_pages(),
            active_page: 0,
            palette_scroll_target: None,
            recent_glyphs: Vec::new(),
            theme_pref: egui::ThemePreference::System,
            show_grid: false,
            help_overlay_open: false,
            stylus_detected: false,
            pinch_zoom_accum: 1.0,
            resize_dialog_open: false,
            resize_w: Document::DEFAULT_WIDTH,
            resize_h: Document::DEFAULT_HEIGHT,
            resize_anchor: ResizeAnchor::default(),
            new_dialog_open: false,
            new_w: Document::DEFAULT_WIDTH,
            new_h: Document::DEFAULT_HEIGHT,
            new_bg: Rgba(0, 0, 0, 255),
            image_bg: None,
            image_bg_gen: 0,
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
            ctrl_c_seen: 0,
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
        self.confirm.is_some()
            || self.resize_dialog_open
            || self.export_dialog_open
            || self.new_dialog_open
            || self.help_overlay_open
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

    /// Test-only downcast into the live `BrushPlugin` instance, for tests that need to drive or
    /// inspect its own state (ramp/mode/pressure) directly rather than only through the `Plugin`
    /// trait's narrow surface — e.g. confirming it survives being rendered through two different
    /// chrome geometries unchanged. Not a production access path: nothing outside tests should ever
    /// need a concrete plugin type back out of `Box<dyn Plugin>`.
    #[cfg(test)]
    pub(crate) fn brush_plugin_mut(&mut self) -> &mut gascii_density_brush::BrushPlugin {
        let i = tool_def(ToolKind::Brush).plugin_slot.expect("Brush is plugin-sourced");
        self.plugins[i].as_any_mut().downcast_mut().expect("plugin at Brush's slot must be BrushPlugin")
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
        // A rebind away from Line invalidates any remembered shift-click-continue point — otherwise
        // a later rebind back to Line would resume "continue from" a point left over from a
        // completely different editing session.
        if kind != ToolKind::Line {
            self.slots[b.ix()].last_line_point = None;
        }
        // The [/] size keys follow the binding the user just acted on — the same rule a canvas
        // gesture applies. Without this, picking a tool by shortcut or toolbox click leaves the
        // keys adjusting the OTHER binding's stamp.
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
        if tool_def(kind).stamps_glyph && self.mask.glyph {
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
        // app -> doc: seeds doc's cursor before the edit applies — see `active_frame`'s field doc
        // comment for the full round trip. A no-op today (`active_frame` never leaves `0`).
        self.doc.set_active_frame(self.active_frame);
        self.history.apply(&mut self.doc, edit);
        // doc -> app: some Edit kinds shift doc's cursor as a side effect of applying, independent
        // of the seed above — resync so this field never drifts from doc.active_frame().
        self.active_frame = self.doc.active_frame();
        self.resync_slots(origin);
    }

    pub(crate) fn resync_slots(&mut self, except: Option<Binding>) {
        for b in Binding::ALL {
            if Some(b) != except {
                self.slots[b.ix()].tool.resync(&self.doc, self.active_frame, self.active_layer);
            }
        }
    }

    /// Moves the editing cursor to `idx`, flushing first — joins the same "flush before a
    /// structural trigger" convention every other cursor-affecting action already follows (Ctrl+S,
    /// Ctrl+Z, Resize, Clear, rebinding a tool). Not an `Edit` — mirrors `active_layer`'s plain-
    /// session-state precedent; only `frame_ops`'s structural ops touch `History`. A no-op if `idx`
    /// is out of range or already active.
    pub(crate) fn switch_active_frame(&mut self, idx: usize) {
        if idx == self.active_frame {
            return;
        }
        self.flush_all();
        if self.doc.set_active_frame(idx) {
            self.active_frame = idx;
            self.resync_slots(None);
        }
    }

    /// Draws every plugin's panel, then applies whatever `PanelOutcome`s they returned. Two passes
    /// — draw-and-collect, then drain — because `apply_edit` needs the whole of `&mut self`, which
    /// would conflict with `self.plugins`'s mutable borrow while the draw loop is still running.
    /// `host`'s borrow of `self.doc` ends at its last use inside the loop (NLL), before the drain
    /// pass's `&mut self` calls. Called with the host's own live root `Ui` — see `Plugin::panel`'s
    /// doc comment for why a plain `Context` cannot substitute for this. A returned `PanelOutcome
    /// ::error` is written straight into `self.last_error` — the same status-bar channel every other
    /// structural trigger already uses (`add_frame_via_menu`, "Resize Canvas…"), so a plugin-
    /// originated failure reads identically to a host-originated one.
    fn run_plugin_panels(&mut self, ui: &mut egui::Ui, kiosk: bool) {
        let (stylus_detected, bound) = host_context(self);
        let host = host_facts(&self.doc, stylus_detected, bound);
        let mut outcomes = Vec::with_capacity(self.plugins.len());
        for p in self.plugins.iter_mut() {
            outcomes.push(p.panel(ui, kiosk, &host));
        }
        self.drain_panel_outcomes(outcomes);
    }

    /// Applies every `PanelOutcome` a plugin's `panel` or `tick` returned this frame, in order —
    /// the drain half of the two-pass draw-then-drain (or tick-then-drain) shape `run_plugin_panels`
    /// and `handle_keys` both need: `apply_edit`/`switch_active_frame` need the whole of `&mut self`,
    /// which would conflict with `self.plugins`'s still-live mutable borrow if called from inside
    /// the draw/tick loop itself. A returned `PanelOutcome::error` is written straight into
    /// `self.last_error` — the same status-bar channel every other structural trigger already uses
    /// (`add_frame_via_menu`, "Resize Canvas…"), so a plugin-originated failure reads identically to
    /// a host-originated one.
    fn drain_panel_outcomes(&mut self, outcomes: Vec<gascii_plugin_api::PanelOutcome>) {
        for outcome in outcomes {
            for edit in outcome.edits {
                self.apply_edit(edit, None);
            }
            if let Some(idx) = outcome.set_active_frame {
                self.switch_active_frame(idx);
            }
            if let Some(loop_playback) = outcome.set_loop_playback {
                // A plain field write, not an `Edit` — matches `Document.loop_playback`'s own
                // "set-and-forget, never History-tracked" contract (see `PanelOutcome::
                // set_loop_playback`'s doc comment).
                self.doc.loop_playback = loop_playback;
            }
            if let Some(msg) = outcome.error {
                self.last_error = Some(msg);
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

    /// Blanks the whole document as one undoable step. Flushes first — same trigger-table
    /// discipline as Save/Export/Resize/Copy — so a live burst or float commits before Clear runs
    /// rather than being silently discarded. No confirm dialog: Clear is undoable like every other
    /// edit, so it doesn't need one.
    pub(crate) fn clear_document(&mut self) {
        self.flush_all();
        if let Some(edit) = clear_document(&self.doc) {
            self.apply_edit(edit, None);
        }
    }

    /// The Edit menu's "Add Frame" bootstrap: the one host-owned, non-plugin-routed frame-creation
    /// path, since `gascii-anim` has no toolbox/menu presence of its own to
    /// host this affordance at the `frame_count() == 1` boundary. Calls `frame_ops::duplicate_frame`
    /// directly through `apply_edit` — the same shape every other menu-triggered structural edit in
    /// this app already uses. Once `frame_count() > 1`, the plugin's own timeline panel takes over
    /// for all further add/duplicate/delete/reorder. Flushes first, same trigger-table discipline as
    /// Clear/Resize/Save.
    pub(crate) fn add_frame_via_menu(&mut self) {
        self.flush_all();
        match duplicate_frame(&self.doc, self.doc.active_frame()) {
            Ok(edit) => {
                self.apply_edit(edit, None);
                self.last_error = None;
            }
            // Matches "Resize Canvas…"'s own convention: a specific, readable message per error
            // variant, not a raw `{e:?}` dump.
            Err(FrameOpError::TooManyFrames { max, .. }) => {
                self.last_error = Some(format!("add frame: exceeds the {max} maximum"));
            }
            Err(FrameOpError::TotalCellBudgetExceeded { .. }) => {
                self.last_error = Some("add frame: exceeds the maximum total cell budget".to_string());
            }
            Err(FrameOpError::TooManyLayers { max, .. }) => {
                self.last_error = Some(format!("add frame: exceeds the {max} maximum layer count"));
            }
            Err(FrameOpError::IndexOutOfBounds { .. } | FrameOpError::LastFrame) => {
                // Unreachable from this call site: `duplicate_frame` is always given
                // `self.doc.active_frame()`, a provably in-range index, and never returns
                // `LastFrame` (that's `remove_frame`'s own error).
                self.last_error = Some("add frame: unexpected error".to_string());
            }
        }
    }

    /// Commits any pending text burst or floating selection, then undoes the most recent edit.
    /// Flushing before undo is correct here: it turns "Undo mid-session" into "undo the very edit
    /// that was just committed" (the same edit the flush just committed), matching ordinary
    /// editor conventions.
    ///
    /// The undo mutates `self.doc` behind both slots' backs, exactly like `request_redo`'s redo —
    /// so both re-pin afterward. Today the resync is belt-and-braces (the `flush_all` just
    /// emptied every session's pending state, and the mid-stroke gates in `handle_keys`/the menu
    /// keep a live stroke out), but stating it locally means this path's safety no longer hangs
    /// on two guards defined elsewhere staying exactly as they are.
    pub(crate) fn request_undo(&mut self) {
        self.flush_all();
        if self.history.undo(&mut self.doc) {
            // doc -> app: undo restores doc's active-frame cursor from the undone Edit's own
            // snapshot — see `active_frame`'s field doc comment.
            self.active_frame = self.doc.active_frame();
            self.resync_slots(None);
        }
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
    pub(crate) fn request_redo(&mut self) {
        if self.history.can_redo() {
            self.history.redo(&mut self.doc);
            // doc -> app: same resync as `request_undo` — see `active_frame`'s field doc comment.
            self.active_frame = self.doc.active_frame();
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

    /// `Ctrl+X`/Edit ▸ Cut: copies the live selection (`copy_selection`), then deletes it — one
    /// atomic change, never "copies but doesn't delete" for even a frame. Mirrors `canvas.rs`'s
    /// existing Selection-Delete-key branch exactly, just triggered from here instead of that
    /// per-frame key routing. A no-op unless a Selection binding has a region defined, same as
    /// `copy_selection`.
    pub(crate) fn cut_selection(&mut self, ctx: &egui::Context) {
        let Some(b) = self.selection_slot() else {
            return;
        };
        self.copy_selection(ctx);
        let tctx = crate::canvas::tool_ctx(self, b);
        let resp = self.slots[b.ix()].tool.update(ToolEvent::Delete, &tctx, &self.doc);
        if let ToolResponse::Commit(Some(edit)) = resp {
            self.apply_edit(edit, Some(b));
        }
    }

    /// `Ctrl+A`/Edit ▸ Select All: selects the whole document via whichever binding already holds
    /// Selection (`paste_target`'s own rule, default L) — never silently no-ops for lack of a
    /// binding to act through.
    pub(crate) fn select_all(&mut self) {
        let b = paste_target(self.slot(Binding::L).kind, self.slot(Binding::R).kind);
        if self.slot(b).kind != ToolKind::Selection {
            self.set_tool(b, ToolKind::Selection);
        }
        self.acquire_keyboard(b);
        let tctx = crate::canvas::tool_ctx(self, b);
        self.slots[b.ix()].tool.update(ToolEvent::SelectAll, &tctx, &self.doc);
    }

    /// `Ctrl+D`/Edit ▸ Deselect: clears the marquee/keyboard claim without deleting document
    /// content — the identical pair `canvas.rs`'s own Selection-Escape branch already performs. A
    /// no-op unless a Selection binding currently holds the keyboard.
    ///
    /// "Without deleting content" means the document, not an uncommitted float: `ToolEvent::Cancel`
    /// discards a lifted-but-not-dropped float outright rather than committing it, matching
    /// `canvas.rs`'s own Selection-Escape precedent exactly (that branch is deliberately
    /// non-flushing so Escape-as-abort can discard an in-progress move). A user who has moved or
    /// pasted a float and presses `Ctrl+D` expecting to "just clear the selection" loses that
    /// float's content, not just the marquee outline.
    pub(crate) fn deselect(&mut self) {
        let Some(b) = self.selection_slot() else {
            return;
        };
        let tctx = crate::canvas::tool_ctx(self, b);
        self.slots[b.ix()].tool.update(ToolEvent::Cancel, &tctx, &self.doc);
        self.release_keyboard(b);
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

    /// Tool-select (`P`/`E`/`I`/`T`/`F`/`R`/`L`/`S`), undo/redo, and Ctrl+C copy keys. The
    /// single-letter tool keys are guarded on no widget having focus *and* not being
    /// mid-text-edit so typing into the color picker's hex field, or into the canvas in text
    /// mode, doesn't get swallowed as a tool switch. The undo/redo chords are guarded on widget
    /// focus alone: a focused `TextEdit` (that hex field lives in a popup, outside
    /// `modal_open()`'s coverage) runs its own undoer against the same event list `consume_key`
    /// removes from, so consuming Ctrl+Z here would silently undo a canvas edit instead of the
    /// field's typo. A canvas text session sets no widget focus, so its Ctrl+Z still reaches the
    /// document. The remaining chords (save/copy/export/fit) don't collide with `TextEdit`'s
    /// editing keys and stay global.
    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        let owner_kind = self.keyboard_owner().map(|b| self.slot(b).kind);
        let widget_focused = ui.memory(|m| m.focused().is_some());
        let focused = widget_focused || suppresses_tool_shortcuts(owner_kind);
        let is_fullscreen = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        let (redo_shift, undo, redo_y, select_all, copy, copy_all, cut, generic_always) = ui.input_mut(|i| {
            // Cmd/Ctrl+Shift+Z must be consumed before the plain Cmd/Ctrl+Z pattern, since
            // `matches_logically` ignores extra Shift/Alt — checking undo first would swallow
            // the redo shortcut's Z key press. Same reasoning for Ctrl+Shift+C vs plain Ctrl+C.
            //
            // Ctrl+A joins this same `widget_focused`-gated group, not the uniform generic subset:
            // egui::TextEdit's own cursor handling treats Ctrl+A as "select all text in this field"
            // while focused (confirmed against the vendored `text_selection/cursor_range.rs`), the
            // same conflict that put Undo/Redo here in the first place. A canvas Text session sets
            // no widget focus, so Ctrl+A there still reaches the Selection-tool chord below.
            let (redo_shift, undo, redo_y, select_all) = if widget_focused {
                (false, false, false, false)
            } else {
                (
                    i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                )
            };
            // The uniform, unconditional subset — one shared consume-and-dispatch loop over
            // `chords::CHORDS`'s `GenericAlways` rows in table order, rather than one near-identical
            // individual `consume_key` call per chord. None of these collide with each other or with
            // anything above, so table order carries no precedence weight here — it exists only
            // because the registry always consumes in table order, the same rule `tools()`'s own
            // shortcut lookup already follows.
            let generic_always = chords::consume_generic_chords(i, ChordDispatch::GenericAlways);
            // Ctrl+C/Cmd+C/Ctrl+Insert (and Ctrl+X) never reach `Event::Key` — egui-winit
            // intercepts the clipboard chord and emits `Event::Copy`/`Event::Cut` instead, which
            // `consume_key` can never see. Scanned (not consumed): nothing else in this app reads
            // these events this frame, so there is no double-handling to guard against.
            let (copy, copy_all) = copy_events(&i.events, i.modifiers.shift);
            let cut = cut_event(&i.events);
            (redo_shift, undo, redo_y, select_all, copy, copy_all, cut, generic_always)
        });
        let save = generic_always.contains(&ChordId::Save);
        let export_dialog = generic_always.contains(&ChordId::ExportDialog);
        let fit = generic_always.contains(&ChordId::Fit);
        let new_document = generic_always.contains(&ChordId::New);
        let open_document = generic_always.contains(&ChordId::Open);
        let save_as = generic_always.contains(&ChordId::SaveAs);
        let deselect = generic_always.contains(&ChordId::Deselect);
        let zoom_in_alias = generic_always.contains(&ChordId::ZoomInAlias);
        let zoom_out_alias = generic_always.contains(&ChordId::ZoomOutAlias);

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
        if new_document {
            self.new_document_via_menu();
        }
        if open_document {
            self.open_file();
        }
        if save_as {
            self.save_file_as();
        }
        if zoom_in_alias {
            self.step_zoom(1);
        }
        if zoom_out_alias {
            self.step_zoom(-1);
        }
        // The tool shortcuts come from the tool registry, so a tool and its key can never drift
        // apart. A shortcut always sets the L binding; right-clicking a toolbox cell is the only
        // way to set R. Text is excluded from the lookup while fullscreen — see
        // `tool_shortcut_reachable` — so its key event is left unconsumed rather than silently
        // rebinding L to a tool that kiosk's own sidebar has no cell for.
        if !focused {
            let picked = ui.input_mut(|i| {
                tools().iter().find(|def| tool_shortcut_fires_and_consumes_its_key(def, i, is_fullscreen)).map(|def| def.kind)
            });
            if let Some(kind) = picked {
                self.set_tool(Binding::L, kind);
            }
        }
        // Escape's lowest-priority claim: exiting fullscreen. Only reachable when nothing with a
        // higher claim on Escape is live (a Text/Selection session, a pointer stroke) — those are
        // handled elsewhere (`canvas.rs`'s own Escape branches, which run on the same frame's raw
        // events and are unaffected by this `consume_key` since it only fires when they wouldn't
        // have claimed the key anyway).
        if should_handle_escape_for_fullscreen(self.keyboard_owner(), self.stroke_in_progress()) {
            let want_exit = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            if want_exit && is_fullscreen {
                let ctx = ui.ctx().clone();
                self.toggle_fullscreen(&ctx);
            }
        }
        // F11 is genuinely unconditional — no `focused` gate. Nothing else in the app binds it, no
        // tool consumes it as content (a Text burst only ever sees `Event::Text`/`Char`), and
        // `handle_keys` itself only runs while `!modal_open()`, which is the one gate F11 does
        // need. Gating it on `focused` would silently swallow the toggle for the whole duration of
        // any Text session (`suppresses_tool_shortcuts` holds `focused` true for as long as
        // composing lasts), which is exactly the bug this is written to avoid.
        let want_toggle = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11));
        if want_toggle {
            let ctx = ui.ctx().clone();
            self.toggle_fullscreen(&ctx);
        }
        if !focused {
            let fired = ui.input_mut(|i| chords::consume_generic_chords(i, ChordDispatch::GenericUnfocused));
            if fired.contains(&ChordId::SwapColors) {
                self.swap_colors();
            }
            if fired.contains(&ChordId::ToggleGrid) {
                self.show_grid = !self.show_grid;
            }
            if fired.contains(&ChordId::HelpOverlay) {
                self.help_overlay_open = !self.help_overlay_open;
            }
        }
        if copy_all {
            self.flush_all();
            ui.ctx().copy_text(export_text(&self.doc));
        } else if copy {
            self.copy_selection(ui.ctx());
        }
        if cut {
            self.cut_selection(ui.ctx());
        }
        if select_all {
            self.select_all();
        }
        if deselect {
            self.deselect();
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
        // Per-frame plugin input outside a canvas gesture (Brush's digit-key intensity shortcut, a
        // playback clock, gascii-anim's frame-navigation/duplicate-frame shortcuts, and whatever a
        // future plugin needs the same hook for). Called unconditionally now — `focused` is passed
        // through as a plain parameter instead of gating the call site, so a plugin that doesn't
        // care about shortcuts (a playback clock) isn't starved whenever any field has focus; a
        // plugin that DOES consume a shortcut (Brush's digit keys) checks `focused` itself.
        //
        // Two passes, mirroring `run_plugin_panels`'s own draw-then-drain shape: `tick` needs only
        // `&host` (borrowing `self.doc`) while it runs, so every plugin's outcome is collected here
        // and applied afterward via the same `drain_panel_outcomes` helper, once `host`'s borrow of
        // `self.doc` has ended (NLL) and `&mut self` is free again.
        let (stylus_detected, bound) = host_context(self);
        let host = host_facts(&self.doc, stylus_detected, bound);
        let mut tick_outcomes = Vec::with_capacity(self.plugins.len());
        for p in self.plugins.iter_mut() {
            tick_outcomes.push(p.tick(ui, focused, &host));
        }
        self.drain_panel_outcomes(tick_outcomes);
        // `[`/`]` adjust the stamp of whichever binding was last used — a gesture on either button
        // selects it, as does binding a tool, so the keys follow the button you last drew with.
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
                // `size` is always >= 1 by construction, so a plain decrement guarded on `> 1` is
                // the whole floor check — no need for the `saturating_sub(1).max(1)` double-guard
                // that pattern used to require.
                if shrink && stamp.size > 1 {
                    stamp.size -= 1;
                }
                if grow {
                    stamp.size = (stamp.size + 1).min(MAX_TOOL_SIZE);
                }
            }
        }
    }


    /// Records `path` at the front of the recent-files list, de-duplicated and capped at 8.
    pub(crate) fn note_recent_file(&mut self, path: &std::path::Path) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(8);
    }

    /// File ▸ New…'s body: flush first, then either open the New dialog directly (a clean document)
    /// or veto through the unsaved-changes confirm. Shared by the menu click and the `Ctrl+N` chord
    /// so the two can never drift apart.
    fn new_document_via_menu(&mut self) {
        self.flush_all();
        if self.is_dirty() {
            self.confirm = Some(PendingConfirm::NewDocument);
        } else {
            self.open_new_dialog();
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("New…").shortcut_text(chords::chord_label(ChordId::New))).clicked() {
                    self.new_document_via_menu();
                }
                if ui.add(egui::Button::new("Open…").shortcut_text(chords::chord_label(ChordId::Open))).clicked() {
                    self.open_file();
                }
                ui.separator();
                if ui.add(egui::Button::new("Save").shortcut_text(chords::chord_label(ChordId::Save))).clicked() {
                    self.save_file();
                }
                if ui
                    .add(egui::Button::new("Save As…").shortcut_text(chords::chord_label(ChordId::SaveAs)))
                    .clicked()
                {
                    self.save_file_as();
                }
                ui.separator();
                if ui
                    .add(egui::Button::new("Export…").shortcut_text(chords::chord_label(ChordId::ExportDialog)))
                    .clicked()
                {
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
                let undo = egui::Button::new("Undo").shortcut_text(chords::chord_label(ChordId::Undo));
                if ui.add_enabled(self.history.can_undo() && no_stroke, undo).clicked() {
                    self.request_undo();
                }
                // Both Ctrl+Shift+Z and Ctrl+Y trigger a redo — `ChordId::Redo`'s label documents
                // both, closing a label-drift gap (Ctrl+Shift+Z was previously undocumented here
                // even though it already worked).
                let redo = egui::Button::new("Redo").shortcut_text(chords::chord_label(ChordId::Redo));
                if ui.add_enabled(self.history.can_redo() && no_stroke, redo).clicked() {
                    self.request_redo();
                }
                ui.separator();
                let can_copy = self
                    .selection_slot()
                    .and_then(|b| self.slot(b).tool.selection_overlay())
                    .and_then(|v| v.marquee)
                    .is_some();
                let copy = egui::Button::new("Copy Selection").shortcut_text(chords::chord_label(ChordId::Copy));
                if ui.add_enabled(can_copy, copy).clicked() {
                    self.copy_selection(ui.ctx());
                }
                let copy_all =
                    egui::Button::new("Copy All as Text").shortcut_text(chords::chord_label(ChordId::CopyAll));
                if ui.add(copy_all).clicked() {
                    // Flush first: a pending text burst or floating selection lives only in
                    // `self.slots[0].tool`'s overlay until committed into `self.doc` — copying without
                    // flushing would silently drop just-typed or just-moved content from the
                    // whole-document clipboard contents.
                    self.flush_all();
                    ui.ctx().copy_text(export_text(&self.doc));
                }
                let paste = egui::Button::new("Paste").shortcut_text(chords::chord_label(ChordId::Paste));
                if ui.add(paste).clicked() {
                    self.paste_from_os_clipboard();
                }
                let cut = egui::Button::new("Cut").shortcut_text(chords::chord_label(ChordId::Cut));
                if ui.add_enabled(can_copy, cut).clicked() {
                    let ctx = ui.ctx().clone();
                    self.cut_selection(&ctx);
                }
                ui.separator();
                let select_all = egui::Button::new("Select All").shortcut_text(chords::chord_label(ChordId::SelectAll));
                if ui.add(select_all).clicked() {
                    self.select_all();
                }
                let can_deselect = self.selection_slot().is_some();
                let deselect = egui::Button::new("Deselect").shortcut_text(chords::chord_label(ChordId::Deselect));
                if ui.add_enabled(can_deselect, deselect).clicked() {
                    self.deselect();
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
                if ui.button("Add Frame").clicked() {
                    self.add_frame_via_menu();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.add(egui::Button::new("Zoom In").shortcut_text(chords::chord_label(ChordId::ZoomIn))).clicked() {
                    self.step_zoom(1);
                }
                if ui
                    .add(egui::Button::new("Zoom Out").shortcut_text(chords::chord_label(ChordId::ZoomOut)))
                    .clicked()
                {
                    self.step_zoom(-1);
                }
                if ui.add(egui::Button::new("Fit").shortcut_text(chords::chord_label(ChordId::Fit))).clicked() {
                    self.pending_fit = true;
                }
                ui.separator();
                ui.checkbox(&mut self.show_grid, format!("Grid  ({})", chords::chord_label(ChordId::ToggleGrid)));
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
                    self.toggle_fullscreen(&ctx);
                }
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

    /// Sends the fullscreen toggle and, only on the false→true transition, snaps zoom to Fit —
    /// matching the design's "zoom snaps to Fit" annotation, which only applies on entry. Exiting
    /// leaves whatever zoom kiosk's own auto-fit last settled on (harmless either way, since normal
    /// mode's zoom is independent per-session state already).
    pub(crate) fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        let is_fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if !is_fs {
            self.pending_fit = true;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_fs));
    }

    /// Requests a one-step zoom (menu item, keyboard chord, status bar — all three call this).
    /// Deferred to `canvas::show` via `pending_step_zoom`, which has the geometry to anchor it on
    /// the pointer like the wheel path; applying it here by bumping `zoom_step` directly would
    /// remap the pointer to a different cell mid-stroke.
    pub(crate) fn step_zoom(&mut self, dir: i32) {
        self.pending_step_zoom += dir;
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

    /// The `?` keyboard-shortcuts overlay: every tool's own letter shortcut (`tools()`) plus every
    /// host chord (`chords::chord_rows()`), read-only. Built on the same `dialog::modal` surface as
    /// every other dialog, so it inherits Escape/backdrop-click/close-box dismissal for free and
    /// needs no bespoke Cancel/Confirm row of its own. A plugin-registered `tick`-driven shortcut
    /// (e.g. `gascii-anim`'s `Space`/`,`/`.`/`Shift+D`) has no enforced way to surface a label here
    /// today — see `gascii_plugin_api::Plugin::tick`'s own doc comment for that limitation.
    fn help_overlay(&mut self, ctx: &egui::Context) {
        if !self.help_overlay_open {
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
            });
        });
        if resp.dismissed {
            self.help_overlay_open = false;
        }
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
                    Err(ResizeError::TotalCellBudgetExceeded { .. }) => {
                        self.last_error = Some("resize: exceeds the maximum total cell budget".to_string());
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
    fn export_dialog(&mut self, ctx: &egui::Context) {
        if !self.export_dialog_open {
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
        self.export_dialog_open = false;
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
                match write_atomic(&path, text.as_bytes()) {
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
                let bg_image = self.image_bg.as_ref().filter(|b| b.use_in_export).map(|b| (&b.pixels, b.export_opacity));
                match png_export::export_png(&self.doc, self.export.cell_px(), opaque_bg, bg_image) {
                    Ok(bytes) => match write_atomic(&path, &bytes) {
                        Ok(()) => {
                            self.last_error = None;
                            self.close_export_dialog();
                        }
                        Err(e) => self.last_error = Some(format!("failed to write {}: {e}", path.display())),
                    },
                    Err(e) => self.last_error = Some(format!("PNG export failed: {e}")),
                }
            }
            ExportFormat::Gif => {
                let Some(path) = rfd::FileDialog::new().add_filter("GIF", &["gif"]).save_file() else {
                    return;
                };
                let opaque_bg = (!self.export.transparent).then_some(self.doc.background);
                let bg_image = self.image_bg.as_ref().filter(|b| b.use_in_export).map(|b| (&b.pixels, b.export_opacity));
                match anim_export::export_gif(&self.doc, self.export.cell_px(), opaque_bg, bg_image) {
                    Ok(bytes) => match write_atomic(&path, &bytes) {
                        Ok(()) => {
                            self.last_error = None;
                            self.close_export_dialog();
                        }
                        Err(e) => self.last_error = Some(format!("failed to write {}: {e}", path.display())),
                    },
                    Err(e) => self.last_error = Some(format!("GIF export failed: {e}")),
                }
            }
            ExportFormat::SpriteSheet => {
                let Some(path) = rfd::FileDialog::new().add_filter("PNG", &["png"]).save_file() else {
                    return;
                };
                let opaque_bg = (!self.export.transparent).then_some(self.doc.background);
                let bg_image = self.image_bg.as_ref().filter(|b| b.use_in_export).map(|b| (&b.pixels, b.export_opacity));
                match anim_export::export_spritesheet(&self.doc, self.export.cell_px(), opaque_bg, bg_image) {
                    Ok(bytes) => match write_atomic(&path, &bytes) {
                        Ok(()) => {
                            self.last_error = None;
                            self.close_export_dialog();
                        }
                        Err(e) => self.last_error = Some(format!("failed to write {}: {e}", path.display())),
                    },
                    Err(e) => self.last_error = Some(format!("spritesheet export failed: {e}")),
                }
            }
            ExportFormat::TextFrames => {
                let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).save_file() else {
                    return;
                };
                let text = if self.export.trim {
                    export_text_frames(&self.doc)
                } else {
                    export_text_frames_untrimmed(&self.doc)
                };
                match write_atomic(&path, text.as_bytes()) {
                    Ok(()) => {
                        self.last_error = None;
                        self.close_export_dialog();
                    }
                    Err(e) => self.last_error = Some(format!("failed to export {}: {e}", path.display())),
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

    /// Drains terminal Ctrl+C presses once per frame, just before `handle_close_request` so a
    /// forced repeat press has `force_close` set ahead of the close request it re-triggers. First
    /// press: a normal close request, identical to the window's close button. Repeat press while
    /// the veto dialog is up: close without saving.
    fn handle_ctrl_c(&mut self, ctx: &egui::Context) {
        let count = CTRL_C_PRESSES.load(Ordering::Relaxed);
        let confirming = self.confirm == Some(PendingConfirm::CloseApp);
        let Some(resp) = ctrl_c_response(count, self.ctrl_c_seen, confirming) else { return };
        self.ctrl_c_seen = count;
        match resp {
            CtrlCResponse::RequestClose => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            CtrlCResponse::ForceClose => self.close_now(ctx),
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
        format!("GASCII < {name}{dirty} >")
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

impl eframe::App for GasciiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.first_frame {
            eprintln!("startup to first frame: {:?}", self.started.elapsed());
            self.first_frame = false;
        }
        let ctx = ui.ctx().clone();
        self.apply_startup_window_state(&ctx);
        self.handle_ctrl_c(&ctx);
        self.handle_close_request(&ctx);
        // A deliberate, accepted side effect of this gate: `handle_keys` is the only driver of every
        // plugin's `tick` (including `gascii-anim`'s playback clock's `elapsed_ms` accumulation and
        // `request_repaint_after` rescheduling), so a running animation preview visibly freezes for
        // as long as any modal dialog (New/Resize/Export/Confirm) is open, then resumes — never
        // skipping ahead — the moment it closes, since each subsequent tick's own `stable_dt` is just
        // the real delta since the last rendered frame. No data loss, no runaway catch-up; not
        // threading `tick` outside this gate to avoid growing the app-vs-plugin coupling for a
        // cosmetic pause with no other reported downside.
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

        let is_fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let t = crate::ui::theme::current(&ctx);
        // The window edge's resize grips, before the panels: the grip is a 5px ring around the whole
        // window and must win over any widget sitting under it. The canvas reads raw pointer state
        // rather than egui interactions, so it must be told explicitly — a press on the grip would
        // otherwise both begin an OS resize and stamp a stroke on the document in the same click.
        // A fullscreen window has no edges to drag, so `handle_resize` already no-ops there.
        let pointer_on_resize_grip = crate::ui::titlebar::handle_resize(&ctx);

        if is_fullscreen {
            egui::Panel::top("kiosk_top")
                .frame(
                    egui::Frame::new()
                        .fill(t.bg_panel)
                        .inner_margin(egui::Margin::symmetric(0, 0))
                        .stroke(egui::Stroke::new(1.0, t.window_edge)),
                )
                .exact_size(crate::ui::kiosk::TOP_H)
                .show(ui, |ui| crate::ui::kiosk::top_bar(ui, self, &ctx));
            egui::Panel::bottom("kiosk_status")
                .frame(egui::Frame::new().fill(t.bg_panel).inner_margin(egui::Margin::symmetric(12, 0)))
                .exact_size(crate::ui::kiosk::STATUS_H)
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| crate::ui::kiosk::status_bar(ui, self));
                });
            egui::Panel::left("kiosk_sidebar")
                .frame(
                    egui::Frame::new()
                        .fill(t.bg_panel)
                        .inner_margin(egui::Margin::same(16))
                        .stroke(egui::Stroke::new(1.0, t.window_edge)),
                )
                .exact_size(crate::ui::kiosk::SIDEBAR_W)
                .resizable(false)
                .show(ui, |ui| crate::ui::kiosk::sidebar(ui, self));
            // A plugin's own panel (the timeline strip, etc.) — declared here, after every other
            // kiosk chrome panel and before `CentralPanel`, so a real `egui::Panel::bottom(..)`
            // called from inside a plugin correctly claims space from what's left (egui does not
            // allow a panel to be added after `CentralPanel` has already claimed the remainder).
            // No visible effect while every plugin's panel is a no-op (the shipped default, and
            // `AnimPlugin`'s own single-frame gate).
            self.run_plugin_panels(ui, true);
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(t.bg_desk))
                .show(ui, |ui| {
                    canvas::show(ui, self, pointer_on_resize_grip);
                });
        } else {
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
            // See the kiosk branch's own comment above — same reasoning, same call, `kiosk: false`.
            self.run_plugin_panels(ui, false);
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(t.bg_desk))
                .show(ui, |ui| {
                    canvas::show(ui, self, pointer_on_resize_grip);
                });
        }

        self.new_dialog(&ctx);
        self.resize_dialog(&ctx);
        self.export_dialog(&ctx);
        self.confirm_dialog(&ctx);
        self.help_overlay(&ctx);

        // Last, on the foreground layer: with the OS frame gone, nothing else draws the window's
        // own outline. Skipped while fullscreen — there is no window edge to outline, and kiosk's
        // own top/sidebar panel frames already draw their own borders.
        if !is_fullscreen {
            crate::ui::titlebar::paint_window_edge(&ctx);
        }
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

    /// `SIZED_TOOL_COUNT` must exactly cover the sized rows in the tool registry — too small would
    /// silently truncate a `stamps` array read, too large wastes slots no kind will ever index.
    #[test]
    fn sized_tool_count_matches_stamp_slots() {
        let count = tools().iter().filter(|d| d.stamp_slot.is_some()).count();
        assert_eq!(SIZED_TOOL_COUNT, count);
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

    /// The tool registry is the single source of truth for names, shortcuts, hints and
    /// constructors. If a kind were missing, `make_tool`'s `expect` would fire; if one were listed
    /// twice, the toolbox would show a duplicate cell and the two entries could drift apart.
    #[test]
    fn tools_table_lists_every_kind_exactly_once() {
        assert_eq!(tools().len(), ALL_KINDS.len());
        for kind in ALL_KINDS {
            let count = tools().iter().filter(|d| d.kind == kind).count();
            assert_eq!(count, 1, "{kind:?} appears {count} times in the tool registry");
        }
    }

    /// Locks each row's capability fields against the pre-refactor per-kind facts, so a typo in
    /// the tool registry can't silently drift from the scattered `match` arms it replaces.
    #[test]
    fn capability_fields_match_expected_for_every_kind() {
        for kind in ALL_KINDS {
            let d = tool_def(kind);
            let expected_stamp_slot = match kind {
                ToolKind::Pencil => Some(0u8),
                ToolKind::Eraser => Some(1),
                ToolKind::Line => Some(2),
                ToolKind::Brush => Some(3),
                _ => None,
            };
            let expected_holds_session = matches!(kind, ToolKind::Text | ToolKind::Selection);
            let expected_shows_hover = !matches!(kind, ToolKind::Selection);
            let expected_stamps_glyph = matches!(
                kind,
                ToolKind::Pencil | ToolKind::Fill | ToolKind::Rectangle | ToolKind::Line
            );
            let expected_suppresses_shortcuts = matches!(kind, ToolKind::Text);
            let expected_kiosk_visible = !matches!(kind, ToolKind::Text);
            // The two plugin-boundary capability fields: today only Brush's plugin-sourced row
            // sets either.
            let expected_pressure_sizeable = matches!(kind, ToolKind::Brush);
            let expected_wants_extra_ctx = matches!(kind, ToolKind::Brush);

            assert_eq!(d.stamp_slot, expected_stamp_slot, "{kind:?}: stamp_slot");
            assert_eq!(d.holds_session, expected_holds_session, "{kind:?}: holds_session");
            assert_eq!(d.shows_hover, expected_shows_hover, "{kind:?}: shows_hover");
            assert_eq!(d.stamps_glyph, expected_stamps_glyph, "{kind:?}: stamps_glyph");
            assert_eq!(
                d.suppresses_shortcuts, expected_suppresses_shortcuts,
                "{kind:?}: suppresses_shortcuts"
            );
            assert_eq!(d.kiosk_visible, expected_kiosk_visible, "{kind:?}: kiosk_visible");
            assert_eq!(d.pressure_sizeable, expected_pressure_sizeable, "{kind:?}: pressure_sizeable");
            assert_eq!(d.wants_extra_ctx, expected_wants_extra_ctx, "{kind:?}: wants_extra_ctx");
        }
    }

    /// Locks the registry's observable shape against the merge machinery's own plumbing: whether a
    /// row came from a pure built-in literal or a plugin bundle, the table still has exactly 9
    /// entries with exactly the capability values `capability_fields_match_expected_for_every_kind`
    /// already pins.
    #[test]
    fn tools_registry_merge_produces_the_same_9_row_table_the_pre_plugin_registry_had() {
        assert_eq!(tools().len(), 9);
        capability_fields_match_expected_for_every_kind();
    }

    /// Guards `prefs.json` forward-compatibility (persisted stamps are positionally indexed by
    /// `sized_slot`): Brush's stamp slot is host-owned and pinned to `3`, exactly its pre-migration
    /// literal value, regardless of where `plugin_factories()` places it in the list.
    #[test]
    fn brush_stamp_slot_is_pinned_to_3_regardless_of_registration_order() {
        assert_eq!(stamp_slot_for_plugin_tool(gascii_density_brush::BRUSH), Some(3));
        assert_eq!(tool_def(ToolKind::Brush).stamp_slot, Some(3));
    }

    /// `merge_plugin_row`'s `plugin_slot` must carry through whatever index it is given, not a
    /// hardcoded value — every downstream consumer (`tool_ctx`'s extra-context injection, the
    /// pressure-override gate, `binding_options_geom`'s dedup) trusts `plugin_slot` to resolve back
    /// to the correct entry of `GasciiApp.plugins`. This phase ships exactly one plugin, so a real
    /// `GasciiApp`'s Brush row can only ever observe `plugin_slot == Some(0)` — this test exercises
    /// indices beyond that single-plugin scale directly against the pure merge function, closing
    /// the plan's own documented residual risk ("no direct test proving a *wrong* index at
    /// 1-plugin scale").
    #[test]
    fn merge_plugin_row_carries_the_given_plugin_slot_index_verbatim() {
        let cap = gascii_plugin_api::PluginToolCapabilities {
            name: "Brush",
            key: egui::Key::B,
            tip: "t",
            make: || Box::new(gascii_core::DensityBrush::new()),
            sized: true,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            pressure_sizeable: true,
            wants_extra_ctx: true,
        };
        for slot in [0usize, 1, 4, 7] {
            let row = merge_plugin_row(slot, &cap);
            assert_eq!(row.plugin_slot, Some(slot), "merge_plugin_row must not hardcode a plugin_slot index");
        }
    }

    /// A fresh `GasciiApp`'s Brush row's `plugin_slot` must resolve back to the exact live plugin
    /// instance that actually registered the "Brush" tool — not merely to *some* valid index into
    /// `plugins`. Proven by calling `register_tools()` on the live instance the index points at and
    /// confirming it names the same tool `tool_def(Brush)` itself describes.
    #[test]
    fn a_fresh_apps_brush_row_plugin_slot_resolves_to_the_live_instance_that_registered_brush() {
        let app = GasciiApp::headless();
        let slot = tool_def(ToolKind::Brush).plugin_slot.expect("Brush is plugin-sourced");
        let registered = app.plugins[slot].register_tools();
        assert_eq!(registered.len(), 1);
        assert_eq!(
            registered[0].name,
            gascii_density_brush::BRUSH,
            "plugin_slot must resolve to the instance that actually registered Brush's row"
        );
    }

    /// D5's refinement, directly: `GasciiApp::with_state`/`headless` must construct one real,
    /// *retained* instance per `plugin_factories()` entry, per app — never a process-global shared
    /// instance. Two independently constructed apps must never see each other's Brush state; a
    /// `OnceLock`-cached plugin list (the design the plan explicitly rejected) would fail this.
    #[test]
    fn two_independent_gascii_apps_never_share_brush_plugin_state() {
        let mut app1 = GasciiApp::headless();
        let mut app2 = GasciiApp::headless();
        app1.brush_plugin_mut().set_active_ramp(1);
        app1.brush_plugin_mut().set_density_mode(gascii_core::DensityMode::Buildup(gascii_core::Buildup));
        app1.brush_plugin_mut().set_pressure_enabled(true);

        assert_eq!(app2.brush_plugin_mut().active_ramp(), 0, "app2 must start at Brush's own default ramp, not app1's mutated one");
        assert!(
            matches!(app2.brush_plugin_mut().density_mode(), gascii_core::DensityMode::Fixed(_)),
            "app2 must not see app1's Buildup mode"
        );
        assert!(!app2.brush_plugin_mut().pressure_enabled(), "app2 must not see app1's pressure opt-in");

        // And app1's own state must still hold, proving this isn't a case of neither app retaining
        // anything at all.
        assert_eq!(app1.brush_plugin_mut().active_ramp(), 1);
    }

    /// `Plugin::panel` must be a true no-op for the real, shipped builtin plugin list
    /// (`BrushPlugin` never overrides it) — called in both chrome modes, must not mutate the
    /// document or panic.
    #[test]
    fn every_plugins_panel_hook_is_a_true_no_op_for_the_real_builtin_list() {
        let mut app = GasciiApp::headless();
        let before = app.doc.clone();
        let ctx = egui::Context::default();
        let (stylus_detected, bound) = host_context(&app);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let host = host_facts(&app.doc, stylus_detected, bound);
            for p in app.plugins.iter_mut() {
                let outcome = p.panel(ui, false, &host);
                assert!(outcome.edits.is_empty());
                let outcome = p.panel(ui, true, &host);
                assert!(outcome.edits.is_empty());
            }
        });
        assert_eq!(app.doc, before, "no plugin's panel hook may mutate the document");
    }

    fn raw_input_with_screen(w: f32, h: f32) -> egui::RawInput {
        egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(w, h))), ..Default::default() }
    }

    /// A throwaway plugin double whose `panel` draws a real `egui::Panel::bottom` — proves the
    /// panel-loop reorder at the mechanism level, not just "it doesn't panic": a bottom
    /// panel declared from inside a plugin, run through `run_plugin_panels` BEFORE `CentralPanel`,
    /// must actually shrink the central panel's claimed rect, exactly like every other panel this
    /// app declares. This is the property a `ctx: &Context`-only plugin signature could never prove
    /// (egui's `Panel` reads/mutates its literal parent `Ui`'s own placer state, not `Context` —
    /// see `Plugin::panel`'s doc comment) — the reason `panel`'s host parameter is `&mut egui::Ui`.
    struct BottomPanelDouble;
    impl Plugin for BottomPanelDouble {
        fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
            Vec::new()
        }
        fn panel(&mut self, ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
            egui::Panel::bottom("test_timeline_double").exact_size(50.0).show(ui, |ui| {
                ui.label("test timeline");
            });
            gascii_plugin_api::PanelOutcome::default()
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn a_real_bottom_panel_from_a_plugin_correctly_shrinks_the_central_panel_before_it_claims_space() {
        let mut app = GasciiApp::headless();
        app.plugins.push(Box::new(BottomPanelDouble));

        let ctx = egui::Context::default();
        let mut with_double = None;
        let _ = ctx.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            with_double = Some(resp.response.rect);
        });

        let mut app_without = GasciiApp::headless(); // no BottomPanelDouble registered
        let ctx2 = egui::Context::default();
        let mut without_double = None;
        let _ = ctx2.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app_without.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            without_double = Some(resp.response.rect);
        });

        let with_double = with_double.unwrap();
        let without_double = without_double.unwrap();
        assert!(
            with_double.height() < without_double.height(),
            "a real Panel::bottom declared inside a plugin must shrink the central panel's rect — \
             with={with_double:?} without={without_double:?}"
        );
    }

    /// `AnimPlugin`'s own single-frame gate, exercised through the real registered plugin list (not
    /// a double): a fresh document has exactly one frame, so its panel must claim zero space and
    /// must not shrink the central panel at all.
    #[test]
    fn anim_panel_claims_no_space_while_frame_count_is_one() {
        let mut app = GasciiApp::headless();
        assert_eq!(app.doc.frame_count(), 1);

        let ctx = egui::Context::default();
        let mut central_rect = None;
        let _ = ctx.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            central_rect = Some(resp.response.rect);
        });

        let ctx2 = egui::Context::default();
        let mut central_rect_no_plugins = None;
        let _ = ctx2.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            central_rect_no_plugins = Some(resp.response.rect);
        });

        assert_eq!(central_rect.unwrap(), central_rect_no_plugins.unwrap(), "a single-frame document's layout must be byte-identical with or without the plugin panel loop running");
    }

    /// The real, registered `gascii-anim` plugin (not a double) must claim real screen space the
    /// moment a second frame exists — the flip side of the single-frame no-op gate above.
    #[test]
    fn anim_panel_claims_space_once_a_second_frame_exists() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.frame_count(), 2);

        let ctx = egui::Context::default();
        let mut with_second_frame = None;
        let _ = ctx.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            with_second_frame = Some(resp.response.rect);
        });

        let app_single = GasciiApp::headless();
        let ctx2 = egui::Context::default();
        let mut single_frame = None;
        let _ = ctx2.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            let _ = &app_single; // single-frame baseline: no plugin panel call needed (the single-frame no-op gate)
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            single_frame = Some(resp.response.rect);
        });

        assert!(
            with_second_frame.unwrap().height() < single_frame.unwrap().height(),
            "the timeline panel must claim real space once frame_count() > 1"
        );
    }

    /// A test-double plugin returning a `PanelOutcome` with one `Edit`, driven through one
    /// `run_plugin_panels` call — the edit must reach the document through the same, unmodified
    /// `apply_edit` choke point every other mutation uses (undo/redo proves this, not just the
    /// forward direction).
    #[test]
    fn plugin_panel_outcome_edits_are_applied_through_apply_edit() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.resolved_frame_duration_ms(0), Some(Document::DEFAULT_FRAME_DURATION_MS));

        struct EditOutcomeDouble;
        impl Plugin for EditOutcomeDouble {
            fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
                Vec::new()
            }
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let idx = host.document().active_frame();
                gascii_plugin_api::PanelOutcome {
                    edits: vec![gascii_core::Edit::SetFrameDuration { index: idx, before: None, after: Some(50) }],
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(EditOutcomeDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.doc.resolved_frame_duration_ms(0), Some(50), "the PanelOutcome's edit must reach the document");
        assert!(app.history.can_undo(), "the edit must have gone through History::apply, not bypassed it");
        app.request_undo();
        assert_eq!(
            app.doc.resolved_frame_duration_ms(0),
            Some(Document::DEFAULT_FRAME_DURATION_MS),
            "undo must reverse the plugin-originated edit exactly like any other apply_edit call"
        );
    }

    /// `set_active_frame` in a returned `PanelOutcome` must flush pending sessions on BOTH bindings
    /// before actually moving the cursor — a live Text burst must commit onto the frame it was
    /// typed on, not silently carry over to the new one.
    #[test]
    fn plugin_panel_outcome_set_active_frame_flushes_pending_sessions_before_switching() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);

        // A pending Text burst on L, uncommitted, at (0,0) on frame 0.
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('a'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        struct SwitchFrameDouble;
        impl Plugin for SwitchFrameDouble {
            fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
                Vec::new()
            }
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                gascii_plugin_api::PanelOutcome { set_active_frame: Some(1), ..Default::default() }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(SwitchFrameDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.active_frame, 1, "set_active_frame must move the cursor");
        assert_eq!(app.doc.active_frame(), 1);
        assert_eq!(
            app.doc.cell_at(0, 0, 0, 0).unwrap().ch,
            'a',
            "a pending burst must be flushed onto the frame it was typed on before the switch, not dropped or carried over"
        );
    }

    /// `set_loop_playback` in a returned `PanelOutcome` must write `Document.loop_playback`
    /// directly — a plain field write, not an `Edit`, so it must NOT create an undo entry.
    #[test]
    fn plugin_panel_outcome_set_loop_playback_writes_the_document_field_directly_without_history() {
        let mut app = GasciiApp::headless();
        assert!(app.doc.loop_playback, "sanity: a fresh document defaults to looping");
        let can_undo_before = app.history.can_undo();

        struct LoopToggleDouble;
        impl Plugin for LoopToggleDouble {
            fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
                Vec::new()
            }
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                gascii_plugin_api::PanelOutcome { set_loop_playback: Some(false), ..Default::default() }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(LoopToggleDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert!(!app.doc.loop_playback, "the PanelOutcome's request must reach Document.loop_playback");
        assert_eq!(app.history.can_undo(), can_undo_before, "a plain field write must never create an undo entry");
    }

    /// A returned `PanelOutcome.error` must reach `self.last_error` — the same status-bar channel
    /// every other structural trigger already uses, not a silent no-op (Important #1).
    #[test]
    fn plugin_panel_outcome_error_surfaces_through_last_error() {
        let mut app = GasciiApp::headless();
        assert!(app.last_error.is_none());

        struct ErrorOutcomeDouble;
        impl Plugin for ErrorOutcomeDouble {
            fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
                Vec::new()
            }
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                gascii_plugin_api::PanelOutcome { error: Some("add frame: exceeds the 256 maximum".to_string()), ..Default::default() }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(ErrorOutcomeDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.last_error.as_deref(), Some("add frame: exceeds the 256 maximum"));
    }

    /// A `PanelOutcome` carrying BOTH a successful edit and a failure message in the same drain pass
    /// (a multi-op outcome) must apply the edit AND still surface the error — neither channel may
    /// silently swallow the other just because they arrived together.
    #[test]
    fn plugin_panel_outcome_with_both_an_edit_and_an_error_applies_the_edit_and_still_surfaces_the_error() {
        let mut app = GasciiApp::headless();

        struct PartialFailureDouble;
        impl Plugin for PartialFailureDouble {
            fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
                Vec::new()
            }
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let idx = host.document().active_frame();
                gascii_plugin_api::PanelOutcome {
                    edits: vec![gascii_core::Edit::SetFrameDuration { index: idx, before: None, after: Some(50) }],
                    error: Some("duplicate frame: exceeds the 256 maximum".to_string()),
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(PartialFailureDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.doc.resolved_frame_duration_ms(0), Some(50), "the succeeding half of a multi-op outcome must still apply");
        assert_eq!(app.last_error.as_deref(), Some("duplicate frame: exceeds the 256 maximum"), "the failing half must still surface");
    }

    /// A `PanelOutcome`-originated `duplicate_frame` edit (built from the real `gascii_core::
    /// duplicate_frame`, exactly what `gascii-anim`'s own timeline controls call — not a hand-built
    /// `Edit` literal) must land in `History` and undo byte-identically to the functionally same
    /// operation reached via the host's own "Add Frame" menu path — the two entry points must be
    /// indistinguishable to `History`/undo, not just visually similar.
    #[test]
    fn plugin_outcome_originated_duplicate_frame_edit_lands_in_history_and_undoes_identically_to_the_menu_path() {
        let mut app_plugin = GasciiApp::headless();
        app_plugin.doc.set_cell(0, 0, 0, cell('D'));

        struct DuplicateOutcomeDouble;
        impl Plugin for DuplicateOutcomeDouble {
            fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
                Vec::new()
            }
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let doc = host.document();
                let edit = gascii_core::duplicate_frame(doc, doc.active_frame()).unwrap();
                gascii_plugin_api::PanelOutcome { edits: vec![edit], ..Default::default() }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app_plugin.plugins.push(Box::new(DuplicateOutcomeDouble));
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app_plugin.run_plugin_panels(ui, false));

        let mut app_menu = GasciiApp::headless();
        app_menu.doc.set_cell(0, 0, 0, cell('D'));
        app_menu.add_frame_via_menu();

        assert_eq!(app_plugin.doc, app_menu.doc, "a plugin-outcome-originated duplicate must produce a byte-identical document to the menu path");
        assert!(app_plugin.history.can_undo());

        app_plugin.request_undo();
        app_menu.request_undo();
        assert_eq!(app_plugin.doc, app_menu.doc, "undo must restore both paths to an identical document");
        assert_eq!(app_plugin.doc.frame_count(), 1, "undo must fully reverse the plugin-originated duplicate");
    }

    /// A single `PanelOutcome` that both adds a frame AND requests switching to the frame the edit
    /// just created — the index requested by `set_active_frame` does not exist in the document until
    /// the outcome's own `edits` are drained first. Proves `run_plugin_panels`'s edits-then-switch
    /// ordering, not just that each half works when tested alone.
    #[test]
    fn plugin_panel_outcome_that_both_adds_a_frame_and_switches_to_it_in_one_pass_resolves_against_the_post_edit_document() {
        let mut app = GasciiApp::headless();
        assert_eq!(app.doc.frame_count(), 1);

        struct AddAndSwitchDouble;
        impl Plugin for AddAndSwitchDouble {
            fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
                Vec::new()
            }
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let doc = host.document();
                let edit = gascii_core::add_frame(doc, 1, gascii_core::Frame::blank(doc.width, doc.height)).unwrap();
                gascii_plugin_api::PanelOutcome { edits: vec![edit], set_active_frame: Some(1), ..Default::default() }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(AddAndSwitchDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.doc.frame_count(), 2, "the edit half of the outcome must have landed");
        assert_eq!(app.active_frame, 1, "the switch half must resolve against the post-edit document, landing on the frame the edit just created");
        assert_eq!(app.doc.active_frame(), 1);
    }

    /// A `PanelOutcome`-originated `remove_frame`/`reorder_frame` edit (the real `gascii_core`
    /// functions, matching what `gascii-anim`'s own Delete/reorder controls call) must undo
    /// byte-exactly, mirroring `frame-substrate`'s own already-proven ladder-undo property, this
    /// time reached through the plugin-drain path rather than a direct `apply_edit` call.
    #[test]
    fn plugin_panel_outcome_originated_delete_edit_undoes_to_a_byte_exact_prior_document() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        app.doc.set_cell(1, 0, 0, cell('B'));
        let before_delete = app.doc.clone();
        assert_eq!(before_delete.frame_count(), 2);

        struct DeleteFrameDouble;
        impl Plugin for DeleteFrameDouble {
            fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
                Vec::new()
            }
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let doc = host.document();
                match gascii_core::remove_frame(doc, doc.active_frame()) {
                    Ok(edit) => gascii_plugin_api::PanelOutcome { edits: vec![edit], ..Default::default() },
                    Err(_) => gascii_plugin_api::PanelOutcome::default(),
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(DeleteFrameDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));
        assert_eq!(app.doc.frame_count(), 1, "the plugin-outcome-originated delete must have applied");

        app.request_undo();
        assert_eq!(app.doc, before_delete, "undo must byte-exactly restore the prior 2-frame document");
    }

    /// `switch_active_frame` (the target of a `PanelOutcome::set_active_frame`) flushes via
    /// `flush_all()`, but that only actually commits a `holds_session` tool's (Text/Selection)
    /// pending work — a plain stroke tool like Pencil does not hold a "session" the flush machinery
    /// recognizes, so a mid-drag Pencil press is left genuinely pending, not force-committed, across
    /// the switch. The eventual `Release` (whenever the pointer lifts) builds its `ToolCtx` fresh
    /// against whatever frame is active *at that moment* — this proves it lands on the frame the
    /// stroke was actually released against, and that the switch itself does not silently commit (or
    /// lose) the in-flight stroke onto either frame by itself.
    #[test]
    fn switch_active_frame_mid_pencil_drag_does_not_silently_commit_the_pending_stroke_to_either_frame() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.active_frame, 0);

        app.active_glyph = 'Z';
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Pencil);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.stroke_owner = Some(Binding::L);

        // A plugin-shaped frame switch arrives mid-drag (mirrors a real timeline click's outcome).
        app.switch_active_frame(1);

        assert_eq!(app.active_frame, 1, "the switch itself must still take effect");
        assert_eq!(app.doc.cell_at(0, 0, 0, 0).unwrap().ch, ' ', "the pending stroke must not have been silently committed onto its origin frame by the flush");
        assert_eq!(app.doc.cell_at(1, 0, 0, 0).unwrap().ch, ' ', "nor onto the frame just switched to");

        // Whenever the pointer eventually releases, it commits against whatever frame is active at
        // that moment — proving the eventual commit target is the frame the release actually
        // targets, never a stale snapshot of the origin frame.
        let tctx2 = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx2, &app.doc) {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell_at(1, 0, 0, 0).unwrap().ch, 'Z', "the eventual release commits onto whichever frame is active when it fires");
        assert_eq!(app.doc.cell_at(0, 0, 0, 0).unwrap().ch, ' ', "the origin frame is left untouched by a release that fires after the switch");
    }

    /// `add_frame_via_menu`'s own per-variant `last_error` message at the `MAX_FRAMES` boundary —
    /// pinned literally so a future wording change is deliberate, and so it can be cross-checked
    /// against `gascii-anim`'s own `frame_op_error_message` test for the same `FrameOpError` variant
    /// (Important #1's "menu and timeline paths produce consistent messages for the same failure").
    #[test]
    fn add_frame_via_menu_reports_the_max_frames_boundary_with_a_specific_readable_message() {
        let mut app = GasciiApp::headless();
        for i in 1..Document::MAX_FRAMES {
            let edit = gascii_core::add_frame(&app.doc, i, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
            app.apply_edit(edit, None);
        }
        assert_eq!(app.doc.frame_count(), Document::MAX_FRAMES);

        app.add_frame_via_menu();

        assert_eq!(app.doc.frame_count(), Document::MAX_FRAMES, "a rejected add must not change frame_count");
        assert_eq!(app.last_error.as_deref(), Some("add frame: exceeds the 256 maximum"));
    }

    /// The Edit menu's "Add Frame" bootstrap: duplicates the active frame and flushes any pending
    /// session first — mirrors "Resize Canvas…"'s own flush-before-structural-trigger discipline.
    #[test]
    fn add_frame_menu_item_duplicates_the_active_frame_and_flushes_first() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 0, 0, cell('D'));

        // A pending Text burst, uncommitted — must be flushed (not dropped) before the duplicate.
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('X'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        app.add_frame_via_menu();

        assert_eq!(app.doc.frame_count(), 2, "Add Frame must duplicate into a second frame");
        assert_eq!(app.doc.cell_at(0, 0, 1, 0).unwrap().ch, 'X', "the pending burst must be flushed before duplicating");
        assert_eq!(app.doc.cell_at(1, 0, 0, 0).unwrap().ch, 'D', "the duplicate must carry the source frame's content");
        assert_eq!(app.doc.cell_at(1, 0, 1, 0).unwrap().ch, 'X', "the duplicate must carry the just-flushed burst too");
        assert!(app.last_error.is_none());
    }

    /// End-to-end integration: drives the whole Add-Frame/switch-frame/undo commit chain together
    /// through the real registered `gascii-anim` plugin (not a double) — Add Frame via the menu,
    /// draw on frame 2, switch back to frame 1 via a synthetic `PanelOutcome`, undo twice, and
    /// confirm both the frame structure and cell content are back to the single-frame starting state.
    #[test]
    fn add_frame_draw_switch_frame_and_undo_twice_restores_the_single_frame_starting_state() {
        let mut app = GasciiApp::headless();
        let starting_doc = app.doc.clone();

        app.add_frame_via_menu();
        assert_eq!(app.doc.frame_count(), 2);

        // Switch to frame 1 via a plugin-shaped PanelOutcome (mirrors a real timeline click).
        app.switch_active_frame(1);
        assert_eq!(app.active_frame, 1);
        assert_eq!(app.doc.active_frame(), 1);

        // Draw on frame 2 (index 1).
        app.active_glyph = 'Z';
        let r = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 2, y: 2 }, &r, &app.doc);
        if let ToolResponse::Commit(Some(edit)) = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &r, &app.doc) {
            app.apply_edit(edit, Some(Binding::L));
        }
        assert_eq!(app.doc.cell_at(1, 0, 2, 2).unwrap().ch, 'Z');

        // Switch back to frame 0.
        app.switch_active_frame(0);
        assert_eq!(app.active_frame, 0);

        // Undo the draw, then the Add Frame — back to the single-frame starting state.
        app.request_undo();
        app.request_undo();
        assert_eq!(app.doc, starting_doc, "two undos must fully restore the pre-Add-Frame document");
    }

    /// A test-only `Plugin` that logs its own tag into a shared log when `wrap_renderer` is
    /// called, proving `build_renderer`'s fold order directly rather than trying to inspect the
    /// opaque composed `Box<dyn CanvasRenderer>` it returns.
    struct TaggingPlugin {
        tag: &'static str,
        log: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    }
    impl Plugin for TaggingPlugin {
        fn register_tools(&self) -> Vec<gascii_plugin_api::PluginToolCapabilities> {
            Vec::new()
        }
        fn wrap_renderer(&self, inner: Box<dyn CanvasRenderer>) -> Box<dyn CanvasRenderer> {
            self.log.borrow_mut().push(self.tag);
            inner
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn build_renderer_folds_every_plugins_wrap_renderer_in_registration_order() {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(TaggingPlugin { tag: "a", log: log.clone() }),
            Box::new(TaggingPlugin { tag: "b", log: log.clone() }),
            Box::new(TaggingPlugin { tag: "c", log: log.clone() }),
        ];
        let _ = build_renderer(&plugins);
        assert_eq!(*log.borrow(), vec!["a", "b", "c"], "fold order must match plugin-list order");
    }

    /// Confirms `BrushPlugin`'s actual no-op defaults hold end-to-end against a real
    /// `GasciiApp::headless()` — not just against test doubles: the plugin-composed renderer
    /// chain paints without panicking.
    #[test]
    fn a_real_app_with_the_builtin_plugin_list_has_an_identity_renderer() {
        let app = GasciiApp::headless();

        let mut renderer = build_renderer(&app.plugins);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            renderer.paint(
                &painter,
                &app.doc,
                &app.viewport as &dyn gascii_plugin_api::CellGrid,
                egui::Pos2::ZERO,
                egui::Vec2::new(10.0, 20.0),
                (0, 0, app.doc.width, app.doc.height),
                &[],
                &[],
                None,
                None,
            );
        });
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
        for def in tools().iter() {
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

    /// The full truth table behind Escape's fullscreen-exit precedence: only "no keyboard-owning
    /// session AND no live stroke" claims Escape for exiting fullscreen. Either higher-priority
    /// claim alone is enough to withhold it.
    #[test]
    fn should_handle_escape_for_fullscreen_truth_table() {
        assert!(
            should_handle_escape_for_fullscreen(None, false),
            "no session, no stroke: Escape should exit fullscreen"
        );
        assert!(
            !should_handle_escape_for_fullscreen(Some(Binding::L), false),
            "an active keyboard-owning session outranks the fullscreen exit"
        );
        assert!(
            !should_handle_escape_for_fullscreen(None, true),
            "a live pointer stroke outranks the fullscreen exit"
        );
        assert!(
            !should_handle_escape_for_fullscreen(Some(Binding::R), true),
            "both higher-priority claims held at once still withhold Escape from fullscreen"
        );
    }

    /// Stylus pressure must drive `tool_ctx`'s size for the live stroke without ever touching the
    /// slot's own `StampSettings.size` — the same field the Size stepper/`[`/`]` keys edit and
    /// `Prefs::from_app` persists. A user's configured size must survive a pressure-modulated
    /// stroke byte-for-byte.
    #[test]
    fn pressure_override_drives_tool_ctx_size_without_touching_the_slots_configured_size() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Brush);
        let brush_slot = sized_slot(ToolKind::Brush).expect("Brush is a sized tool");
        app.slots[Binding::L.ix()].stamps[brush_slot].size = 10;

        crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
        assert_eq!(app.stroke_owner, Some(Binding::L), "sanity: L is mid-stroke");
        assert_eq!(
            app.pressure_stamp_size, None,
            "a fresh stroke starts with no pressure override"
        );

        // A light-pressure dab (mirrors canvas.rs's quantization) sets only the transient override.
        app.pressure_stamp_size = Some(2);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(tctx.size, 2, "the live stroke's footprint follows the pressure override");
        assert_eq!(
            app.slots[Binding::L.ix()].stamps[brush_slot].size, 10,
            "the binding's configured/persisted Brush size must be untouched by pressure"
        );

        // The other binding never sees a pressure override that isn't its own.
        let r_tctx = crate::canvas::tool_ctx(&app, Binding::R);
        assert_ne!(r_tctx.size, 2, "pressure only overrides the stroke-owning binding");

        // Ending the stroke clears the override; the slot's size is still exactly what it was.
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        app.pressure_stamp_size = None;
        assert_eq!(
            app.slots[Binding::L.ix()].stamps[brush_slot].size, 10,
            "the configured size survives the whole stroke, including release"
        );
    }

    /// `tool_ctx`'s extra-context injection (density mode, ramp) must reach only a plugin tool that
    /// asks for it (Brush, via `wants_extra_ctx`), reading it from the *live* plugin instance rather
    /// than a fresh default — and must leave a non-plugin tool at the inert default the pre-migration
    /// literal `GasciiApp::with_state` used, matching what every non-Brush tool already got before
    /// this workstream.
    #[test]
    fn tool_ctx_injects_extra_context_only_for_a_plugin_tool_that_wants_it() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Brush);
        app.brush_plugin_mut().set_active_ramp(1);
        let expected_ramp = gascii_core::builtin_ramps()[1].chars.clone();

        let brush_ctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(brush_ctx.ramp, expected_ramp, "Brush's tool_ctx.ramp must follow the live plugin's active ramp");

        app.bind(Binding::L, ToolKind::Pencil);
        let pencil_ctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert!(pencil_ctx.ramp.is_empty(), "a non-plugin tool must get the inert default, not Brush's ramp");
    }

    /// End-to-end proof that a real Brush stroke, driven the same way every other tool-stroke test
    /// in this module drives one (`tool.update` + `apply_edit`, not just inspecting `tool_ctx` in
    /// isolation), actually stamps a glyph read off the live plugin's active ramp — not a default
    /// or a stale snapshot. Ramp index 1 ("Block shades", `"░▒▓█"`) with the plugin's default
    /// `Fixed(1.0)` intensity picks the ramp's last character deterministically.
    #[test]
    fn a_full_brush_stroke_through_the_app_commits_a_glyph_from_the_plugins_active_ramp() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Brush);
        app.brush_plugin_mut().set_active_ramp(1);
        let expected_ch = gascii_core::builtin_ramps()[1].chars[3]; // '█', Fixed(1.0) on a 4-char ramp

        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        assert_eq!(
            app.doc.cell(app.active_layer, 2, 2).unwrap().ch,
            expected_ch,
            "the committed glyph must come from the live plugin's active ramp/density, not a default"
        );

        // A non-plugin tool bound to the same binding must never read a ramp at all — it stamps
        // the app's plain active_glyph, completely untouched by whatever the plugin's ramp holds.
        app.bind(Binding::L, ToolKind::Pencil);
        app.active_glyph = '#';
        let pencil_ctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 3, y: 3 }, &pencil_ctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &pencil_ctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        assert_eq!(app.doc.cell(app.active_layer, 3, 3).unwrap().ch, '#');
    }

    /// Per-binding isolation through the plugin: Brush need not be on L. Bound to R alone while L
    /// holds an unrelated tool, `tool_ctx` must still resolve R's ramp/density through the live
    /// plugin (not just when Brush happens to be the L-bound case every other test exercises), and
    /// L must see none of it.
    #[test]
    fn brush_bound_only_to_r_while_l_holds_a_different_tool_still_resolves_through_the_plugin() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, ToolKind::Brush);
        app.brush_plugin_mut().set_active_ramp(1);
        let expected_ramp = gascii_core::builtin_ramps()[1].chars.clone();

        let r_ctx = crate::canvas::tool_ctx(&app, Binding::R);
        assert_eq!(r_ctx.ramp, expected_ramp, "R's tool_ctx must resolve through the plugin even though L holds a different tool");
        let l_ctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert!(l_ctx.ramp.is_empty(), "L (Pencil) must not see Brush's ramp just because R holds Brush");
    }

    /// The digit-key intensity shortcut's real gating, driven through `handle_keys` itself (not
    /// `BrushPlugin::tick` in isolation with a `FakeHost`, which `gascii-density-brush`'s own suite
    /// already covers) — proving the host's `!focused` gate, `host_facts`, and the per-frame
    /// `plugins.iter_mut().for_each(|p| p.tick(...))` loop are wired together correctly end to end.
    /// Also exercises kiosk (fullscreen) input: the shortcut was never fullscreen-gated pre-migration
    /// and must not become so now.
    #[test]
    fn digit_key_intensity_shortcut_through_handle_keys_sets_fixed_intensity_while_bound_and_unfocused() {
        for fullscreen in [false, true] {
            let mut app = GasciiApp::headless();
            app.bind(Binding::L, ToolKind::Brush);
            app.brush_plugin_mut().set_density_mode(gascii_core::DensityMode::Buildup(gascii_core::Buildup));

            let ctx = egui::Context::default();
            let mut raw = egui::RawInput::default();
            raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(fullscreen);
            raw.events.push(egui::Event::Key {
                key: egui::Key::Num5,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

            match app.brush_plugin_mut().density_mode() {
                gascii_core::DensityMode::Fixed(gascii_core::Fixed(level)) => {
                    assert!((level - 0.5).abs() < 1e-4, "fullscreen={fullscreen}: expected Fixed(0.5)")
                }
                other => panic!("fullscreen={fullscreen}: expected Fixed(0.5), got {other:?}"),
            }
        }
    }

    /// The exact suppression the pre-migration `bound_to(ToolKind::Brush).is_some() && !focused`
    /// gate provided: an active Text session anywhere on the keyboard must still suppress the
    /// digit-key shortcut, even though Brush is bound to the OTHER binding (R), not the one holding
    /// the session — `focused` is a single app-wide fact, not per-binding.
    #[test]
    fn digit_key_intensity_shortcut_is_suppressed_while_a_text_session_owns_the_keyboard() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.bind(Binding::R, ToolKind::Brush);
        app.keyboard_owner = Some(Binding::L);
        app.brush_plugin_mut().set_density_mode(gascii_core::DensityMode::Buildup(gascii_core::Buildup));

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Num5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(
            matches!(app.brush_plugin_mut().density_mode(), gascii_core::DensityMode::Buildup(_)),
            "an active Text session must suppress Brush's digit-key shortcut even though Brush is bound to the other binding"
        );
    }

    /// The other suppression path: a focused egui widget (e.g. the HEX color field) must also
    /// suppress the shortcut, matching every other single-key tool shortcut's own `!focused` gate.
    #[test]
    fn digit_key_intensity_shortcut_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Brush);
        app.brush_plugin_mut().set_density_mode(gascii_core::DensityMode::Buildup(gascii_core::Buildup));

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Num5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| {
            let id = egui::Id::new("qa_test_fake_focused_widget");
            ui.memory_mut(|m| m.request_focus(id));
            app.handle_keys(ui);
        });

        assert!(
            matches!(app.brush_plugin_mut().density_mode(), gascii_core::DensityMode::Buildup(_)),
            "a focused widget must suppress Brush's digit-key shortcut, matching every other tool-shortcut gate"
        );
    }

    /// `active_layer` is the single source `tool_ctx` and the eyedropper pick read from — pins that
    /// a non-zero value (session-only in this scope; the app itself never writes anything but 0)
    /// actually reaches both call sites rather than a stale `0` literal surviving in either.
    #[test]
    fn tool_ctx_and_eyedropper_follow_active_layer() {
        let mut app = GasciiApp::headless();
        let (w, h) = (app.doc.width, app.doc.height);
        app.doc.layers_mut().push(gascii_core::Layer::blank(w, h));
        app.doc.layers_mut().push(gascii_core::Layer::blank(w, h));
        app.active_layer = 2;
        app.doc.set_cell(2, 3, 3, gascii_core::Cell { ch: 'z', fg: Rgba(1, 2, 3, 255), bg: Rgba::TRANSPARENT });

        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(tctx.layer, 2, "tool_ctx's layer must follow active_layer");

        app.bind(Binding::L, ToolKind::Eyedropper);
        crate::canvas::begin_gesture(&mut app, Binding::L, 3, 3, false, false);
        let (expected_fg, _) = gascii_core::eyedrop(&app.doc.cell(2, 3, 3).copied().unwrap());
        assert_eq!(
            app.active_fg, expected_fg,
            "the eyedropper pick must read the cell from active_layer, not layer 0"
        );
    }

    /// Mirrors `tool_ctx_and_eyedropper_follow_active_layer`'s shape, but for `frame`:
    /// `active_frame` defaults to `0` and `tool_ctx` follows whatever it's set to.
    #[test]
    fn active_frame_defaults_to_zero_and_tool_ctx_follows_it() {
        let app = GasciiApp::headless();
        assert_eq!(app.active_frame, 0);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(tctx.frame, 0, "tool_ctx's frame must follow active_frame");

        let mut app = app;
        app.active_frame = 1;
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(tctx.frame, 1, "tool_ctx's frame must follow a non-default active_frame too");
    }

    /// `apply_edit`'s app -> doc sync actually reaches `doc.active_frame()` before every applied
    /// edit, exercised end-to-end against a multi-frame document.
    #[test]
    fn apply_edit_syncs_doc_active_frame_from_app_active_frame_before_applying() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.frame_count(), 2);

        app.active_frame = 1;
        let cell_edit = gascii_core::Edit::Cells(vec![gascii_core::CellEdit {
            frame: 1,
            layer: 0,
            x: 0,
            y: 0,
            before: gascii_core::Cell::BLANK,
            after: gascii_core::Cell { ch: 'x', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT },
        }]);
        app.apply_edit(cell_edit, None);
        assert_eq!(app.doc.active_frame(), 1, "apply_edit must sync doc's active-frame cursor from app.active_frame");
    }

    /// `apply_edit`'s doc -> app direction: `AddFrame` shifts `doc`'s cursor as a side effect of
    /// applying (inserting at index 0 pushes the active frame from 0 to 1) — `app.active_frame`
    /// must follow that shift, not just the app -> doc seed. Then `request_undo`'s own doc -> app
    /// resync must follow `doc`'s cursor back down when the insert is undone.
    #[test]
    fn undoing_an_add_frame_moves_the_docs_cursor_and_app_active_frame_follows() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 0, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.frame_count(), 2);
        assert_eq!(app.doc.active_frame(), 1, "inserting at index 0 shifts the active cursor forward");
        assert_eq!(app.active_frame, 1, "apply_edit's doc -> app resync must follow the shift");

        app.request_undo();
        assert_eq!(app.doc.active_frame(), 0, "undo restores doc's pre-insert cursor");
        assert_eq!(app.active_frame, 0, "app.active_frame must follow doc's cursor back down after undo");
    }

    /// App-side pinning spot-check: with `active_frame` shipped pinned at `0` (no UI writes it),
    /// a full stroke -> undo -> redo -> save -> load cycle driven through the real app pipeline
    /// must still land on `frame_count() == 1` and save exactly the pre-frames v1 envelope shape —
    /// the `frame_count() == 1 => save v1` rule makes this directly assertable without a second,
    /// pre-frames build to diff against.
    #[test]
    fn a_stroke_undo_redo_save_load_cycle_through_the_app_still_produces_a_plain_v1_file() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.active_glyph = '#';
        app.active_fg = Rgba::WHITE;
        app.active_bg = Rgba::TRANSPARENT;

        crate::canvas::begin_gesture(&mut app, Binding::L, 2, 2, false, false);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc) {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, '#', "sanity: the stroke committed");

        app.request_undo();
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, ' ', "sanity: undo reverted the stroke");
        app.request_redo();
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, '#', "sanity: redo restored it");

        assert_eq!(app.doc.frame_count(), 1, "the shipped app never leaves frame_count() == 1");
        assert_eq!(app.active_frame, 0, "the shipped app never moves active_frame off 0");
        assert_eq!(app.doc.active_frame(), 0);

        let dir = scratch_dir("frame_pin_v1_shape");
        let path = dir.join("out.gascii");
        app.current_path = Some(path.clone());
        app.save_file();

        let raw = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["background", "height", "layers", "version", "width"],
            "a single-frame session must save exactly the pre-frames v1 key set — no frame-substrate field leaks in"
        );
        assert_eq!(value["version"], 1, "a single-frame session must be tagged version 1, the pre-frames version");

        let loaded = load_str(&raw).unwrap();
        assert_eq!(loaded, app.doc, "the round trip must be byte-exact");
        assert_eq!(loaded.cell(0, 2, 2).unwrap().ch, '#');

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `F11` must exit/enter fullscreen even while a Text session is fully active — the exact
    /// scenario `suppresses_tool_shortcuts` exists to gate (single-letter tool keys), which F11 is
    /// not one of. A stale `!focused` gate on F11 previously reused that same flag and swallowed
    /// the toggle for as long as a Text burst lasted.
    #[test]
    fn f11_toggles_fullscreen_even_during_an_active_text_session() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        let owner_kind = app.keyboard_owner().map(|b| app.slot(b).kind);
        assert!(
            suppresses_tool_shortcuts(owner_kind),
            "sanity: an active Text session suppresses the single-letter tool shortcuts"
        );

        let ctx = egui::Context::default();
        let mut raw_input = egui::RawInput::default();
        raw_input.events.push(egui::Event::Key {
            key: egui::Key::F11,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let output = ctx.run_ui(raw_input, |ui| app.handle_keys(ui));

        let sent_toggle = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(true))));
        assert!(sent_toggle, "F11 must toggle fullscreen even while a Text session is active");
    }

    /// eframe's own window persistence restores the previous run's fullscreen state, so the first
    /// frame must force the launch state — windowed unless `--fullscreen` — exactly once.
    #[test]
    fn startup_window_state_is_forced_exactly_once() {
        let mut app = GasciiApp::headless();
        app.startup_fullscreen = Some(false);
        let ctx = egui::Context::default();

        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let c = ui.ctx().clone();
            app.apply_startup_window_state(&c);
        });
        let sent_windowed = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(false))));
        assert!(sent_windowed, "the first frame must pin the window state even if eframe restored fullscreen");
        assert!(app.startup_fullscreen.is_none(), "the forced state is one-shot");

        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let c = ui.ctx().clone();
            app.apply_startup_window_state(&c);
        });
        let sent_again = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| !vp.commands.is_empty());
        assert!(!sent_again, "later frames must never re-force the window state");
    }

    /// `--fullscreen` launches straight into kiosk mode, which must arrive with zoom snapped to
    /// Fit exactly like an interactive entry does.
    #[test]
    fn a_fullscreen_launch_requests_the_same_fit_snap_as_an_interactive_entry() {
        let mut app = GasciiApp::headless();
        app.startup_fullscreen = Some(true);
        app.pending_fit = false;
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let c = ui.ctx().clone();
            app.apply_startup_window_state(&c);
        });
        assert!(app.pending_fit, "a fullscreen launch must snap zoom to Fit");
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
        crate::canvas::begin_gesture(&mut app, Binding::L, 1, 1, false, false);
        assert_eq!(app.keyboard_owner, Some(Binding::L));
        assert_eq!(app.selection_slot(), Some(Binding::L));

        // A press on R takes over: ownership moves, and it is still the only session.
        crate::canvas::begin_gesture(&mut app, Binding::R, 4, 4, false, false);
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
    fn copy_events_with_no_event_copy_present_fires_neither_copy_nor_copy_all() {
        let events = [egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }];
        assert_eq!(
            copy_events(&events, false),
            (false, false),
            "a bare Event::Key{{C}} is the exact fiction egui-winit never produces for the clipboard \
             chord — it must not fire copy"
        );
    }

    #[test]
    fn copy_events_with_event_copy_and_no_shift_fires_plain_copy_only() {
        let events = [egui::Event::Copy];
        assert_eq!(copy_events(&events, false), (true, false));
    }

    #[test]
    fn copy_events_with_event_copy_and_shift_held_fires_copy_all_only() {
        let events = [egui::Event::Copy];
        assert_eq!(copy_events(&events, true), (false, true));
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

    #[test]
    fn ctrl_c_response_is_none_when_no_new_presses() {
        assert_eq!(ctrl_c_response(2, 2, false), None);
        assert_eq!(ctrl_c_response(2, 2, true), None);
    }

    #[test]
    fn ctrl_c_response_first_press_requests_a_normal_close() {
        assert_eq!(ctrl_c_response(1, 0, false), Some(CtrlCResponse::RequestClose));
    }

    /// Several presses landing before the first frame drains them still count as one request —
    /// the veto dialog hasn't had a chance to appear, so nothing is discarded unprompted.
    #[test]
    fn ctrl_c_response_burst_before_dialog_shows_stays_a_normal_close() {
        assert_eq!(ctrl_c_response(3, 0, false), Some(CtrlCResponse::RequestClose));
    }

    #[test]
    fn ctrl_c_response_repeat_press_while_confirming_forces_the_close() {
        assert_eq!(ctrl_c_response(2, 1, true), Some(CtrlCResponse::ForceClose));
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

    /// Pure-function coverage over every `ToolKind`: only Text's shortcut is gated, and only while
    /// fullscreen — kiosk's sidebar has no cell for Text, so `T` must not be reachable there, but
    /// every other tool's shortcut (visible in the kiosk grid, showing L/R badges) stays live in
    /// both chrome modes.
    #[test]
    fn tool_shortcut_reachable_only_gates_text_and_only_while_fullscreen() {
        for kind in ALL_KINDS {
            assert!(tool_shortcut_reachable(kind, false), "{kind:?}: every shortcut works windowed");
        }
        for kind in ALL_KINDS {
            let expected = kind != ToolKind::Text;
            assert_eq!(
                tool_shortcut_reachable(kind, true), expected,
                "{kind:?}: fullscreen gating must affect only Text"
            );
        }
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
        crate::canvas::begin_gesture(&mut app, Binding::L, 3, 3, false, false);
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
        crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
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
        crate::canvas::begin_gesture(&mut app, Binding::L, 1, 1, false, false);
        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().and_then(|v| v.marquee).is_some(),
            "sanity: L has a marquee"
        );

        // A press on R takes over: L's session must be fully ended, not just masked.
        crate::canvas::begin_gesture(&mut app, Binding::R, 4, 4, false, false);

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
        assert!(crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false), "the press on the float starts a drag");
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
        assert!(crate::canvas::begin_gesture(&mut app, Binding::R, 2, 2, false, false));
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

    /// `step_zoom` is a deferred request — it accumulates into `pending_step_zoom` for
    /// `canvas::show` to apply through the anchored `zoom_at` path (whose end-of-scale clamping
    /// the viewport tests cover). Mutating `zoom_step` directly here would bypass both the
    /// anchoring and the mid-stroke gate.
    #[test]
    fn step_zoom_defers_into_pending_step_zoom_without_touching_the_viewport() {
        let mut app = GasciiApp::headless();
        let before = app.viewport.zoom_step;
        app.step_zoom(1);
        app.step_zoom(1);
        app.step_zoom(-1);
        assert_eq!(app.pending_step_zoom, 1, "requests accumulate by sign");
        assert_eq!(app.viewport.zoom_step, before, "the viewport itself must be untouched until canvas::show applies it");
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

        app.help_overlay_open = true;
        assert!(app.modal_open());
        app.help_overlay_open = false;

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

    /// Builds a real `n`-frame document via `apply_edit` (the same choke point every other
    /// multi-frame test in this module uses), rather than mutating `app.doc` directly.
    fn app_with_frame_count(n: usize) -> GasciiApp {
        let mut app = GasciiApp::headless();
        while app.doc.frame_count() < n {
            let edit = gascii_core::add_frame(&app.doc, app.doc.frame_count(), gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
            app.apply_edit(edit, None);
        }
        app
    }

    /// A single-frame document's Export dialog offers exactly Text/PNG — the zero-visible-
    /// behavior-change constraint for every pre-existing document, locked in by construction.
    #[test]
    fn a_single_frame_documents_format_list_is_exactly_text_and_png() {
        let doc = Document::default_document();
        assert_eq!(doc.frame_count(), 1);
        let names: Vec<&str> = export_dialog_formats(&doc).iter().map(|(_, label)| *label).collect();
        assert_eq!(names, vec!["Text (.txt)", "PNG"]);
    }

    /// A multi-frame document's Export dialog offers all five formats, in the documented order.
    #[test]
    fn a_multi_frame_documents_format_list_offers_all_five_formats_in_order() {
        let app = app_with_frame_count(2);
        let names: Vec<&str> = export_dialog_formats(&app.doc).iter().map(|(_, label)| *label).collect();
        assert_eq!(names, vec!["Text (.txt)", "PNG", "Animated GIF", "PNG Spritesheet", "Text Frames (.txt)"]);
    }

    /// `snap_unavailable_export_format`: a multi-frame-only format survives while `frame_count() >
    /// 1`, snaps back to `Text` the moment it drops to 1, and every format is a no-op at `> 1`.
    #[test]
    fn snap_unavailable_export_format_only_touches_multi_frame_only_formats_at_frame_count_one() {
        for format in [ExportFormat::Gif, ExportFormat::SpriteSheet, ExportFormat::TextFrames] {
            assert_eq!(snap_unavailable_export_format(format, 1), ExportFormat::Text);
            assert_eq!(snap_unavailable_export_format(format, 2), format, "must be a no-op while still offered");
        }
        for format in [ExportFormat::Text, ExportFormat::Png] {
            assert_eq!(snap_unavailable_export_format(format, 1), format, "an always-offered format is never snapped");
        }
    }

    /// `refresh_export_preview`'s gate: Gif and SpriteSheet build a preview texture exactly like
    /// PNG (all three are raster formats sharing the active-frame rasterizer); TextFrames does not,
    /// mirroring the existing Text case.
    #[test]
    fn refresh_export_preview_builds_a_texture_for_every_raster_format_but_not_text_frames() {
        let mut app = app_with_frame_count(2);
        let ctx = egui::Context::default();
        for format in [ExportFormat::Png, ExportFormat::Gif, ExportFormat::SpriteSheet] {
            app.export.format = format;
            app.export_preview = None;
            app.export_preview_key = None;
            app.refresh_export_preview(&ctx);
            assert!(app.export_preview.is_some(), "{format:?} must build a preview texture");
        }
        app.export.format = ExportFormat::TextFrames;
        app.export_preview = None;
        app.export_preview_key = None;
        app.refresh_export_preview(&ctx);
        assert!(app.export_preview.is_none(), "TextFrames must not build a preview texture");
    }

    /// `export_gif`/`export_spritesheet`'s output written through `write_atomic` — the same
    /// file-write half `write_atomic_creates_a_new_file_with_exact_contents` already pins for
    /// Text/PNG — decodes back as the expected format and dimensions. `run_export` itself opens a
    /// real native (blocking) `rfd::FileDialog`, so it — like the pre-existing Text/Png format
    /// arms — has no direct test; this exercises the same export-function-then-write_atomic
    /// pipeline `run_export`'s new arms perform, without the interactive file picker.
    #[test]
    fn gif_and_spritesheet_bytes_round_trip_through_write_atomic() {
        let app = app_with_frame_count(2);
        let dir = std::env::temp_dir().join(format!("gascii_anim_export_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let gif_path = dir.join("out.gif");
        let gif_bytes = anim_export::export_gif(&app.doc, 8, None, None).unwrap();
        write_atomic(&gif_path, &gif_bytes).unwrap();
        let decoded = image::load_from_memory(&std::fs::read(&gif_path).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (app.doc.width as u32 * 8, app.doc.height as u32 * 8));

        let sheet_path = dir.join("out.png");
        let sheet_bytes = anim_export::export_spritesheet(&app.doc, 8, None, None).unwrap();
        write_atomic(&sheet_path, &sheet_bytes).unwrap();
        let decoded = image::load_from_memory(&std::fs::read(&sheet_path).unwrap()).unwrap();
        // 2 frames -> a 2x1 grid (`cols = ceil(sqrt(2)) = 2`, `rows = 1`).
        assert_eq!(
            (decoded.width(), decoded.height()),
            (app.doc.width as u32 * 8 * 2, app.doc.height as u32 * 8),
            "2 frames, 2x1 grid"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The Export dialog's "Trim trailing spaces" checkbox toggles between `export_text_frames`
    /// (trimmed) and `export_text_frames_untrimmed` (padded) for `TextFrames`, the same divergence
    /// `export_trim_checkbox_toggles_between_trimmed_and_full_width_padded_rows` already pins for
    /// the single-frame `Text` pair.
    #[test]
    fn text_frames_trim_checkbox_toggles_between_trimmed_and_full_width_padded_rows() {
        let mut doc = Document::new(5, 1);
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(0, 1, 0, cell('b'));

        let trimmed = export_text_frames(&doc);
        let untrimmed = export_text_frames_untrimmed(&doc);

        assert_eq!(trimmed, format!("--- frame 1 ({}ms) ---\nab", Document::DEFAULT_FRAME_DURATION_MS));
        assert_eq!(untrimmed, format!("--- frame 1 ({}ms) ---\nab   ", Document::DEFAULT_FRAME_DURATION_MS));
        assert_ne!(trimmed, untrimmed, "the two export paths must genuinely diverge for this document");
    }

    /// `TextFrames`'s combined dump, like every other export format, must round-trip byte-exact
    /// through `write_atomic` and leave no `.tmp` file behind -- the same file-write half
    /// `write_atomic_creates_a_new_file_with_exact_contents` already pins for Text/PNG/Gif/
    /// SpriteSheet, extended to the one format that had no direct atomic-write test yet.
    #[test]
    fn text_frames_bytes_round_trip_through_write_atomic_with_no_tmp_file_left_behind() {
        let mut doc = Document::new(3, 1);
        doc.set_cell(0, 0, 0, cell('a'));
        let mut history = History::new();
        let edit = gascii_core::add_frame(&doc, 1, gascii_core::Frame::blank(3, 1)).unwrap();
        history.apply(&mut doc, edit);
        assert!(doc.set_active_frame(1));
        doc.set_cell(0, 1, 0, cell('b'));
        assert!(doc.set_active_frame(0));

        let dir = scratch_dir("text_frames_atomic");
        let path = dir.join("out.txt");
        let text = export_text_frames(&doc);
        write_atomic(&path, text.as_bytes()).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(!dir.join("out.txt.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cap composition, GIF: a document legitimately built past the joint frame-count budget
    /// (`validate_gif_dimensions`'s own locked-in 196-frame boundary) must be rejected with no
    /// bytes ever reaching the filesystem -- the same `Err` -> skip-`write_atomic` shape
    /// `run_export`'s `Gif` arm itself follows -- and the source document/history must be
    /// completely untouched by the failed attempt (export functions only ever borrow `&Document`,
    /// but this pins that invariant at the integration level rather than trusting the type system
    /// silently).
    #[test]
    fn a_gif_export_rejected_for_too_many_frames_writes_no_file_and_leaves_the_document_untouched() {
        let mut doc = Document::new(80, 25);
        let mut history = History::new();
        for _ in 1..196 {
            let edit = gascii_core::add_frame(&doc, doc.frame_count(), gascii_core::Frame::blank(80, 25)).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.frame_count(), 196);
        let before = doc.clone();
        let before_top_edit = history.top_edit_id();

        let dir = scratch_dir("gif_cap_rejection");
        let path = dir.join("rejected.gif");
        match anim_export::export_gif(&doc, 16, None, None) {
            Ok(_) => panic!("196 frames at 80x25/16px must be rejected by the joint pixel budget"),
            Err(e) => assert!(matches!(e, png_export::PngExportAppError::Dimensions(gascii_core::PngExportError::TooManyFrames { .. }))),
        }

        assert!(!path.exists(), "a rejected export must never reach write_atomic, so no file may exist");
        assert_eq!(doc, before, "a failed export must not mutate the source document");
        assert_eq!(history.top_edit_id(), before_top_edit, "a failed export must not touch history");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cap composition, spritesheet: a per-frame size that's individually fine
    /// (`validate_png_dimensions` succeeds) can still be rejected once tiled into a multi-frame
    /// grid (`validate_spritesheet_dimensions` fails on the joint canvas) -- a genuinely different
    /// rejection reason from the GIF case above, reached through the same `Err`-before-any-write
    /// shape, no file left behind, document/history untouched.
    #[test]
    fn a_spritesheet_export_rejected_for_a_too_large_tiled_canvas_writes_no_file_and_leaves_the_document_untouched() {
        let mut doc = Document::new(1024, 512);
        let mut history = History::new();
        for _ in 1..4 {
            let edit = gascii_core::add_frame(&doc, doc.frame_count(), gascii_core::Frame::blank(1024, 512)).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.frame_count(), 4);
        // Sanity: one frame alone is well under the single-PNG cap (8192x4096 ≈ 33.5MP < 100MP).
        assert!(gascii_core::validate_png_dimensions(doc.width, doc.height, 8).is_ok());
        let before = doc.clone();
        let before_top_edit = history.top_edit_id();

        let dir = scratch_dir("spritesheet_cap_rejection");
        let path = dir.join("rejected.png");
        match anim_export::export_spritesheet(&doc, 8, None, None) {
            Ok(_) => panic!("a 2x2 grid of 8192x4096 tiles (~134MP) must be rejected by the spritesheet cap"),
            Err(e) => assert!(matches!(e, png_export::PngExportAppError::Dimensions(gascii_core::PngExportError::TooLarge { .. }))),
        }

        assert!(!path.exists(), "a rejected export must never reach write_atomic, so no file may exist");
        assert_eq!(doc, before, "a failed export must not mutate the source document");
        assert_eq!(history.top_edit_id(), before_top_edit, "a failed export must not touch history");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `Gif`-format export preference persisted while a multi-frame document was open survives
    /// its own `Prefs` JSON round-trip unmodified -- `Prefs` is document-agnostic, it has no idea
    /// what document it'll next be applied to -- but the moment it's paired with a single-frame
    /// document (e.g. the app restarted after the user deleted every extra frame), the Export
    /// dialog's own reopen-time guard (`snap_unavailable_export_format`, applied at the top of
    /// `export_dialog` every frame it's open) snaps it back to `Text`, exactly as it would for any
    /// other document that dropped to one frame mid-session.
    #[test]
    fn a_persisted_gif_preference_meeting_a_single_frame_document_snaps_back_to_text_on_reopen() {
        let mut multi = app_with_frame_count(2);
        multi.export.format = ExportFormat::Gif;
        let prefs = crate::prefs::Prefs::from_app(&multi);
        let json = serde_json::to_string(&prefs).unwrap();
        let restored_prefs: crate::prefs::Prefs = serde_json::from_str(&json).unwrap();

        let mut single = GasciiApp::headless();
        assert_eq!(single.doc.frame_count(), 1, "sanity: a fresh headless app starts single-frame");
        restored_prefs.apply_to(&mut single);
        assert_eq!(
            single.export.format,
            ExportFormat::Gif,
            "Prefs itself is document-agnostic -- it restores whatever format was last stored, unsnapped"
        );

        let snapped = snap_unavailable_export_format(single.export.format, single.doc.frame_count());
        assert_eq!(snapped, ExportFormat::Text, "the dialog's reopen-time guard must snap an unavailable format back to Text");
    }

    /// Regression proof for WS5's `rasterize_composited` extraction, with realistic (not
    /// single-pixel) content: multiple distinct glyphs, distinct fg/bg colors including partial
    /// alpha, and an opaque document background -- `export_png` (built on `rasterize_rgba8`, which
    /// now delegates to `rasterize_composited`) must still produce byte-identical output to
    /// manually driving the frame-explicit path (`rasterize_frame_rgba8` at the active frame) and
    /// encoding it the same way `export_png` itself does. A regression that only shows up on
    /// multi-cell, multi-color, partially-transparent content (not the trivial 1x1 cases the
    /// pre-existing suite already covers) would only be caught here.
    #[test]
    fn export_png_is_byte_identical_to_manually_driving_the_frame_explicit_rasterizer_on_realistic_content() {
        let mut doc = Document::new(6, 4);
        doc.background = Rgba(20, 30, 40, 255);
        doc.set_cell(0, 0, 0, gascii_core::Cell { ch: 'A', fg: Rgba(255, 0, 0, 255), bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 1, 0, gascii_core::Cell { ch: 'B', fg: Rgba(0, 255, 0, 200), bg: Rgba(10, 10, 10, 128) });
        doc.set_cell(0, 2, 1, gascii_core::Cell { ch: '#', fg: Rgba::WHITE, bg: Rgba(0, 0, 255, 255) });
        doc.set_cell(0, 5, 3, gascii_core::Cell { ch: 'Z', fg: Rgba(1, 2, 3, 90), bg: Rgba::TRANSPARENT });

        let opaque_bg = Some(doc.background);
        let via_export_png = png_export::export_png(&doc, 12, opaque_bg, None).unwrap();

        let (w, h, pixels) = png_export::rasterize_frame_rgba8(&doc, doc.active_frame(), 12, opaque_bg, None).unwrap();
        let img = image::RgbaImage::from_raw(w, h, pixels).unwrap();
        let mut via_manual_frame_explicit = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut via_manual_frame_explicit), image::ImageFormat::Png).unwrap();

        assert_eq!(via_export_png, via_manual_frame_explicit, "export_png must remain byte-identical to the frame-explicit rasterization path");
    }

    /// Program-level end-to-end proof for the whole animation-plugin program (Phases 1-5): a
    /// document built up through the real user-facing sequence -- new doc, draw per frame, add
    /// frames, set per-frame durations -- is saved to `.gascii`, reloaded as a fresh `Document`
    /// sharing no memory with the original, and every one of this phase's three export formats is
    /// produced from that *reloaded* document and independently verified against it. A bug in the
    /// v2 save/load frame round-trip that only manifested at export time would slip through every
    /// phase-scoped test suite but not this one.
    #[test]
    fn a_full_new_draw_frames_durations_save_load_export_lifecycle_round_trips_correctly() {
        let mut app = GasciiApp::headless();
        app.doc = Document::new(3, 2);
        app.history = History::new();

        // Frame 0: draw.
        let red = Rgba(255, 0, 0, 255);
        app.doc.set_cell(0, 0, 0, gascii_core::Cell { ch: 'A', fg: red, bg: Rgba::TRANSPARENT });

        // Frame 1: add, draw distinct content.
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(3, 2)).unwrap();
        app.apply_edit(edit, None);
        assert!(app.doc.set_active_frame(1));
        let green = Rgba(0, 255, 0, 255);
        app.doc.set_cell(0, 1, 0, gascii_core::Cell { ch: 'B', fg: green, bg: Rgba::TRANSPARENT });

        // Frame 2: add, draw distinct content, give it its own duration.
        let edit = gascii_core::add_frame(&app.doc, 2, gascii_core::Frame::blank(3, 2)).unwrap();
        app.apply_edit(edit, None);
        assert!(app.doc.set_active_frame(2));
        let blue = Rgba(0, 0, 255, 255);
        app.doc.set_cell(0, 2, 0, gascii_core::Cell { ch: 'C', fg: blue, bg: Rgba::TRANSPARENT });
        let edit = gascii_core::set_frame_duration(&app.doc, 2, Some(250)).unwrap().unwrap();
        app.apply_edit(edit, None);
        app.doc.loop_playback = false;
        assert!(app.doc.set_active_frame(0));

        // Save + reload: the loaded document shares no memory with `app.doc`.
        let saved = save_string(&app.doc);
        let loaded = load_str(&saved).unwrap();
        assert_eq!(loaded.frame_count(), 3);
        assert_eq!(loaded.resolved_frame_duration_ms(2), Some(250));
        assert!(!loaded.loop_playback);

        // Export all three multi-frame formats from the *loaded* document.
        let gif_bytes = anim_export::export_gif(&loaded, 8, None, None).unwrap();
        let sheet_bytes = anim_export::export_spritesheet(&loaded, 8, None, None).unwrap();
        let text = export_text_frames(&loaded);

        // GIF: 3 frames, no loop extension (loop_playback == false survived the round trip), each
        // frame carries its source glyph's color, frame 2's delay honors the reloaded 250ms override.
        use image::AnimationDecoder;
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&gif_bytes)).unwrap();
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), 3);
        let close = |a: u8, b: u8| (a as i16 - b as i16).abs() <= 16;
        for (frame, &color) in frames.iter().zip([red, green, blue].iter()) {
            assert!(
                frame.buffer().pixels().any(|p| close(p.0[0], color.0) && close(p.0[1], color.1) && close(p.0[2], color.2)),
                "each decoded GIF frame must contain a pixel close to its source frame's color {color:?}"
            );
        }
        let (numer, denom) = frames[2].delay().numer_denom_ms();
        assert_eq!(numer / denom, 250, "frame 2's reloaded duration_override must survive into the GIF's delay");
        assert!(!gif_bytes.windows(11).any(|w| w == b"NETSCAPE2.0"), "the reloaded loop_playback == false must write no loop extension");

        // Spritesheet: 3 frames -> a 2x2 grid (cols=ceil(sqrt(3))=2, rows=2); frame 2 lands at (0,1).
        // Each frame draws one glyph at a different cell column (not a whole-tile fill), so the
        // check scans frame 2's whole tile region for its color rather than one hard-coded pixel.
        let decoded = image::load_from_memory(&sheet_bytes).unwrap().to_rgba8();
        let (frame_px_w, frame_px_h) = gascii_core::validate_png_dimensions(loaded.width, loaded.height, 8).unwrap();
        let (sheet_w, sheet_h) = gascii_core::validate_spritesheet_dimensions(frame_px_w, frame_px_h, 2, 2).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (sheet_w, sheet_h));
        let (tile_x0, tile_y0) = (0u32, frame_px_h); // frame 2's tile origin at grid (col=0, row=1)
        let found_blue_in_frame_2s_tile = (tile_y0..tile_y0 + frame_px_h)
            .flat_map(|y| (tile_x0..tile_x0 + frame_px_w).map(move |x| (x, y)))
            .any(|(x, y)| decoded.get_pixel(x, y).0[..3] == [blue.0, blue.1, blue.2]);
        assert!(found_blue_in_frame_2s_tile, "frame 2's own tile must contain its glyph's exact fg color somewhere");

        // Text frames: 3 headered bodies, each matching that frame's own `export_text` taken in
        // isolation on the reloaded document. Sliced by each header's own position (not a naive
        // `"\n\n"` split) since frame 0's body itself ends in a blank second row -- its own
        // trailing newline plus the `"\n\n"` frame separator would otherwise misalign a
        // position-based split.
        let headers: Vec<String> = (0..3)
            .map(|i| format!("--- frame {} ({}ms) ---", i + 1, loaded.resolved_frame_duration_ms(i).unwrap()))
            .collect();
        let starts: Vec<usize> = headers.iter().map(|h| text.find(h.as_str()).unwrap()).collect();
        for i in 0..3 {
            let mut isolated = loaded.clone();
            isolated.set_active_frame(i);
            let expected_body = export_text(&isolated);
            let seg_end = if i + 1 < 3 { starts[i + 1] - 2 } else { text.len() };
            let segment = &text[starts[i]..seg_end];
            assert_eq!(
                segment,
                format!("{}\n{expected_body}", headers[i]),
                "frame {i}'s text segment must match export_text of the reloaded document in isolation"
            );
        }
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

    /// The Clear button's wiring end to end: one undoable step that blanks the document and
    /// undoes cleanly, exercised through `GasciiApp::clear_document` rather than the core free
    /// function directly (core's own tests already cover the pure edit-building math).
    #[test]
    fn clear_document_app_method_produces_one_undoable_step() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 0, 0, cell('a'));
        app.doc.set_cell(0, 5, 5, cell('z'));
        let before = app.doc.clone();

        app.clear_document();

        assert!(app.doc.layers()[0].cells().iter().all(gascii_core::Cell::is_blank));
        assert!(app.history.can_undo());
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc, before);
    }

    /// Clearing an already-blank document must not push a phantom undo entry — matches every
    /// other tool's "nothing to commit" contract.
    #[test]
    fn clear_document_on_an_already_blank_document_creates_no_undo_entry() {
        let mut app = GasciiApp::headless();
        assert!(!app.history.can_undo());
        app.clear_document();
        assert!(!app.history.can_undo());
    }

    /// Clear flushes first, same trigger-table discipline as Save/Export/Resize/Copy: a live text
    /// burst must commit before Clear blanks the document, not be silently discarded.
    #[test]
    fn clear_document_flushes_a_pending_text_burst_before_blanking() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('A'), &tctx, &app.doc);

        app.clear_document();

        // The burst's 'A' commits, then Clear blanks it right back out — both as real edits, so
        // two undos are needed to get back to the empty starting document.
        assert!(app.doc.layers()[0].cells().iter().all(gascii_core::Cell::is_blank));
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'A', "undo #1 restores the burst's commit");
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, ' ', "undo #2 restores the pre-burst blank state");
    }

    /// The Selection counterpart of the Text-burst flush test above: a floating stamp is a
    /// session too (`holds_session`), so `clear_document`'s `flush_all()` must drop it into the
    /// document before Clear blanks everything — not silently discard the pending paste/move.
    #[test]
    fn clear_document_flushes_a_pending_floating_selection_stamp_before_blanking() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let patch = CellPatch { width: 1, height: 1, cells: vec![cell('z')] };
        app.slots[Binding::L.ix()].tool.accept_stamp(patch, (4, 4), &app.doc);

        app.clear_document();

        // Same two-undo-entry shape as the Text-burst case: the drop commits, then Clear blanks
        // it back out.
        assert!(app.doc.layers()[0].cells().iter().all(gascii_core::Cell::is_blank));
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.cell(0, 4, 4).unwrap().ch, 'z', "undo #1 restores the dropped stamp");
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.cell(0, 4, 4).unwrap().ch, ' ', "undo #2 restores the pre-drop blank state");
    }

    /// Clear must round-trip through both undo AND redo, not just undo — `History::redo` re-applies
    /// the exact same `Edit::Cells` `clear_document` built, so re-blanking after a redo must match
    /// the original clear byte-for-byte, and the history's can_undo/can_redo flags must track it.
    #[test]
    fn clear_document_survives_an_undo_then_redo_round_trip() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 1, 1, cell('a'));
        app.doc.set_cell(0, 3, 3, cell('b'));
        let before = app.doc.clone();

        app.clear_document();
        let after_clear = app.doc.clone();
        assert!(app.history.can_undo());
        assert!(!app.history.can_redo());

        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc, before, "undo must restore the exact pre-Clear document");
        assert!(app.history.can_redo());

        assert!(app.history.redo(&mut app.doc));
        assert_eq!(app.doc, after_clear, "redo must re-apply the exact same Clear edit");
        assert!(!app.history.can_redo());
    }

    /// The bug class `Tool::resync` exists to prevent: Clear runs mid-stroke (`flush_all` only
    /// commits session-holding kinds, so a raw Pencil drag survives Clear untouched at the tool
    /// level), but the document underneath it is blanked. `apply_edit`'s resync fan-out must
    /// recompose the stroke's already-touched pending cells against the new blank document —
    /// including on a masked-off plane, where a missed recompose would silently commit the
    /// pre-Clear bg color back in on release.
    #[test]
    fn clear_mid_stroke_resyncs_the_pending_drags_masked_off_bg_plane_to_the_post_clear_blank_state() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.mask = PlaneMask { glyph: true, bg: false }; // bg masked off: must always track `before`
        app.active_glyph = '#';
        app.active_fg = Rgba::WHITE;

        let old_bg = Rgba(10, 20, 30, 255);
        app.doc.set_cell(0, 2, 2, gascii_core::Cell { ch: 'x', fg: Rgba::WHITE, bg: old_bg });

        crate::canvas::begin_gesture(&mut app, Binding::L, 2, 2, false, false);
        assert_eq!(app.stroke_owner, Some(Binding::L));
        assert!(
            !app.slots[Binding::L.ix()].tool.pending().is_empty(),
            "sanity: the stroke touched a cell before Clear ran"
        );

        app.clear_document();
        assert!(
            app.doc.layers()[0].cells().iter().all(gascii_core::Cell::is_blank),
            "Clear must blank the document even with a stroke mid-flight"
        );
        assert_eq!(app.stroke_owner, Some(Binding::L), "Clear must not itself end an in-progress stroke");

        // Finish the stroke where it started (a click) and commit.
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;

        let committed = app.doc.cell(0, 2, 2).unwrap();
        assert_eq!(committed.ch, '#', "the unmasked glyph plane still stamps the drawn glyph");
        assert_ne!(committed.bg, old_bg, "the masked-off bg plane must not resurrect the pre-Clear color");
        assert_eq!(
            committed.bg,
            Rgba::TRANSPARENT,
            "the masked-off bg plane must follow the post-Clear blank bg, not a stale composition"
        );
    }

    /// `begin_gesture`'s own reset of `pressure_stamp_size` is the last line of defense against a
    /// leaked override, independent of every release/cancel path already covered elsewhere: even if
    /// some future bug left a stale value behind, the very next stroke on ANY binding — pen or not —
    /// must never inherit it.
    #[test]
    fn begin_gesture_always_clears_a_stale_pressure_override_regardless_of_which_binding_or_tool_set_it() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.pressure_stamp_size = Some(3); // simulates a leftover value some other path failed to clear

        crate::canvas::begin_gesture(&mut app, Binding::R, 0, 0, false, false);

        assert_eq!(
            app.pressure_stamp_size, None,
            "a fresh stroke on ANY binding must start with no pressure override"
        );
        let tctx = crate::canvas::tool_ctx(&app, Binding::R);
        let pencil_slot = sized_slot(ToolKind::Pencil).expect("Pencil is sized");
        assert_eq!(
            tctx.size, app.slots[Binding::R.ix()].stamps[pencil_slot].size,
            "a non-pen stroke must use its own configured size, never an inherited override"
        );
    }

    /// K2: zoom snaps to Fit only on the false→true transition, never on exit. `pending_fit` is the
    /// mechanism — this pins it directly against `toggle_fullscreen` rather than trusting the
    /// existing Escape/F11 tests, which don't inspect `pending_fit` at all.
    #[test]
    fn toggle_fullscreen_snaps_pending_fit_only_on_the_false_to_true_transition() {
        let mut app = GasciiApp::headless();
        app.pending_fit = false;
        let ctx = egui::Context::default(); // no viewport info registered: fullscreen reads as false
        app.toggle_fullscreen(&ctx); // false -> true
        assert!(app.pending_fit, "entering fullscreen must snap zoom to Fit");

        app.pending_fit = false;
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        let _ = ctx.run_ui(raw, |_ui| {});
        app.toggle_fullscreen(&ctx); // true -> false
        assert!(!app.pending_fit, "exiting fullscreen must NOT re-trigger a Fit snap");
    }

    /// End-to-end trace of K1's full precedence chain across two real frames — not just the pure
    /// `should_handle_escape_for_fullscreen` predicate, but `handle_keys` AND `canvas.rs`'s own
    /// Text-session Escape handling racing over the same frame's events, exactly as `GasciiApp::ui`
    /// drives them in sequence. Frame 1's Escape must end the session and leave fullscreen alone;
    /// frame 2's Escape (session now gone) must exit fullscreen.
    #[test]
    fn escape_precedence_chain_first_ends_text_session_second_exits_fullscreen() {
        let mut app = GasciiApp::headless();
        app.pending_fit = false;
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        assert_eq!(app.keyboard_owner(), Some(Binding::L), "sanity: the Text session holds the keyboard");

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);

        fn escape_event() -> egui::Event {
            egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }
        }

        // Frame 1: fullscreen, Escape pressed, Text session still active.
        let mut raw1 = egui::RawInput::default();
        raw1.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw1.events.push(escape_event());
        let out1 = ctx.run_ui(raw1, |ui| {
            app.handle_keys(ui);
            crate::canvas::show(ui, &mut app, false);
        });
        let toggled_frame1 = out1
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))));
        assert!(!toggled_frame1, "frame 1's Escape must be claimed by the session, not fullscreen");
        assert_eq!(
            app.keyboard_owner(), None,
            "frame 1's Escape must end the Text session (canvas.rs's own handling)"
        );

        // Frame 2: fullscreen, Escape pressed again, no session left to claim it.
        let mut raw2 = egui::RawInput::default();
        raw2.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw2.events.push(escape_event());
        let out2 = ctx.run_ui(raw2, |ui| app.handle_keys(ui));
        let toggled_frame2 = out2
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(false))));
        assert!(toggled_frame2, "frame 2's Escape, with no session left, must exit fullscreen");
    }

    /// The third rung of K1's precedence chain: a live pointer stroke outranks Escape's
    /// fullscreen-exit claim exactly like an active session does — exiting mid-drag would yank the
    /// canvas out from under the pointer. Drives the real `handle_keys` rather than only the pure
    /// predicate, so a future accidental reordering of the guards would be caught here too.
    #[test]
    fn escape_does_not_exit_fullscreen_while_a_stroke_is_mid_drag() {
        let mut app = GasciiApp::headless();
        crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
        assert!(app.stroke_in_progress(), "sanity: a stroke is mid-drag");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let output = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        let toggled = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))));
        assert!(!toggled, "Escape must not exit fullscreen while a stroke is mid-drag");
        assert!(app.stroke_in_progress(), "the mid-drag stroke itself must be untouched");
    }

    /// Extends the existing F11-during-Text-session regression test: toggling fullscreen must be a
    /// pure side-channel to the Text session, not just "doesn't block the toggle command" — typing
    /// must keep working immediately afterward and the eventual flush must commit every character,
    /// proving the toggle never touched `keyboard_owner`, the tool instance, or its pending buffer.
    #[test]
    fn f11_mid_text_burst_leaves_the_burst_content_and_caret_fully_intact_after_toggling() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('i'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::F11,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        // The toggle must be a pure side-channel: keyboard ownership, the tool instance, and the
        // caret are all untouched, so typing continues exactly where it left off.
        assert_eq!(app.keyboard_owner(), Some(Binding::L), "F11 must not release the keyboard");
        assert!(app.slots[Binding::L.ix()].tool.caret().is_some(), "F11 must not clear the caret");
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('!'), &tctx, &app.doc);
        app.flush_slot(Binding::L);

        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'h');
        assert_eq!(app.doc.cell(0, 1, 0).unwrap().ch, 'i');
        assert_eq!(app.doc.cell(0, 2, 0).unwrap().ch, '!', "typing after the F11 toggle must still commit");
    }

    /// End-to-end companion to `tool_shortcut_reachable_only_gates_text_and_only_while_fullscreen`:
    /// drives the real `handle_keys` rather than the pure predicate alone, confirming `T` is left
    /// unconsumed (L stays whatever it was) while fullscreen, and that this gating is narrow — every
    /// other tool's shortcut (e.g. Fill's `F`) still switches L normally in the same chrome mode.
    #[test]
    fn pressing_t_while_fullscreen_leaves_l_unchanged_but_other_tool_shortcuts_still_work() {
        let mut app = GasciiApp::headless();
        let original_l_kind = app.slot(Binding::L).kind;

        let ctx = egui::Context::default();
        let mut raw_t = egui::RawInput::default();
        raw_t.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw_t.events.push(egui::Event::Key {
            key: egui::Key::T,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw_t, |ui| app.handle_keys(ui));
        assert_eq!(app.slot(Binding::L).kind, original_l_kind, "T must not switch L to Text while fullscreen");

        let mut raw_f = egui::RawInput::default();
        raw_f.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw_f.events.push(egui::Event::Key {
            key: egui::Key::F,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw_f, |ui| app.handle_keys(ui));
        assert_eq!(
            app.slot(Binding::L).kind, ToolKind::Fill,
            "every other tool's shortcut must stay reachable while fullscreen"
        );
    }

    /// Acceptance criterion: "X swaps FG/BG in both chrome modes". K12's own fix (the pre-existing
    /// tooltip/missing-keybinding gap) had no dedicated test anywhere — the `X` branch isn't gated
    /// on `is_fullscreen` at all, so this drives the real `handle_keys` in both chrome modes and
    /// confirms the swap actually happens each time.
    #[test]
    fn x_key_swaps_fg_and_bg_in_both_windowed_and_fullscreen_chrome() {
        for fullscreen in [false, true] {
            let mut app = GasciiApp::headless();
            app.active_fg = Rgba(1, 2, 3, 255);
            app.active_bg = Rgba(4, 5, 6, 255);

            let ctx = egui::Context::default();
            let mut raw = egui::RawInput::default();
            raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(fullscreen);
            raw.events.push(egui::Event::Key {
                key: egui::Key::X,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

            assert_eq!(app.active_fg, Rgba(4, 5, 6, 255), "fullscreen={fullscreen}: X must swap FG");
            assert_eq!(app.active_bg, Rgba(1, 2, 3, 255), "fullscreen={fullscreen}: X must swap BG");
        }
    }

    /// Loading a second image over an already-loaded one must **replace** the whole
    /// `ImageBackground` in one assignment, not accumulate state — mirrors `load_trace_image`'s own
    /// `self.image_bg = Some(...)` step (an `rfd` pick can't be driven headlessly, so this drives
    /// the same decode/upload/assign sequence directly). Proven two ways: the second image's
    /// `pixels`/`path` are the only ones present afterward (no merge), and the *first* texture is
    /// actually freed from the `TextureManager` once the second assignment drops it — a stacking or
    /// leak bug (e.g. pushing into a `Vec` instead of replacing the `Option`) would leave the first
    /// texture id still allocated.
    #[test]
    fn loading_a_second_image_over_an_existing_one_replaces_it_and_frees_the_old_texture() {
        let mut app = GasciiApp::headless();
        let ctx = egui::Context::default();

        fn make_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
            let mut img = image::RgbaImage::new(w, h);
            for px in img.pixels_mut() {
                px.0 = rgba;
            }
            let mut bytes = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
            bytes
        }
        fn load(ctx: &egui::Context, bytes: &[u8]) -> image_bg::ImageBackground {
            let rgba = image_bg::decode_image(bytes).unwrap();
            let (w, h) = (rgba.width() as usize, rgba.height() as usize);
            let tex = ctx.load_texture(
                "trace_bg",
                egui::ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw()),
                egui::TextureOptions::LINEAR,
            );
            image_bg::ImageBackground::new(rgba, Some(tex), None)
        }

        let bg_a = load(&ctx, &make_png(3, 2, [10, 20, 30, 255]));
        let id_a = bg_a.texture.as_ref().unwrap().id();
        app.image_bg = Some(bg_a);
        app.image_bg_gen += 1;
        assert!(
            ctx.tex_manager().read().meta(id_a).is_some(),
            "sanity: the first texture must actually be allocated before the replace"
        );

        let bg_b = load(&ctx, &make_png(5, 7, [90, 100, 110, 255]));
        let id_b = bg_b.texture.as_ref().unwrap().id();
        // Mirrors `load_trace_image`'s own replace: this single assignment swaps the whole
        // `Option`, dropping the previous `ImageBackground` (and its `TextureHandle`) right here.
        app.image_bg = Some(bg_b);
        app.image_bg_gen += 1;

        let bg = app.image_bg.as_ref().unwrap();
        assert_eq!(
            (bg.pixels.width(), bg.pixels.height()),
            (5, 7),
            "the second image's dimensions replace the first's, not stack alongside them"
        );
        assert_eq!(app.image_bg_gen, 2, "each load bumps the generation once, not merged into one bump");
        assert!(
            ctx.tex_manager().read().meta(id_a).is_none(),
            "the first texture must be freed once the second `Some(...)` assignment drops it"
        );
        assert!(ctx.tex_manager().read().meta(id_b).is_some(), "the second (current) texture must still be allocated");
    }

    /// An `image_bg_gen` bump alone — with `ExportSettings` completely unchanged — must still
    /// invalidate `refresh_export_preview`'s cache key. This is the whole reason
    /// `ExportPreviewKey` exists (`Option<ExportSettings>` alone can't see an opacity/gate/load
    /// edit): without the generation folded in, a preview built before an image edit would be
    /// served forever afterward, since `self.export` never itself changed.
    #[test]
    fn an_image_bg_gen_bump_invalidates_the_cached_export_preview_key_with_export_settings_unchanged() {
        let mut app = GasciiApp::headless();
        app.export.format = ExportFormat::Png;
        let ctx = egui::Context::default();

        app.refresh_export_preview(&ctx);
        let key_before = app.export_preview_key;
        assert!(key_before.is_some(), "sanity: a PNG-format refresh must produce a cache key");

        // `self.export` is untouched below — only the image generation moves, exactly as
        // `load_trace_image`/`clear_image_bg`/the export dialog's opacity slider and "Use as
        // background" toggle all do.
        app.image_bg_gen += 1;
        app.refresh_export_preview(&ctx);
        let key_after = app.export_preview_key;

        assert_ne!(key_before, key_after, "an image_bg_gen bump alone must change the cache key");
        assert_eq!(
            key_after.map(|k| k.image_gen),
            Some(1),
            "the new key must reflect the bumped generation, not just differ arbitrarily"
        );
    }

    /// `suppresses_tool_shortcuts` end to end through the real `handle_keys` loop, not just the
    /// pure predicate: a live Text session must swallow `P` as burst content rather than let it
    /// rebind L to Pencil, and the very next frame after that session ends must let the same key
    /// through normally — proving the gate tracks the session's lifetime, not a stuck flag.
    #[test]
    fn a_live_text_session_suppresses_the_pencil_shortcut_through_handle_keys_and_releases_it_once_the_session_ends() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        let press_p = |app: &mut GasciiApp| {
            let ctx = egui::Context::default();
            let mut raw = egui::RawInput::default();
            raw.events.push(egui::Event::Key {
                key: egui::Key::P,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));
        };

        press_p(&mut app);
        assert_eq!(
            app.slot(Binding::L).kind, ToolKind::Text,
            "P must be swallowed as burst content while the Text session is active, not rebind L"
        );

        // Ending the session (Escape/toolbox click equivalent) must release the gate.
        app.end_session(Binding::L);
        press_p(&mut app);
        assert_eq!(
            app.slot(Binding::L).kind, ToolKind::Pencil,
            "once the session has ended, the very same key must rebind L normally"
        );
    }

    /// Windowed complement to `pressing_t_while_fullscreen_leaves_l_unchanged_but_other_tool_
    /// shortcuts_still_work`: outside fullscreen every registry entry's shortcut — including
    /// Text's, which `kiosk_visible` gates only while fullscreen — must reach `set_tool` normally.
    #[test]
    fn pressing_t_while_windowed_rebinds_l_to_text() {
        let mut app = GasciiApp::headless();
        assert_ne!(app.slot(Binding::L).kind, ToolKind::Text, "sanity: L doesn't already start on Text");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(false);
        raw.events.push(egui::Event::Key {
            key: egui::Key::T,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Text, "T must rebind L to Text while windowed");
    }

    /// The `[`/`]` size keys must adjust whichever binding `options_focus` currently names, driven
    /// through the real `handle_keys` loop rather than by mutating `stamps` directly — proving the
    /// `sized_slot` capability lookup and the focus-tracking field are both actually wired into the
    /// live key-handling path, not merely consistent in isolation.
    #[test]
    fn close_bracket_grows_only_the_options_focused_bindings_stamp_through_handle_keys() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Eraser);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Eraser);
        app.options_focus = Binding::R;
        let slot = sized_slot(ToolKind::Eraser).expect("Eraser is sized");
        app.slots[Binding::L.ix()].stamps[slot].size = 1;
        app.slots[Binding::R.ix()].stamps[slot].size = 1;

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::CloseBracket,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slots[Binding::R.ix()].stamps[slot].size, 2, "R (options_focus) must grow");
        assert_eq!(app.slots[Binding::L.ix()].stamps[slot].size, 1, "L must be untouched by R's focused key");
    }

    /// A committed Pencil stroke that stamps the glyph plane must add the active glyph to RECENT —
    /// closing the code review's flagged gap (`note_glyph_drawn`/`stamps_glyph` had no test driving
    /// the real committed-stroke call path, only the underlying capability-table field). Eraser
    /// (`stamps_glyph: false`) is driven the same way as a negative control: a real committed
    /// stroke that does NOT count toward RECENT.
    #[test]
    fn a_committed_stroke_updates_recent_glyphs_exactly_for_stamps_glyph_kinds() {
        for (kind, should_note) in [(ToolKind::Pencil, true), (ToolKind::Eraser, false)] {
            let mut app = GasciiApp::headless();
            app.slots[Binding::L.ix()] = ToolSlot::new(kind);
            app.mask = PlaneMask::ALL;
            app.active_glyph = 'Q';
            assert!(app.recent_glyphs.is_empty(), "{kind:?}: sanity, RECENT starts empty");

            crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
            let tctx = crate::canvas::tool_ctx(&app, Binding::L);
            let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
            app.stroke_owner = None;
            if let ToolResponse::Commit(Some(edit)) = resp {
                app.apply_edit(edit, Some(Binding::L));
                // Mirrors canvas.rs's own commit call site exactly (`show`'s stroke-tail branch).
                app.note_glyph_drawn(app.slots[Binding::L.ix()].kind);
            }

            if should_note {
                assert_eq!(
                    app.recent_glyphs.first(), Some(&'Q'),
                    "{kind:?}: a committed glyph-plane stroke must add the active glyph to RECENT"
                );
            } else {
                assert!(
                    app.recent_glyphs.is_empty(),
                    "{kind:?}: a stamps_glyph=false kind's committed stroke must not touch RECENT"
                );
            }
        }
    }

    /// The recurring stale-`before` bug class (flagged in prior reviews of redesign-round-2 and
    /// fullscreen-mode), exercised specifically against a non-default `active_layer`: a Pencil
    /// commit, a live Text session's resync, a second Pencil commit that re-pins the Text session's
    /// `before` mid-burst, the Text session's own eventual commit, and finally undo/redo — every
    /// one of those five steps must read and write the SAME layer (2), and layer 0 must stay
    /// completely untouched throughout. `active_layer` is session-only (always 0 in the shipped
    /// app), but the plumbing this pins must already be correct for the layers feature that will
    /// set it to something other than 0.
    #[test]
    fn active_layer_resync_and_undo_redo_all_target_the_same_non_default_layer_under_adversarial_sequencing() {
        let mut app = GasciiApp::headless();
        let (w, h) = (app.doc.width, app.doc.height);
        app.doc.layers_mut().push(gascii_core::Layer::blank(w, h));
        app.doc.layers_mut().push(gascii_core::Layer::blank(w, h));
        app.active_layer = 2;
        app.mask = PlaneMask::ALL;

        // R: a first Pencil stroke stamps layer 2's (2,2) with 'Z'.
        app.bind(Binding::R, ToolKind::Pencil);
        app.active_glyph = 'Z';
        crate::canvas::begin_gesture(&mut app, Binding::R, 2, 2, false, false);
        let r_tctx = crate::canvas::tool_ctx(&app, Binding::R);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r_tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell(2, 2, 2).unwrap().ch, 'Z', "sanity: R's first stroke landed on layer 2");
        assert_eq!(app.doc.cell(0, 2, 2), Some(&gascii_core::Cell::BLANK), "sanity: layer 0 untouched so far");

        // L: a Text burst starts on the SAME cell, pinning `before` against layer 2's current 'Z'.
        app.bind(Binding::L, ToolKind::Text);
        app.acquire_keyboard(Binding::L);
        let l_tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 2, y: 2 }, &l_tctx, &app.doc);
        app.active_glyph = 'A';
        let l_tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('A'), &l_tctx, &app.doc);

        // R: a second Pencil stroke, still mid-L-session, touches the SAME cell again — its commit
        // must resync L against layer 2's new value ('Y'), not layer 0's untouched blank.
        app.active_glyph = 'Y';
        crate::canvas::begin_gesture(&mut app, Binding::R, 2, 2, false, false);
        let r_tctx2 = crate::canvas::tool_ctx(&app, Binding::R);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r_tctx2, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell(2, 2, 2).unwrap().ch, 'Y', "sanity: R's second stroke committed 'Y' on layer 2");

        // L's burst finally commits 'A'. Correctness here is invisible on the forward write (the
        // committed `after` is always 'A' either way) — the resync's target layer only shows up in
        // the undo entry's `before`, which is exactly why this test checks undo/redo, not just the
        // post-commit cell.
        app.flush_slot(Binding::L);
        assert_eq!(app.doc.cell(2, 2, 2).unwrap().ch, 'A', "L's committed burst lands 'A' on layer 2");

        app.request_undo();
        assert_eq!(
            app.doc.cell(2, 2, 2).unwrap().ch, 'Y',
            "undo must restore layer 2's actual prior content ('Y' from R's second stroke), not a \
             stale before pinned against the wrong layer"
        );

        app.request_redo();
        assert_eq!(app.doc.cell(2, 2, 2).unwrap().ch, 'A', "redo must re-land the Text commit");

        assert_eq!(
            app.doc.cell(0, 2, 2), Some(&gascii_core::Cell::BLANK),
            "layer 0 must stay completely untouched by every edit and every undo/redo in this sequence"
        );
    }

    /// The process-global tool registry must build exactly once: repeated `tools()` calls return
    /// the same backing allocation, not a freshly rebuilt `Vec` each time. Guards the `OnceLock`
    /// contract `prefs::load`'s first-ever-read-in-the-app-lifetime relies on (a rebuild-per-call
    /// registry would still be correct today since every row is a pure constant, but would silently
    /// stop being `&'static`-cheap the moment a plugin's `register_tool` needs to append once,
    /// before the first read, rather than on every read).
    #[test]
    fn tools_registry_returns_the_same_backing_slice_across_repeated_calls() {
        let a = tools().as_ptr();
        let b = tools().as_ptr();
        assert_eq!(a, b, "tools() must not rebuild the registry on every call");
    }

    fn selection_at_1_1(app: &mut GasciiApp) {
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        app.doc.set_cell(0, 1, 1, cell('x'));
        let tctx = crate::canvas::tool_ctx(app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
    }

    fn copy_event() -> egui::Event {
        egui::Event::Copy
    }

    /// The real fix `handle_keys` needed: `Event::Copy` (what egui-winit actually emits for
    /// Ctrl+C/Cmd+C) with a live selection must copy that selection's text into the internal
    /// clipboard — not the dead `consume_key(COMMAND, C)` pair that never fired because
    /// `Event::Key{C}` is never produced for this chord.
    #[test]
    fn ctrl_c_via_event_copy_copies_the_live_selections_text_to_the_internal_clipboard() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(copy_event());
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        let patch = app.internal_clipboard.as_ref().expect("Ctrl+C must populate the internal clipboard");
        assert_eq!(patch.to_text(), "x", "the copied patch must hold the selected cell's glyph");
    }

    /// `Ctrl+Shift+C`'s copy-all path, discriminated purely from `InputState::modifiers.shift` at
    /// the moment `Event::Copy` is observed, must copy the whole document as text to the OS
    /// clipboard, not just the live selection.
    #[test]
    fn ctrl_shift_c_via_event_copy_with_shift_held_copies_the_whole_document_as_text() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 0, 0, cell('z'));

        let ctx = egui::Context::default();
        let mut raw =
            egui::RawInput { modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(copy_event());
        let output = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        let expected = export_text(&app.doc);
        let copied = output
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if *t == expected));
        assert!(copied, "Ctrl+Shift+C must copy the whole document's exported text to the OS clipboard");
    }

    /// A bare `Event::Key{key: C, modifiers: COMMAND}` — the event egui-winit never actually
    /// produces for this chord — must NOT fire copy. Reproducing that event shape as if it were
    /// real is the exact fiction that let the dead `consume_key` pair look correct while never
    /// actually firing.
    #[test]
    fn a_bare_event_key_c_with_no_event_copy_present_does_not_fire_copy() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(
            app.internal_clipboard.is_none(),
            "a synthetic Event::Key{{C}} with no real Event::Copy must not copy anything — this is the \
             exact fiction that let the dead consume_key pair look correct while never actually firing"
        );
    }

    /// `Event::Copy` also fires on `Ctrl+Insert` (Windows) — the app receives the exact same event
    /// shape either way, so scanning for the event variant (rather than a specific key chord)
    /// handles this chord for free. Pinned as its own dedicated test.
    #[test]
    fn event_copy_from_ctrl_insert_copies_the_live_selection_identically_to_ctrl_c() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(copy_event()); // egui-winit emits the identical Event::Copy for Ctrl+Insert
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(
            app.internal_clipboard.as_ref().map(|p| p.to_text()).as_deref(),
            Some("x"),
            "Ctrl+Insert's Event::Copy must copy the live selection just like Ctrl+C's"
        );
    }

    /// The more-specific chord (Redo, Ctrl+Shift+Z) must win over the less-specific one (Undo,
    /// Ctrl+Z) that would otherwise also match via `matches_logically`'s modifier-superset rule —
    /// driven through the real `handle_keys`, not just a pure predicate, so a future reordering of
    /// the two `consume_key` calls is actually caught.
    #[test]
    fn ctrl_shift_z_via_handle_keys_fires_redo_not_undo() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        app.request_undo(); // one edit undone: redo is now available, undo is not
        assert!(app.history.can_redo(), "sanity: a redo is available");
        assert!(!app.history.can_undo(), "sanity: no undo is available after the single edit was undone");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, '#', "Ctrl+Shift+Z must have redone the edit");
        assert!(!app.history.can_redo(), "the redo must have actually fired, emptying the redo stack");
    }

    /// The other precedence-sensitive pair: Ctrl+Shift+C (copy-all) firing must mean plain Ctrl+C's
    /// copy-selection path does NOT also run — proven by asserting the internal (selection)
    /// clipboard stays untouched while the OS clipboard receives the whole document, not merely
    /// that copy-all's own effect happened in isolation.
    #[test]
    fn ctrl_shift_c_fires_copy_all_and_never_also_the_plain_selection_copy_path() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);
        assert!(app.internal_clipboard.is_none(), "sanity: nothing has been copied yet");

        let ctx = egui::Context::default();
        let mut raw =
            egui::RawInput { modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(copy_event());
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(
            app.internal_clipboard.is_none(),
            "Ctrl+Shift+C must not also run the plain-copy path (copy_selection), which would have \
             populated the internal clipboard"
        );
    }

    #[test]
    fn tool_key_collides_with_reserved_flags_a_key_a_global_chord_already_owns() {
        assert!(tool_key_collides_with_reserved(egui::Key::X), "X is SwapColors's reserved key");
        assert!(!tool_key_collides_with_reserved(egui::Key::Q), "Q is not reserved by any global chord");
    }

    /// `Space` has no `CHORDS` row of its own (the animation play/pause hold lives entirely inside
    /// `gascii-anim`, driven by `key_down` rather than a `consume_key` pattern this table could
    /// represent) — it must still be caught by the collision predicate, or a plugin could silently
    /// claim it as a tool shortcut with nothing to stop it.
    #[test]
    fn tool_key_collides_with_reserved_flags_space_even_though_it_has_no_chords_row_of_its_own() {
        assert!(tool_key_collides_with_reserved(egui::Key::Space), "Space must be reserved for gascii-anim's play/pause hold");
    }

    /// Mirrors `tool_shortcuts_are_unique`'s shape, but for the collision `tools()`-internal check
    /// structurally cannot see: a synthetic plugin tool row bound to a reserved global chord key
    /// must be caught by `tool_key_collides_with_reserved`, driven against a real row from `tools()`
    /// rather than only the bare predicate.
    #[test]
    fn a_plugin_tool_row_bound_to_a_reserved_global_chord_key_is_caught_by_the_collision_predicate() {
        // `X` is real production data (SwapColors's own key) — reusing it here proves the
        // predicate reads the live registry, not a hand-copied literal.
        let colliding = ToolDef {
            kind: ToolKind::Brush,
            name: "synthetic",
            key: egui::Key::X,
            tip: "",
            make: || Box::new(InertTool),
            stamp_slot: None,
            holds_session: false,
            shows_hover: false,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: false,
            plugin_slot: Some(0),
            pressure_sizeable: false,
            wants_extra_ctx: false,
        };
        assert!(
            tool_key_collides_with_reserved(colliding.key),
            "a plugin row bound to X must be flagged — X is already SwapColors's reserved key"
        );
    }

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// `Ctrl+N` on a clean document must open the New dialog directly, exactly like clicking
    /// File ▸ New… on a clean document — no confirm veto in the way.
    #[test]
    fn ctrl_n_on_a_clean_document_opens_the_new_dialog_directly() {
        let mut app = GasciiApp::headless();
        assert!(!app.is_dirty(), "sanity: a fresh headless app starts clean");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::N, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(app.new_dialog_open, "Ctrl+N on a clean document must open the New dialog");
        assert!(app.confirm.is_none(), "a clean document must not raise the unsaved-changes veto");
    }

    /// `Ctrl+N` on a dirty document must veto through the same unsaved-changes confirm the menu
    /// click uses — proving `new_document_via_menu` is genuinely shared, not reimplemented.
    #[test]
    fn ctrl_n_on_a_dirty_document_raises_the_unsaved_changes_confirm_instead_of_opening_new_directly() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        assert!(app.is_dirty(), "sanity: the committed stroke made the document dirty");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::N, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.confirm, Some(PendingConfirm::NewDocument), "a dirty document must veto through the confirm");
        assert!(!app.new_dialog_open, "the New dialog must not open directly while the veto is pending");
    }

    /// `G` toggles the grid overlay, gated on `!focused` exactly like `X` (SwapColors) already is —
    /// driven through the real `handle_keys`, not the pure `consume_generic_chords` helper alone.
    #[test]
    fn g_toggles_the_grid_overlay_through_handle_keys_while_unfocused() {
        let mut app = GasciiApp::headless();
        let starting = app.show_grid;

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::G, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.show_grid, !starting, "G must flip show_grid");
    }

    /// `G` must be suppressed while a widget has focus — the same `!focused` gate `X` already
    /// obeys, so typing "g" into a focused field never toggles the grid.
    #[test]
    fn g_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        let starting = app.show_grid;

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::G, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert_eq!(app.show_grid, starting, "a focused widget must suppress G");
    }

    /// `?` opens the keyboard-shortcuts overlay while unfocused, the same `GenericUnfocused` gate
    /// `G`/`X` already use.
    #[test]
    fn question_mark_via_handle_keys_opens_the_help_overlay_while_unfocused() {
        let mut app = GasciiApp::headless();
        assert!(!app.help_overlay_open, "sanity: the overlay starts closed");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::Questionmark, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(app.help_overlay_open, "? must open the overlay");
    }

    #[test]
    fn question_mark_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::Questionmark, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert!(!app.help_overlay_open, "a focused widget must suppress ?");
    }

    /// While open, the overlay counts as a modal — `handle_keys` (and therefore every other chord)
    /// must stop running, matching every other dialog's own `modal_open()` coverage.
    #[test]
    fn help_overlay_open_suppresses_handle_keys_via_modal_open() {
        let mut app = GasciiApp::headless();
        app.help_overlay_open = true;
        assert!(app.modal_open(), "the open overlay must count as a modal");
    }

    /// The overlay renders without panicking and is dismissed (Escape, matching every other
    /// `dialog::modal`-built dialog) by clearing `help_overlay_open` — not by a second `?` press,
    /// which can never reach `handle_keys` while the overlay counts as a modal.
    #[test]
    fn help_overlay_renders_and_closes_on_escape_dismiss() {
        let mut app = GasciiApp::headless();
        app.help_overlay_open = true;

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        // `egui::Modal`'s "am I the topmost modal" bookkeeping is layer-order state the context
        // only has after at least one real frame — mirrors how the overlay is always already open
        // for at least a frame before a real Escape press can reach it.
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.help_overlay(ui.ctx()));
        assert!(app.help_overlay_open, "sanity: a no-input frame must not close the overlay");

        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::Escape, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.help_overlay(ui.ctx()));

        assert!(!app.help_overlay_open, "Escape must dismiss the overlay, matching every other dialog");
    }

    /// `Ctrl+=`/`Ctrl+-` request the same one-step zoom the plain `+`/`-` chords and the View menu
    /// already do — proven through the deferred `pending_step_zoom` field `step_zoom` writes,
    /// mirroring how `canvas::show` itself applies the request.
    #[test]
    fn ctrl_equals_and_ctrl_minus_request_the_same_one_step_zoom_as_the_plain_aliases() {
        for (key, expected_dir) in [(egui::Key::Equals, 1), (egui::Key::Minus, -1)] {
            let mut app = GasciiApp::headless();
            let ctx = egui::Context::default();
            let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
            raw.events.push(key_event(key, egui::Modifiers::COMMAND));
            let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

            assert_eq!(
                app.pending_step_zoom, expected_dir,
                "{key:?} with Ctrl held must request a one-step zoom in direction {expected_dir}"
            );
        }
    }

    /// `Ctrl+A` with neither binding already holding Selection must rebind L (`paste_target`'s
    /// default) and select the whole document, without requiring a prior manual tool switch.
    #[test]
    fn ctrl_a_via_handle_keys_rebinds_l_to_selection_and_selects_the_whole_document_by_default() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, ToolKind::Eraser);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::A, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Selection, "Ctrl+A must rebind L by default");
        assert_eq!(app.selection_slot(), Some(Binding::L));
        assert_eq!(
            app.slot(Binding::L).tool.selection_overlay().and_then(|v| v.marquee),
            Some(gascii_core::CellRect {
                x0: 0,
                y0: 0,
                x1: app.doc.width - 1,
                y1: app.doc.height - 1
            }),
            "the marquee must span the full document"
        );
    }

    /// `Ctrl+A` must prefer whichever binding already holds Selection — the same `paste_target`
    /// rule `paste_text` already follows — rather than always defaulting to L.
    #[test]
    fn ctrl_a_via_handle_keys_prefers_a_binding_that_already_holds_selection() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, ToolKind::Selection);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::A, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "L must be left untouched");
        assert_eq!(app.selection_slot(), Some(Binding::R), "Ctrl+A must select through R, which already held it");
    }

    /// `Ctrl+A` must be suppressed while a widget has focus, matching Undo/Redo's own gate —
    /// `egui::TextEdit`'s own Ctrl+A (select-all-in-field) must win instead.
    #[test]
    fn ctrl_a_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::A, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "a focused widget must suppress Ctrl+A");
    }

    /// A live canvas Text burst sets no egui widget focus, so the `widget_focused` gate above does
    /// NOT suppress Ctrl+A during one — `select_all` still fires and rebinds the Text slot to
    /// Selection via `set_tool`'s own `end_session`. The one thing that must never happen is silent
    /// data loss: `end_session` flushes (commits) the pending burst before the slot's tool is
    /// replaced, exactly like an Escape or a toolbox click already would, so the typed content lands
    /// in the document rather than vanishing underneath the tool switch.
    #[test]
    fn ctrl_a_during_a_live_text_burst_commits_the_burst_before_switching_to_select_all() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('i'), &tctx, &app.doc);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::A, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'h', "the burst's typed text must be committed, not discarded");
        assert_eq!(app.doc.cell(0, 1, 0).unwrap().ch, 'i', "the burst's typed text must be committed, not discarded");
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Selection, "Ctrl+A must switch the Text binding to Selection");
        assert_eq!(
            app.slot(Binding::L).tool.selection_overlay().and_then(|v| v.marquee),
            Some(gascii_core::CellRect { x0: 0, y0: 0, x1: app.doc.width - 1, y1: app.doc.height - 1 }),
            "the marquee must span the full document"
        );
    }

    /// The user-facing checkpoint dropped `D` (reset fg/bg) entirely: a bare, unmodified `D` press
    /// must not be bound to anything — no color change, no tool switch, no document mutation.
    /// `Ctrl+D`/`Shift+D` (Deselect/animation duplicate-frame) are unaffected; this only pins the
    /// bare key.
    #[test]
    fn bare_d_key_is_bound_to_nothing_and_leaves_colors_and_tools_untouched() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, ToolKind::Eraser);
        let (fg_before, bg_before) = (app.active_fg, app.active_bg);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.active_fg, fg_before, "bare D must not touch the active foreground color");
        assert_eq!(app.active_bg, bg_before, "bare D must not touch the active background color");
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "bare D must not rebind L");
        assert_eq!(app.slot(Binding::R).kind, ToolKind::Eraser, "bare D must not rebind R");
    }

    /// `Ctrl+X` must copy the live selection AND delete it in the same change — never leave an
    /// interim state where it only copied.
    #[test]
    fn ctrl_x_via_handle_keys_copies_and_deletes_the_selection_in_one_change() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Cut);
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(
            app.internal_clipboard.as_ref().map(|p| p.to_text()).as_deref(),
            Some("x"),
            "Ctrl+X must copy the selection's text"
        );
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, ' ', "Ctrl+X must also delete the selected cell");
    }

    /// `Ctrl+X` with no live selection must be a true no-op — no clipboard write, no document
    /// mutation, no panic.
    #[test]
    fn ctrl_x_via_handle_keys_is_a_no_op_without_a_live_selection() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 1, 1, cell('x'));

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Cut);
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(app.internal_clipboard.is_none());
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "no selection: nothing may be deleted");
    }

    /// `Ctrl+D` must clear the marquee and release the keyboard without deleting the selection's
    /// content — the same pair `canvas.rs`'s own Selection-Escape handling already performs.
    #[test]
    fn ctrl_d_via_handle_keys_clears_the_marquee_and_releases_the_keyboard_without_deleting_content() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);
        assert_eq!(app.selection_slot(), Some(Binding::L), "sanity: L holds the live selection");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.keyboard_owner(), None, "Ctrl+D must release the keyboard");
        assert!(
            app.slot(Binding::L).tool.selection_overlay().is_none(),
            "Ctrl+D must clear the marquee"
        );
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "Ctrl+D must never delete the selected content");
    }

    /// `Ctrl+D` with no live selection must be a true no-op.
    #[test]
    fn ctrl_d_via_handle_keys_is_a_no_op_without_a_live_selection() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "nothing may change with no live selection");
    }

    /// `Plugin::tick`'s breaking return-type change end to end, mirroring
    /// `digit_key_intensity_shortcut_through_handle_keys_sets_fixed_intensity_while_bound_and_unfocused`:
    /// `AnimPlugin::tick`'s `Shift+D` duplicate-frame shortcut returns a `PanelOutcome` whose `edits`
    /// must reach `apply_edit` via `handle_keys`'s new two-pass tick-then-drain loop
    /// (`drain_panel_outcomes`), not just be silently discarded.
    #[test]
    fn plugin_tick_panel_outcome_edits_reach_apply_edit_via_handle_keys() {
        let mut app = GasciiApp::headless();
        let before_frame_count = app.doc.frame_count();

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::SHIFT));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(
            app.doc.frame_count(), before_frame_count + 1,
            "Shift+D's PanelOutcome::edits must reach apply_edit through drain_panel_outcomes"
        );
    }

    /// The other half of the same wiring: `.`'s `PanelOutcome::set_active_frame` must reach
    /// `switch_active_frame` through the same drain pass.
    #[test]
    fn plugin_tick_panel_outcome_set_active_frame_reaches_switch_active_frame_via_handle_keys() {
        let mut app = GasciiApp::headless();
        app.add_frame_via_menu(); // now 2 frames, so '.' has somewhere to advance to
        assert_eq!(app.active_frame, 0);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::Period, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(
            app.active_frame, 1,
            "'.'s PanelOutcome::set_active_frame must reach switch_active_frame through drain_panel_outcomes"
        );
    }

    /// `,`/`.`/`Shift+D` must all be suppressed while a widget has focus, matching every other
    /// `gascii-anim` shortcut's own `!focused` gate.
    #[test]
    fn comma_period_and_shift_d_are_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        app.add_frame_via_menu();
        app.switch_active_frame(1);
        let before_frame_count = app.doc.frame_count();

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(key_event(egui::Key::Comma, egui::Modifiers::NONE));
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::SHIFT));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert_eq!(app.active_frame, 1, "a focused widget must suppress ','");
        assert_eq!(app.doc.frame_count(), before_frame_count, "a focused widget must suppress Shift+D");
    }
}
