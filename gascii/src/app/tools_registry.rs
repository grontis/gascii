//! The tool registry: `ToolKind`/`Binding`/`ToolSlot`/`StampSettings`, the `ToolDef` table
//! (`build_tools`, `PLUGINS`, `TOOL_REGISTRY`), and the pure validation passes (unique names, key
//! collisions) run over it once at first read.

use eframe::egui;
use gascii_core::{
    BrushShape, Document, Eraser, FloodFill, Line, Pencil, Rectangle, SelectionTool, TextTool,
    Tool, ToolEvent, ToolResponse,
};

use gascii_plugin_api::{CanvasRenderer, Plugin};

use crate::canvas::NaiveRenderer;
use crate::chords;

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
    /// A plugin-contributed tool, identified by its registry `name` — never minted by hand outside
    /// this module. Every value that exists in the app originates from a `tools()` row (`build_tools`'s
    /// `merge_plugin_row`), from prefs' name resolution (`tool_kind_from_str`, which itself only
    /// ever returns a `tools()` row's kind), or from a `#[cfg(test)]` const. `tool_def`'s `expect`
    /// is the enforcement of that invariant: a hand-constructed `ToolKind::Plugin("nope")` that
    /// never went through the registry panics there, not silently.
    Plugin(&'static str),
}

/// Test-only stand-in for the pre-migration `ToolKind::Brush` literal — `ToolKind` is no longer
/// total-by-type over its plugin-sourced tools, so this is the one place a test names Brush's kind
/// directly rather than re-deriving `ToolKind::Plugin(gascii_density_brush::BRUSH)` at every site.
#[cfg(test)]
pub(crate) const BRUSH_KIND: ToolKind = ToolKind::Plugin(gascii_density_brush::BRUSH);

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
    /// and R's Eraser size are independent by construction rather than by two parallel arrays. Sized
    /// at construction to `sized_tool_count()` — the registry's own row count, not a compile-time
    /// constant, since a sized row's runtime index is now derived from table order rather than
    /// hand-assigned.
    pub stamps: Vec<StampSettings>,
    /// The terminal cell of this slot's last committed `Line` stroke, so a Shift-held fresh Press
    /// can continue from it (`begin_gesture`). `None` until a Line stroke actually commits, and
    /// cleared whenever the binding is rebound away from `Line` (`set_tool`) so a later rebind back
    /// to `Line` never resumes a point from an unrelated editing session.
    pub last_line_point: Option<(u16, u16)>,
}

impl ToolSlot {
    pub(super) fn new(kind: ToolKind) -> Self {
        ToolSlot {
            kind,
            tool: make_tool(kind),
            stamps: vec![StampSettings::default(); sized_tool_count()],
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

/// Number of sized tools — every `ToolSlot::stamps`' length. Derived from the live registry rather
/// than a compile-time constant, since a plugin-contributed sized tool grows this count without any
/// host edit.
pub(crate) fn sized_tool_count() -> usize {
    tools().iter().filter(|d| d.sized).count()
}

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
pub(super) struct InertTool;

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
    /// This row's icon, painted by `icons::paint`; an empty slice falls back to `name`'s first
    /// letter.
    pub icon: &'static [gascii_plugin_api::IconPath],
    /// Whether this kind has a size/shape footprint. Drives `stamp_slot`'s assignment (see
    /// `build_tools`'s post-pass) — never set the slot directly.
    pub sized: bool,
    /// Slot in a `ToolSlot`'s `stamps` array for this kind's size/shape footprint; `None` for
    /// unsized kinds. Assigned by `build_tools`'s post-pass (a dense counter over `sized` rows in
    /// table order) — never a literal in this table, and never persisted positionally (`prefs.rs`
    /// persists stamps by name).
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
    /// The index into `PLUGINS`/`GasciiApp::plugins` that owns this row, for a plugin-sourced tool;
    /// `None` for every pure built-in row. This is how `sidebar::binding_options_geom`'s dedup,
    /// `tool_ctx`'s ctx-patch injection, and the pressure-override gate all find "which live plugin
    /// instance, if any, owns this bound row" without a second lookup table.
    pub plugin_slot: Option<usize>,
    /// Whether a stylus-pressure stroke should override this kind's stamp size.
    pub pressure_sizeable: bool,
    /// Whether `tool_ctx` should ask the owning plugin (via `plugin_slot`) for a `ToolCtxPatch`
    /// while this kind is bound.
    pub wants_ctx_patch: bool,
}

/// The eight pure built-in tools (Brush's row is plugin-sourced — see `PLUGINS`), and the single
/// source of truth for their names, shortcuts, hints, constructors, and capability facts. Feeds
/// `tools()`, the registry every call site (the toolbox, the shortcut handler, the sidebar's option
/// rows, both bindings, prefs) reads.
pub(super) fn build_tools() -> Vec<ToolDef> {
    let mut rows = vec![
        ToolDef {
            kind: ToolKind::Pencil,
            name: "Pencil",
            key: egui::Key::P,
            tip: "Draw the active glyph",
            make: || Box::new(Pencil::new()),
            icon: crate::ui::icons::PENCIL,
            sized: true,
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: true,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_ctx_patch: false,
        },
        ToolDef {
            kind: ToolKind::Eraser,
            name: "Eraser",
            key: egui::Key::E,
            tip: "Erase cells to blank",
            make: || Box::new(Eraser::new()),
            icon: crate::ui::icons::ERASER,
            sized: true,
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_ctx_patch: false,
        },
        ToolDef {
            kind: ToolKind::Text,
            name: "Text",
            key: egui::Key::T,
            tip: "Click to place a cursor, then type",
            make: || Box::new(TextTool::new()),
            icon: crate::ui::icons::TEXT,
            sized: false,
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
            wants_ctx_patch: false,
        },
        ToolDef {
            kind: ToolKind::Fill,
            name: "Fill",
            key: egui::Key::F,
            tip: "Flood-fill a connected region",
            make: || Box::new(FloodFill::new()),
            icon: crate::ui::icons::FILL,
            sized: false,
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: true,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_ctx_patch: false,
        },
        ToolDef {
            kind: ToolKind::Rectangle,
            name: "Rectangle",
            key: egui::Key::R,
            tip: "Drag a box outline; joins box-drawing art",
            make: || Box::new(Rectangle::new()),
            icon: crate::ui::icons::RECTANGLE,
            sized: false,
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: true,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_ctx_patch: false,
        },
        ToolDef {
            kind: ToolKind::Line,
            name: "Line",
            key: egui::Key::L,
            tip: "Drag a straight line; joins box-drawing art",
            make: || Box::new(Line::new()),
            icon: crate::ui::icons::LINE,
            sized: true,
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: true,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_ctx_patch: false,
        },
        ToolDef {
            kind: ToolKind::Selection,
            name: "Selection",
            key: egui::Key::S,
            tip: "Drag a region to move, copy, or delete",
            make: || Box::new(SelectionTool::new()),
            icon: crate::ui::icons::SELECTION,
            sized: false,
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
            wants_ctx_patch: false,
        },
        ToolDef {
            kind: ToolKind::Eyedropper,
            name: "Eyedropper",
            key: egui::Key::I,
            tip: "Click a cell to pick up its text and background colors",
            make: || Box::new(InertTool),
            icon: crate::ui::icons::EYEDROPPER,
            sized: false,
            stamp_slot: None,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            plugin_slot: None,
            pressure_sizeable: false,
            wants_ctx_patch: false,
        },
    ];
    for (i, descriptor) in PLUGINS.iter().enumerate() {
        for cap in (descriptor.tools)() {
            rows.push(merge_plugin_row(i, &cap));
        }
    }
    assign_stamp_slots(&mut rows);
    if let Err(dup) = validate_unique_plugin_ids(PLUGINS) {
        panic!("{dup}");
    }
    if let Err(dup) = validate_unique_tool_names(&rows) {
        panic!("{dup}");
    }
    if let Err(e) = validate_key_claims(&key_claims(&rows)) {
        panic!("{e}");
    }
    rows
}

/// Assigns every `sized` row a dense, distinct `stamp_slot` index in table order — a plain counter,
/// never a literal per row. Runtime-only: the index never leaves the process (`prefs.rs` persists
/// stamps by tool name, not by this index), so a plugin adding a sized tool never requires a host
/// edit here.
fn assign_stamp_slots(rows: &mut [ToolDef]) {
    let mut next: u8 = 0;
    for d in rows.iter_mut() {
        if d.sized {
            d.stamp_slot = Some(next);
            next += 1;
        }
    }
}

/// Two plugins (or a plugin and a built-in) registering the same tool name would make `tool_def`'s
/// `find()` resolve ambiguously — the first row wins and the second is never reachable by name.
/// Caught here, at registry-construction time, rather than silently.
pub(super) fn validate_unique_tool_names(rows: &[ToolDef]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for d in rows {
        if !seen.insert(d.name) {
            return Err(format!("duplicate tool name {:?} — two rows registered the same name", d.name));
        }
    }
    Ok(())
}

/// Two plugins persisting under the same `id` would make prefs resolution ambiguous — the first
/// `position()` match wins and the second plugin's stored enabled state silently applies to the
/// wrong one. Caught here, at registry-construction time, rather than silently.
pub(super) fn validate_unique_plugin_ids(descriptors: &[gascii_plugin_api::PluginDescriptor]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for d in descriptors {
        if !seen.insert(d.id) {
            return Err(format!("duplicate plugin id {:?} — two descriptors registered the same id", d.id));
        }
    }
    Ok(())
}

/// Where one bare-key claim in the app comes from — a chord, a tool's own shortcut letter, or a
/// plugin's declared `tick` shortcut — named so `validate_key_claims`'s error can point at both
/// claimants precisely.
#[derive(Clone, Copy, Debug)]
pub(super) enum ClaimSource {
    Chord(&'static str),
    Tool(&'static str),
    Plugin { plugin: &'static str, action: &'static str },
}

impl std::fmt::Display for ClaimSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimSource::Chord(name) => write!(f, "chord {name:?}"),
            ClaimSource::Tool(name) => write!(f, "tool {name:?}"),
            ClaimSource::Plugin { plugin, action } => write!(f, "plugin {plugin:?}'s {action:?} shortcut"),
        }
    }
}

pub(super) struct KeyClaim {
    pub(super) key: egui::Key,
    pub(super) source: ClaimSource,
}

/// Every bare-key claim in the app: every reserved chord's modifier-less key pattern, every tool's
/// shortcut letter, and every `PLUGINS` descriptor's declared `tick` shortcut keys. The full input
/// `validate_key_claims` checks for collisions — this function only assembles the claim set, so it
/// stays testable against a synthetic `rows` slice with no live registry.
pub(super) fn key_claims(rows: &[ToolDef]) -> Vec<KeyClaim> {
    let mut claims: Vec<KeyClaim> = chords::reserved_chord_keys()
        .map(|(key, name)| KeyClaim { key, source: ClaimSource::Chord(name) })
        .collect();
    for d in rows {
        claims.push(KeyClaim { key: d.key, source: ClaimSource::Tool(d.name) });
    }
    for descriptor in PLUGINS {
        for shortcut in (descriptor.shortcuts)() {
            for &key in shortcut.keys {
                claims.push(KeyClaim { key, source: ClaimSource::Plugin { plugin: descriptor.name, action: shortcut.name } });
            }
        }
    }
    claims
}

/// Pure: any key claimed more than once is an error naming both claimants. A colliding key would
/// otherwise leave one claimant silently unreachable — `tools()`'s own `find()`-based lookup and
/// `consume_generic_chords`'s dispatch both resolve in table/claim order, so the loser never fires.
/// Called from `build_tools`, which panics on `Err` — a key collision in a compile-time, in-repo
/// plugin system is a programmer error, not a user-recoverable condition.
pub(super) fn validate_key_claims(claims: &[KeyClaim]) -> Result<(), String> {
    for (i, a) in claims.iter().enumerate() {
        for b in &claims[i + 1..] {
            if a.key == b.key {
                return Err(format!("key collision on {:?}: {} vs {}", a.key, a.source, b.source));
            }
        }
    }
    Ok(())
}

/// The fixed, ordered list of plugin descriptors — the single list every consumer (`build_tools`'s
/// description harvest via `(d.tools)()`, `with_state`'s retained-instance construction via
/// `(d.make)()`, and `key_claims`'s shortcut harvest via `(d.shortcuts)()`) reads. A `ToolDef` row's
/// `plugin_slot` is the index into this same slice, so "every consumer iterates in the same order"
/// is now structurally guaranteed rather than a convention two separately-written functions have to
/// uphold by hand — there is no second, independently-iterated list left to drift from this one.
pub(crate) const PLUGINS: &[gascii_plugin_api::PluginDescriptor] =
    &[gascii_density_brush::DESCRIPTOR, gascii_anim::DESCRIPTOR];

/// Folds every plugin's `wrap_renderer` over the host's own `NaiveRenderer`, innermost (the host's)
/// first, in iteration order. A pure function of the plugins it's given — an iterator rather than
/// `&GasciiApp`, so `rebuild_renderer`'s enabled filter composes in without allocating and tests
/// can feed a synthetic list with no live app.
pub(crate) fn build_renderer<'a>(plugins: impl Iterator<Item = &'a dyn Plugin>) -> Box<dyn CanvasRenderer> {
    plugins.fold(Box::new(NaiveRenderer) as Box<dyn CanvasRenderer>, |r, p| p.wrap_renderer(r))
}

/// Merges one plugin-contributed capability bundle into a full `ToolDef` row. Identity is now the
/// bundle's own name, wrapped in `ToolKind::Plugin` — the host mints no separate identifier and
/// keeps no name-to-kind table; `stamp_slot` is left `None` here and filled by `assign_stamp_slots`.
pub(super) fn merge_plugin_row(plugin_slot: usize, cap: &gascii_plugin_api::PluginToolCapabilities) -> ToolDef {
    ToolDef {
        kind: ToolKind::Plugin(cap.name),
        name: cap.name,
        key: cap.key,
        tip: cap.tip,
        make: cap.make,
        icon: cap.icon,
        sized: cap.sized,
        stamp_slot: None,
        holds_session: cap.holds_session,
        shows_hover: cap.shows_hover,
        stamps_glyph: cap.stamps_glyph,
        suppresses_shortcuts: cap.suppresses_shortcuts,
        kiosk_visible: cap.kiosk_visible,
        plugin_slot: Some(plugin_slot),
        pressure_sizeable: cap.pressure_sizeable,
        wants_ctx_patch: cap.wants_ctx_patch,
    }
}

/// The process-global tool registry: lazily built from `build_tools` on first read.
static TOOL_REGISTRY: std::sync::OnceLock<Vec<ToolDef>> = std::sync::OnceLock::new();

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
