use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use eframe::egui;
use gascii_core::{
    builtin_pages, export_text, CellPatch, Document, History, Page, PlaneMask, ResizeAnchor, Rgba,
    WidthReject, MAX_TOOL_SIZE,
};

use gascii_plugin_api::{CanvasRenderer, Plugin};

use crate::canvas;
use crate::chords::{self, ChordDispatch, ChordId};
use crate::fonts;
use crate::image_bg;
use crate::prefs;
use crate::viewport::Viewport;

mod dialogs;
mod files;
mod plugin_host;
mod session;
mod tools_registry;
pub(crate) use dialogs::*;
use files::*;
pub(crate) use plugin_host::*;
pub(crate) use session::*;
pub(crate) use tools_registry::*;

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
/// so this has three other claims on Escape to check: an active Text/Selection session (ends on
/// its own Escape handling inside `canvas.rs`), a live pointer stroke (exiting fullscreen mid-drag
/// would yank the canvas out from under the pointer), and a focused widget — egui's own popups
/// close on Escape at draw time, which runs after `handle_keys`, so consuming the key here would
/// swallow it before a popup (the hex color field, say) ever gets a chance to react to it.
fn should_handle_escape_for_fullscreen(
    keyboard_owner: Option<Binding>,
    stroke_in_progress: bool,
    widget_focused: bool,
) -> bool {
    keyboard_owner.is_none() && !stroke_in_progress && !widget_focused
}

/// Whether `kind`'s single-letter shortcut should be reachable from the keyboard this frame. A
/// shortcut is only reachable while fullscreen if its tool has a cell in kiosk's grid
/// (`kiosk_visible`) — otherwise the keypress would silently rebind L to a tool with no cell and
/// no other on-screen trace of what changed. Today that gates exactly Eyedropper (`I`), whose
/// kiosk role Alt+click's temporary sample covers without a tool switch.
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
fn tool_shortcut_fires_and_consumes_its_key(
    def: &ToolDef,
    i: &mut egui::InputState,
    is_fullscreen: bool,
) -> bool {
    tool_shortcut_reachable(def.kind, is_fullscreen)
        && i.consume_key(egui::Modifiers::NONE, def.key)
}

/// One plugin's per-app runtime state — `plugins`' enabled/resume sidecar, indexed identically.
/// A separate parallel `Vec` rather than a wrapper around the instance so `ToolDef.plugin_slot`
/// keeps indexing `plugins` directly, and a bare test double pushed onto `plugins` alone stays
/// legal (it reads as enabled — see `plugin_enabled`).
pub(crate) struct PluginRuntime {
    pub(crate) enabled: bool,
    /// Latched at re-enable, consumed by the plugin's first tick after — delivered as
    /// `resumed_after_suppression`, the same stale-hold-state reset the modal-suppression latch
    /// delivers globally. Without it, a prefs- or test-driven toggle would skip the reset the
    /// `Plugin::tick` contract promises.
    pub(crate) resume_pending: bool,
}

/// How long the status bar shows an `ErrorFlash` before it disappears on its own.
pub(crate) const ERROR_FLASH_TTL: std::time::Duration = std::time::Duration::from_secs(3);

/// One error message plus the moment it was raised. The status bar stops showing it
/// `ERROR_FLASH_TTL` after that moment (`GasciiApp::error_flash`); dialog-inline validation reads
/// `text` directly and keeps it until the dialog resolves or clears it.
pub(crate) struct ErrorFlash {
    pub(crate) text: String,
    pub(crate) at: std::time::Instant,
}

pub struct GasciiApp {
    pub(crate) doc: Document,
    pub(crate) viewport: Viewport,
    pub(crate) hovered_cell: Option<(u16, u16)>,
    pub(crate) renderer: Box<dyn CanvasRenderer>,
    /// One retained instance per `PLUGINS` entry, in the same order — constructed via `(d.make)()`.
    /// `build_tools` never constructs a plugin instance at all; it reads descriptions straight off
    /// `PLUGINS` via `(d.tools)()`. A `ToolDef.plugin_slot` indexes into this same slice.
    pub(crate) plugins: Vec<Box<dyn Plugin>>,
    /// One entry per `PLUGINS` descriptor, same order — see `PluginRuntime`. Production apps never
    /// grow it past `PLUGINS.len()`; `push_plugin_double` extends both vecs together for tests.
    pub(crate) plugin_runtime: Vec<PluginRuntime>,
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
    /// and `resync_slots`' resync target all source it from here, mirroring `active_frame` exactly.
    ///
    /// Kept in sync with `doc.active_layer()` at every `History` choke point, in both directions:
    /// `apply_edit` seeds `doc`'s cursor from this field before every `History::apply` (app -> doc,
    /// since a caller-built `Edit` targets a specific layer that must already be `doc`'s active one
    /// before it applies), then reads it back afterward (doc -> app), because `AddLayer`/
    /// `RemoveLayer`/`ReorderLayer` shift `doc`'s cursor as a side effect of applying — independent
    /// of whatever was just seeded. `request_undo`/`request_redo` mutate `doc` directly (bypassing
    /// `apply_edit`) and restore `doc.active_layer()` from the `Edit`'s own baked-in snapshot, so
    /// they resync this field the same way afterward. `doc.active_layer()` is the ground truth;
    /// this field only ever leads at the one seed point in `apply_edit`, and follows everywhere
    /// else — see `switch_active_layer` for the direct-selection path.
    pub(crate) active_layer: usize,
    /// The frame every tool reads and writes: `tool_ctx`'s `ToolCtx.frame` and `resync_slots`'
    /// resync target both source it from here, mirroring `active_layer` exactly.
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
    /// Which of the New/Resize/Export/Help dialogs is showing, if any — at most one at a time, by
    /// construction (a single field, not four independent flags). Session-only, never persisted.
    /// Counted by `modal_open()`: `canvas.rs` polls raw pointer/keyboard state rather than using
    /// egui's occlusion system, so without this a click "through" a dialog would still draw on the
    /// canvas underneath it.
    open_dialog: Option<OpenDialog>,
    /// True once this session has observed a pressure-bearing `Event::Touch` (a stylus contact).
    /// Session-only, never persisted. A device-capability fact, not Brush-owned state — it only
    /// happens to gate the Pressure toggle's visibility in Brush's options block, exposed to
    /// plugins read-only via `PluginHost::stylus_detected`.
    pub(crate) stylus_detected: bool,
    /// True from a barrel-button press until its matching release. `gascii_stylus::barrel_down`
    /// only reads true while the button is physically held, and Windows clears it before the
    /// release event reaches egui — so `raw_input_hook` latches at press time to keep both halves
    /// of the pair routed to the secondary button.
    pub(crate) barrel_stroke: bool,
    /// Accumulated multiplicative pinch-zoom delta since the last discrete zoom step fired.
    /// `multi_touch()`'s `zoom_delta` is a per-frame ratio (1.0 = no change), not a cumulative
    /// gesture magnitude, so this multiplies frame deltas together until they cross a threshold —
    /// see the pinch-zoom handling in `canvas.rs` for why a per-frame trigger would be too twitchy
    /// against the 6-step discrete `ZOOM_SCALES` model.
    pub(crate) pinch_zoom_accum: f32,
    resize_w: u16,
    resize_h: u16,
    /// The 3x3 anchor the Resize dialog is currently set to. Remembered for the session (not
    /// persisted across restarts) — each resize starts from whatever the last one used.
    pub(crate) resize_anchor: ResizeAnchor,
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
    pub(crate) last_error: Option<ErrorFlash>,
    /// Whether the last primary press landed inside the animation panel (`PanelOutcome::
    /// pressed_inside`) — while armed (and no session owns the keyboard), Ctrl+D/Delete/Copy/Paste
    /// act on frames instead of the canvas selection. A press anywhere else disarms.
    pub(crate) frames_section_armed: bool,
    /// The frames-section clipboard: `copy_active_frame`'s snapshot, `paste_frame`'s source.
    /// App-internal — a frame is structured cell data, not text, so the OS clipboard is not
    /// involved.
    pub(crate) frame_clipboard: Option<gascii_core::Frame>,
    /// The undo-stack edit id (`History::top_edit_id`) at the moment of the last successful save
    /// or load — `None` matches a fresh `History`'s own sentinel. `is_dirty` is a pure comparison
    /// against `self.history.top_edit_id()`; nothing else needs to know about this field.
    saved_marker: Option<u64>,
    /// `Document.loop_playback` at the same checkpoint `saved_marker` is captured at (save, load,
    /// New). Loop is a plain, non-`Edit`-tracked field write (`DocProperty::LoopPlayback`), so
    /// `saved_marker` alone — which only tracks `History`'s undo stack — can never notice it
    /// changed; `is_dirty` compares both.
    saved_loop_playback: bool,
    /// `Document.frame_duration_ms` at the same checkpoint, for the identical reason
    /// (`DocProperty::DefaultFrameDuration`).
    saved_frame_duration_ms: u32,
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
    /// Latches `true` the moment `ui()` skips `handle_keys` (and therefore every plugin's `tick`)
    /// because a modal is open; delivered once to every plugin's `tick` as
    /// `resumed_after_suppression`, then cleared, on the next frame `handle_keys` actually runs.
    /// See `Plugin::tick`'s own doc comment for why a plugin needs to know this at all.
    plugin_ticks_suppressed: bool,
    /// The title last pushed to the OS, so it is only sent when it changes.
    shown_title: String,
    /// Process start time, read once by the first frame's debug-only startup timing log.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
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

/// Which of the New/Resize/Export/Help/Plugins dialogs is showing, tagging
/// `GasciiApp::open_dialog`. Replaces independent `_open: bool` flags with one field —
/// `modal_open()` no longer has to enumerate them by hand, and opening a dialog structurally
/// replaces whatever was open before rather than leaving a stale flag set alongside it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpenDialog {
    New,
    Resize,
    Export,
    Help,
    Plugins,
}

impl GasciiApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        started: Instant,
        launch_fullscreen: bool,
    ) -> Self {
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
        // Same slice `build_tools` reads descriptions from — see `PLUGINS`'s own doc comment for
        // why the two can no longer iterate it differently.
        let plugins: Vec<Box<dyn Plugin>> = PLUGINS.iter().map(|d| (d.make)()).collect();
        let plugin_runtime: Vec<PluginRuntime> = PLUGINS
            .iter()
            .map(|_| PluginRuntime {
                enabled: true,
                resume_pending: false,
            })
            .collect();
        let renderer = build_renderer(plugins.iter().map(|p| p.as_ref()));
        let doc = Document::default_document();
        // Mirrors `saved_marker`'s own "start clean" contract, read from the fresh `doc` itself
        // rather than hardcoded, so this stays correct if `Document::default_document`'s starting
        // values ever change.
        let saved_loop_playback = doc.loop_playback;
        let saved_frame_duration_ms = doc.frame_duration_ms;
        Self {
            doc,
            viewport: Viewport::default(),
            hovered_cell: None,
            renderer,
            plugins,
            plugin_runtime,
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
            slots: [
                ToolSlot::new(ToolKind::Pencil),
                ToolSlot::new(ToolKind::Eraser),
            ],
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
            open_dialog: None,
            stylus_detected: false,
            barrel_stroke: false,
            pinch_zoom_accum: 1.0,
            resize_w: Document::DEFAULT_WIDTH,
            resize_h: Document::DEFAULT_HEIGHT,
            resize_anchor: ResizeAnchor::default(),
            new_w: Document::DEFAULT_WIDTH,
            new_h: Document::DEFAULT_HEIGHT,
            new_bg: Rgba(0, 0, 0, 255),
            image_bg: None,
            image_bg_gen: 0,
            export: ExportSettings::default(),
            export_preview: None,
            export_preview_key: None,
            current_path: None,
            recent_files: Vec::new(),
            last_error: None,
            frames_section_armed: false,
            frame_clipboard: None,
            saved_marker: None,
            saved_loop_playback,
            saved_frame_duration_ms,
            confirm: None,
            force_close: false,
            ctrl_c_seen: 0,
            plugin_ticks_suppressed: false,
            shown_title: String::new(),
            started,
            first_frame: true,
        }
    }

    /// True while any modal dialog is showing. `canvas.rs` polls raw pointer/keyboard state rather
    /// than using egui's occlusion system, so a modal's backdrop alone does not block it — every
    /// raw-input-polling site in `canvas.rs`/`handle_keys` must gate on this rather than any single
    /// dialog's own state. Structural now that `open_dialog` is one field: nothing to forget here
    /// when a new dialog is added, unlike the four-independent-bool shape this replaced.
    pub(crate) fn modal_open(&self) -> bool {
        self.open_dialog.is_some() || self.confirm.is_some()
    }

    /// Whether any pointer gesture — primary stroke or right-click stroke — currently owns the
    /// canvas.
    pub(crate) fn stroke_in_progress(&self) -> bool {
        self.stroke_owner.is_some()
    }

    pub(crate) fn slot(&self, b: Binding) -> &ToolSlot {
        &self.slots[b.ix()]
    }

    /// Whether the plugin at `i` is enabled. Out-of-range — a test double pushed onto `plugins`
    /// alone — reads as enabled: a double has no descriptor row and nothing to toggle.
    pub(crate) fn plugin_enabled(&self, i: usize) -> bool {
        self.plugin_runtime.get(i).is_none_or(|r| r.enabled)
    }

    /// The one enabled-filter every tool-row consumer goes through: a built-in row is always
    /// enabled, a plugin row follows its plugin's toggle. Filtering anywhere else would let two
    /// consumers drift on what "available" means.
    pub(crate) fn tool_enabled(&self, kind: ToolKind) -> bool {
        tool_def(kind)
            .plugin_slot
            .is_none_or(|i| self.plugin_enabled(i))
    }

    /// The registry minus disabled plugins' rows — what the toolbox and kiosk grids draw. Owned:
    /// `ToolDef` is `Copy` and both grids need `&mut GasciiApp` alongside the list.
    pub(crate) fn active_tools(&self) -> Vec<ToolDef> {
        tools()
            .iter()
            .copied()
            .filter(|d| self.tool_enabled(d.kind))
            .collect()
    }

    /// Whether gascii-anim is enabled — gates the menu bar's Animation menu: a second frame
    /// with no timeline to manage it would be stranded. Resolved by descriptor id, not a hardcoded
    /// index, and reads as enabled were the plugin ever not registered at all (Add Frame is
    /// host-owned; only the *management* UI is the plugin's).
    pub(crate) fn anim_plugin_enabled(&self) -> bool {
        PLUGINS
            .iter()
            .position(|d| d.id == gascii_anim::DESCRIPTOR.id)
            .is_none_or(|i| self.plugin_enabled(i))
    }

    /// Toggles the plugin at `i`, effective immediately. Disabling rebinds any binding holding one
    /// of its tools to Pencil — together with `set_tool`'s own enabled guard this makes "a disabled
    /// plugin's tool is never bound" structural, which is why `tool_ctx_patch`, the pressure gate,
    /// and the sidebar's `options_ui` block need no gating of their own. The live instance is kept,
    /// not dropped: unpersisted plugin state (a ramp choice, a playback position) survives the
    /// toggle, and `resume_pending` hands the plugin its tick-contract reset at re-enable instead.
    ///
    /// The fallback rebind rides on `bind`, whose stroke guard could in principle swallow it — but
    /// toggling only happens inside the Plugins modal, and `canvas.rs` gates all raw pointer input
    /// on `modal_open()`, so no stroke can be live here.
    pub(crate) fn set_plugin_enabled(&mut self, i: usize, enabled: bool) {
        let Some(rt) = self.plugin_runtime.get(i) else {
            return;
        };
        if rt.enabled == enabled {
            return;
        }
        if enabled {
            self.plugin_runtime[i].enabled = true;
            self.plugin_runtime[i].resume_pending = true;
        } else {
            for b in Binding::ALL {
                if tool_def(self.slot(b).kind).plugin_slot == Some(i) {
                    self.bind(b, ToolKind::Pencil);
                }
            }
            self.plugin_runtime[i].enabled = false;
        }
        self.rebuild_renderer();
    }

    /// Rebuilds the canvas renderer from the enabled plugins only — `build_renderer`'s fold is a
    /// pure function of the list it's given, so this is the whole cost of a toggle. `runtime` and
    /// `self.plugins` are disjoint field borrows; routing the filter through `plugin_enabled`
    /// instead would borrow all of `self` inside the closure and conflict with `iter()`'s own.
    fn rebuild_renderer(&mut self) {
        let runtime = &self.plugin_runtime;
        let renderer = build_renderer(
            self.plugins
                .iter()
                .enumerate()
                .filter(|(i, _)| runtime.get(*i).is_none_or(|r| r.enabled))
                .map(|(_, p)| p.as_ref()),
        );
        self.renderer = renderer;
    }

    /// Test-only: pushes a plugin double with a matching runtime entry, so a test can toggle it by
    /// the returned index. A bare push onto `plugins` alone stays legal — it reads as always
    /// enabled, untoggleable.
    #[cfg(test)]
    pub(crate) fn push_plugin_double(&mut self, p: Box<dyn Plugin>) -> usize {
        self.plugins.push(p);
        self.plugin_runtime.push(PluginRuntime {
            enabled: true,
            resume_pending: false,
        });
        self.plugins.len() - 1
    }

    /// Test-only downcast into the live `BrushPlugin` instance, for tests that need to drive or
    /// inspect its own state (ramp/mode/pressure) directly rather than only through the `Plugin`
    /// trait's narrow surface — e.g. confirming it survives being rendered through two different
    /// chrome geometries unchanged. Not a production access path: nothing outside tests should ever
    /// need a concrete plugin type back out of `Box<dyn Plugin>`.
    #[cfg(test)]
    pub(crate) fn brush_plugin_mut(&mut self) -> &mut gascii_density_brush::BrushPlugin {
        let i = tool_def(BRUSH_KIND)
            .plugin_slot
            .expect("Brush is plugin-sourced");
        self.plugins[i]
            .as_any_mut()
            .downcast_mut()
            .expect("plugin at Brush's slot must be BrushPlugin")
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
        // A disabled plugin's tool is unbindable from every path (toolbox click, shortcut, prefs
        // replay) — the other half, alongside `set_plugin_enabled`'s fallback rebind, of the
        // invariant that a disabled plugin's tool is never bound.
        if self.stroke_in_progress() || !self.tool_enabled(kind) {
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
        self.flash_error(format!("typed {ch:?} rejected: {why}"));
    }

    /// Raises a status-bar error, stamping the moment it was raised so `error_flash` can expire it.
    pub(crate) fn flash_error(&mut self, text: impl Into<String>) {
        self.last_error = Some(ErrorFlash {
            text: text.into(),
            at: std::time::Instant::now(),
        });
    }

    /// The status bar's view of `last_error`: the text plus the time left before it expires,
    /// `None` once `ERROR_FLASH_TTL` has passed. Takes `now` rather than reading the clock so the
    /// expiry decision is testable without waiting out the TTL.
    pub(crate) fn error_flash(
        &self,
        now: std::time::Instant,
    ) -> Option<(&str, std::time::Duration)> {
        let flash = self.last_error.as_ref()?;
        let elapsed = now.saturating_duration_since(flash.at);
        if elapsed >= ERROR_FLASH_TTL {
            return None;
        }
        Some((flash.text.as_str(), ERROR_FLASH_TTL - elapsed))
    }

    /// `last_error`'s bare text, without the status bar's expiry rule. Production reads inside
    /// dialog closures use the field directly (`&self.last_error` borrows one field; a method call
    /// would borrow all of `self` and collide with the closure's other captures).
    #[cfg(test)]
    pub(crate) fn last_error_text(&self) -> Option<&str> {
        self.last_error.as_ref().map(|e| e.text.as_str())
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
        // One-shot: `true` only on this call if one or more prior frames skipped `handle_keys`
        // entirely because a modal was open (see `ui()`'s own `modal_open()` gate, the only place
        // `plugin_ticks_suppressed` is ever set). Delivered to every plugin's `tick` once below,
        // then cleared here regardless of what any plugin does with it — a plugin holding
        // cross-frame hold state (gascii-anim's Space hold) needs to tell "I was just suppressed"
        // apart from "nothing happened," the same way it already needs to tell OS focus loss apart
        // from an ordinary frame.
        let resumed_after_suppression = self.plugin_ticks_suppressed;
        self.plugin_ticks_suppressed = false;
        let owner_kind = self.keyboard_owner().map(|b| self.slot(b).kind);
        let widget_focused = ui.memory(|m| m.focused().is_some());
        let focused = widget_focused || suppresses_tool_shortcuts(owner_kind);
        let is_fullscreen = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        // Which section Ctrl+D/Delete/Copy/Paste act on: the frames section (armed by the last
        // press landing inside the animation panel) or the canvas. A keyboard-owning session (a
        // live marquee, a Text burst) always outranks the section — an explicit selection's
        // duplicate/delete/copy beats the implicit "last clicked here" state.
        let frames_mode =
            self.frames_section_armed && self.keyboard_owner().is_none() && !widget_focused;
        let (
            redo_shift,
            undo,
            redo_y,
            select_all,
            copy,
            copy_all,
            cut,
            duplicate,
            delete_frame,
            paste_frame,
            generic_always,
        ) = ui.input_mut(|i| {
            // Cmd/Ctrl+Shift+Z must be consumed before the plain Cmd/Ctrl+Z pattern, since
            // `matches_logically` ignores extra Shift/Alt — checking undo first would swallow
            // the redo shortcut's Z key press. Same reasoning for Ctrl+Shift+C vs plain Ctrl+C.
            //
            // Ctrl+A joins this same `widget_focused`-gated group, not the uniform generic
            // subset: egui::TextEdit's own cursor handling treats Ctrl+A as "select all text in
            // this field" while focused (confirmed against the vendored
            // `text_selection/cursor_range.rs`), the same conflict that put Undo/Redo here in
            // the first place. A canvas Text session sets no widget focus, so Ctrl+A there still
            // reaches the Selection-tool chord below.
            //
            // Copy/CopyAll/Cut join the same gate for the identical reason: a focused
            // `TextEdit` (the hex color popup, which lives outside `modal_open()`'s coverage)
            // reads `Event::Copy`/`Event::Cut` off this frame's event list too, via its own
            // `filtered_events` — cloned, not consumed, so an unguarded scan here would fire
            // `copy_selection`/`cut_selection` on the canvas alongside the field's own cut/copy
            // of its selected text. Duplicate (Ctrl+D) joins it too: it flushes pending work
            // and spawns a float, so a Ctrl+D typed into a focused popup must not reach the
            // canvas and rewrite the session underneath it.
            let (redo_shift, undo, redo_y, select_all, copy, copy_all, cut, duplicate) =
                if widget_focused {
                    (false, false, false, false, false, false, false, false)
                } else {
                    let (copy, copy_all) = copy_events(&i.events, i.modifiers.shift);
                    (
                        i.consume_key(
                            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                            egui::Key::Z,
                        ),
                        i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z),
                        i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y),
                        i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                        copy,
                        copy_all,
                        cut_event(&i.events),
                        i.consume_key(egui::Modifiers::COMMAND, egui::Key::D),
                    )
                };
            // The uniform, unconditional subset — one shared consume-and-dispatch loop over
            // `chords::CHORDS`'s `GenericAlways` rows in table order, rather than one
            // near-identical individual `consume_key` call per chord. None of these collide with
            // each other or with anything above, so table order carries no precedence weight
            // here — it exists only because the registry always consumes in table order, the
            // same rule `tools()`'s own shortcut lookup already follows.
            // `Ctrl+Shift+=` (the same physical key as `Ctrl+=`) produces `Key::Plus` on US
            // layouts rather than `Key::Equals` — folded into `ZoomInAlias`'s own CHORDS row as
            // a second key pattern (D-7), so the generic loop above already consumes it; no
            // hand-written second `consume_key` call needed here anymore.
            // Frames-mode-only keys: Delete removes the active frame, and `Event::Paste` is
            // consumed (retain), not merely read — canvas.rs reads Paste un-consumed later
            // this same frame, and without the removal a frames-mode paste would ALSO land as
            // a floating text stamp on the canvas.
            let (delete_frame, paste_frame) = if frames_mode {
                let del = i.consume_key(egui::Modifiers::NONE, egui::Key::Delete);
                let mut pasted = false;
                i.events.retain(|e| {
                    if matches!(e, egui::Event::Paste(_)) {
                        pasted = true;
                        return false;
                    }
                    true
                });
                (del, pasted)
            } else {
                (false, false)
            };
            let generic_always = chords::consume_generic_chords(i, ChordDispatch::GenericAlways);
            (
                redo_shift,
                undo,
                redo_y,
                select_all,
                copy,
                copy_all,
                cut,
                duplicate,
                delete_frame,
                paste_frame,
                generic_always,
            )
        });
        let save = generic_always.contains(&ChordId::Save);
        let export_dialog = generic_always.contains(&ChordId::ExportDialog);
        let fit = generic_always.contains(&ChordId::Fit);
        let new_document = generic_always.contains(&ChordId::New);
        let open_document = generic_always.contains(&ChordId::Open);
        let save_as = generic_always.contains(&ChordId::SaveAs);
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
        // The enabled check runs before the consuming predicate, so a disabled tool's key event is
        // left untouched — the same leave-unconsumed shape Text-in-kiosk uses.
        if !focused {
            let picked = ui.input_mut(|i| {
                tools()
                    .iter()
                    .find(|def| {
                        self.tool_enabled(def.kind)
                            && tool_shortcut_fires_and_consumes_its_key(def, i, is_fullscreen)
                    })
                    .map(|def| def.kind)
            });
            if let Some(kind) = picked {
                self.set_tool(Binding::L, kind);
            }
        }
        // Escape's lowest-priority claim: exiting fullscreen. Only reachable when nothing with a
        // higher claim on Escape is live (a Text/Selection session, a pointer stroke, a focused
        // widget) — those are handled elsewhere (`canvas.rs`'s own Escape branches, which run on
        // the same frame's raw events and are unaffected by this `consume_key` since it only fires
        // when they wouldn't have claimed the key anyway) — and only consumed when it will actually
        // do something: consuming it in a windowed session, or while a widget popup still needs a
        // shot at it, would swallow the key for no effect at all.
        if should_handle_escape_for_fullscreen(
            self.keyboard_owner(),
            self.stroke_in_progress(),
            widget_focused,
        ) && is_fullscreen
        {
            let want_exit =
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            if want_exit {
                let ctx = ui.ctx().clone();
                self.toggle_fullscreen(&ctx);
            }
        }
        // F11 is genuinely unconditional — no `focused` gate. Nothing else in the app binds it, no
        // tool consumes it as content (a Text burst only ever sees `Event::Text`/`Char`), and
        // `handle_keys` itself only runs while `!modal_open()`, which is the one gate F11 does
        // need. Gating it on `focused` would silently swallow the toggle for the whole duration of
        // any Text session (`suppresses_tool_shortcuts` holds `focused` true for as long as
        // composing lasts), which is exactly the bug this is written to avoid. It's a toggle, so
        // key-repeat is ignored the same way the generic loop's toggle rows are — holding F11 down
        // must flip fullscreen once, not flicker it on every OS-generated repeat.
        let want_toggle = ui
            .input_mut(|i| chords::consume_key_no_repeat(i, egui::Modifiers::NONE, egui::Key::F11));
        if want_toggle {
            let ctx = ui.ctx().clone();
            self.toggle_fullscreen(&ctx);
        }
        if !focused {
            let fired = ui
                .input_mut(|i| chords::consume_generic_chords(i, ChordDispatch::GenericUnfocused));
            if fired.contains(&ChordId::SwapColors) {
                self.swap_colors();
            }
            if fired.contains(&ChordId::ToggleGrid) {
                self.show_grid = !self.show_grid;
            }
            if fired.contains(&ChordId::HelpOverlay) {
                self.open_dialog = if self.open_dialog == Some(OpenDialog::Help) {
                    None
                } else {
                    Some(OpenDialog::Help)
                };
            }
        }
        if copy_all {
            self.flush_all();
            ui.ctx().copy_text(export_text(&self.doc));
        } else if copy {
            if frames_mode {
                self.copy_active_frame(ui.ctx());
            } else {
                self.copy_selection(ui.ctx());
            }
        }
        if cut {
            self.cut_selection(ui.ctx());
        }
        if select_all {
            self.select_all();
        }
        if duplicate {
            if frames_mode {
                // The exact duplicate-active-frame action the Animation menu's Add Frame performs.
                self.add_frame_via_menu();
            } else {
                self.duplicate_selection();
            }
        }
        if delete_frame {
            self.delete_active_frame();
        }
        if paste_frame {
            self.paste_frame();
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
        // Disabled plugins are skipped outright — their clocks freeze and their shortcut dispatch
        // (which lives inside `tick`) goes silent with them. `runtime` and `self.plugins` are
        // disjoint field borrows, same shape as `rebuild_renderer`.
        let (stylus_detected, bound) = host_context(self);
        let host = host_facts(
            &self.doc,
            stylus_detected,
            bound,
            self.history.top_edit_id(),
        );
        let mut tick_outcomes = Vec::with_capacity(self.plugins.len());
        let runtime = &mut self.plugin_runtime;
        for (i, p) in self.plugins.iter_mut().enumerate() {
            if runtime.get(i).is_some_and(|r| !r.enabled) {
                continue;
            }
            // `mem::take` consumes the one-shot re-enable latch: read it and reset it to false in
            // a single move, so the reset signal fires on exactly one tick.
            let resumed = resumed_after_suppression
                || runtime
                    .get_mut(i)
                    .map(|r| std::mem::take(&mut r.resume_pending))
                    .unwrap_or(false);
            tick_outcomes.push(p.tick(ui, focused, resumed, &host));
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

    /// File ▸ New…'s body: flush first, then either open the New dialog directly (a clean document)
    /// or veto through the unsaved-changes confirm. Shared by the menu click and the `Ctrl+N` chord
    /// so the two can never drift apart.
    pub(crate) fn new_document_via_menu(&mut self) {
        self.flush_all();
        if self.is_dirty() {
            self.confirm = Some(PendingConfirm::NewDocument);
        } else {
            self.open_new_dialog();
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

    pub(crate) fn open_export_dialog(&mut self) {
        // Not the authoritative flush — harmless dialog-open convenience only. The dialog reads
        // `self.doc` again (via the preview and the final "Export…" click), which is what matters.
        self.flush_all();
        self.export_preview = None;
        self.export_preview_key = None;
        // An unrelated prior error must not read as if this fresh dialog already failed.
        self.last_error = None;
        self.open_dialog = Some(OpenDialog::Export);
    }

    /// Edit ▸ "Resize Canvas…"'s body: flush first (keeps the dialog's initial W/H consistent with
    /// whatever's about to be committed), seed the steppers from the current extent, and open the
    /// dialog. The menu's one mediator method, mirroring `open_export_dialog`'s shape.
    pub(crate) fn open_resize_dialog(&mut self) {
        self.flush_all();
        self.resize_w = self.doc.width;
        self.resize_h = self.doc.height;
        // An unrelated error from a prior action (e.g. a dead Recent Files entry) must not read as
        // if this fresh dialog already failed.
        self.last_error = None;
        self.open_dialog = Some(OpenDialog::Resize);
    }

    /// View ▸ "Plugins…"'s body — the menu's one mediator method, mirroring `open_resize_dialog`'s
    /// shape. No flush needed: the manager reads and toggles plugin state only, never the document.
    pub(crate) fn open_plugins_dialog(&mut self) {
        self.open_dialog = Some(OpenDialog::Plugins);
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

impl eframe::App for GasciiApp {
    /// Reroutes stylus barrel-button clicks to the secondary pointer button. Pen contacts arrive
    /// as touches, which egui-winit emulates as *primary* presses regardless of pen buttons; the
    /// message hook in `gascii-stylus` tracks the real barrel state, and rewriting here — before
    /// egui parses the frame's input — makes the tap an ordinary right click everywhere downstream.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        for event in &mut raw_input.events {
            if let egui::Event::PointerButton {
                button, pressed, ..
            } = event
            {
                if *button != egui::PointerButton::Primary {
                    continue;
                }
                if *pressed {
                    self.barrel_stroke = gascii_stylus::barrel_down();
                }
                if self.barrel_stroke {
                    *button = egui::PointerButton::Secondary;
                    if !*pressed {
                        self.barrel_stroke = false;
                    }
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.first_frame {
            #[cfg(debug_assertions)]
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
        // cosmetic pause with no other reported downside. `plugin_ticks_suppressed` latches while
        // skipped so the next real `handle_keys` call can tell every plugin's `tick` it just came
        // back from a suppression window, not merely an ordinary frame (see `Plugin::tick`'s doc
        // comment) — cross-frame key-hold state (a Space hold started before the modal opened, its
        // release swallowed by the modal's own event loop) would otherwise read the modal's close as
        // the original hold ending and fire a spurious toggle.
        if !self.modal_open() {
            self.handle_keys(ui);
        } else {
            self.plugin_ticks_suppressed = true;
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
                .frame(
                    egui::Frame::new()
                        .fill(t.bg_panel)
                        .inner_margin(egui::Margin::symmetric(12, 0)),
                )
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
                .frame(
                    egui::Frame::new()
                        .fill(t.bg_panel)
                        .inner_margin(egui::Margin::symmetric(8, 0)),
                )
                .exact_size(28.0)
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| crate::ui::menu::show(ui, self));
                });
            // The status bar is claimed BEFORE the sidebar, so it spans the full window width. Panels
            // take their slice in declaration order: sidebar-first would give the left panel the whole
            // remaining height and leave the status bar starting at x=208.
            egui::Panel::bottom("status")
                .frame(
                    egui::Frame::new()
                        .fill(t.bg_panel)
                        .inner_margin(egui::Margin::symmetric(12, 0)),
                )
                .exact_size(crate::ui::status_bar::HEIGHT)
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| crate::ui::status_bar::show(ui, self));
                });
            egui::Panel::left("sidebar")
                .frame(
                    egui::Frame::new()
                        .fill(t.bg_panel)
                        .inner_margin(egui::Margin::same(12)),
                )
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
        self.plugins_dialog(&ctx);

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
mod tests;
