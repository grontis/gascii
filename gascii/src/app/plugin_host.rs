//! The app's `PluginHost` implementation and the per-frame plugin panel draw/drain cycle.

use gascii_core::Document;

use super::GasciiApp;

/// A `PluginHost` snapshot, built fresh at each call site rather than implemented directly on
/// `GasciiApp`. `options_ui`/`tick`/`panel` need `&mut self.plugins[i]` (or to iterate
/// `self.plugins`) at the same call site that would otherwise need `&GasciiApp` too if `PluginHost`
/// were implemented on the app type directly — a field-level double-borrow the compiler rejects.
/// Carries a live `&Document` alongside three `Copy` facts (`stylus_detected`, `bound`,
/// `top_edit_id`) — built from individual field expressions (`&self.doc`, `self.history.
/// top_edit_id()`, never `&GasciiApp` or `self` as a whole) so the borrow it holds is scoped to
/// just `self.doc`, disjoint from `self.plugins`, which every one of this type's call sites
/// immediately borrows mutably afterward. Passing `&GasciiApp` here instead would tie the returned
/// value's lifetime to the *whole* struct, conflicting with every one of those mutable borrows.
pub(crate) struct HostFacts<'a> {
    doc: &'a Document,
    stylus_detected: bool,
    bound: [&'static str; 2],
    top_edit_id: Option<u64>,
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

    fn top_edit_id(&self) -> Option<u64> {
        self.top_edit_id
    }
}

/// Builds a `HostFacts` from an explicit `&Document` plus the app-level facts — never from
/// `&GasciiApp` (see `HostFacts`'s own doc comment for why). `top_edit_id` is `self.history.
/// top_edit_id()`, read at the same call site as the other two facts.
pub(crate) fn host_facts<'a>(
    doc: &'a Document,
    stylus_detected: bool,
    bound: [&'static str; 2],
    top_edit_id: Option<u64>,
) -> HostFacts<'a> {
    HostFacts {
        doc,
        stylus_detected,
        bound,
        top_edit_id,
    }
}

/// The `stylus_detected`/`bound` half of `host_facts`'s arguments, computed from `app` in one
/// place. Takes `&GasciiApp` and returns owned data only — its borrow of `app` ends the moment it
/// returns, before the caller separately, disjointly borrows `app.doc`/`app.plugins`.
pub(crate) fn host_context(app: &GasciiApp) -> (bool, [&'static str; 2]) {
    (
        app.stylus_detected,
        [
            super::tool_def(app.slot(super::Binding::L).kind).name,
            super::tool_def(app.slot(super::Binding::R).kind).name,
        ],
    )
}

impl GasciiApp {
    /// True while any enabled plugin reports `Plugin::blocks_editing` (animation playback): the
    /// canvas is showing something other than the editing cursor's frame, so edit-initiation
    /// seams refuse rather than landing changes on a frame the user can't currently see. Polled
    /// live — no cached flag to go stale between frames.
    pub(crate) fn editing_blocked(&self) -> bool {
        self.plugins.iter().enumerate().any(|(i, p)| {
            self.plugin_runtime.get(i).is_none_or(|r| r.enabled) && p.blocks_editing()
        })
    }

    /// The standard refusal at an edit-initiation seam: flashes the shared message and reports
    /// whether the caller must bail — mirrors the hidden-layer stroke gate's flash-and-refuse
    /// shape.
    pub(crate) fn refuse_edit_during_playback(&mut self) -> bool {
        if self.editing_blocked() {
            self.flash_error("Playback is running — pause to edit");
            return true;
        }
        false
    }

    /// Draws every plugin's panel, then applies whatever `PanelOutcome`s they returned. Two passes
    /// — draw-and-collect, then drain — because `apply_edit` needs the whole of `&mut self`, which
    /// would conflict with `self.plugins`'s mutable borrow while the draw loop is still running.
    /// `host`'s borrow of `self.doc` ends at its last use inside the loop (NLL), before the drain
    /// pass's `&mut self` calls. Called with the host's own live root `Ui` — see `Plugin::panel`'s
    /// doc comment for why a plain `Context` cannot substitute for this. A returned `PanelOutcome
    /// ::error` is surfaced via `flash_error` — the same status-bar channel every other
    /// structural trigger already uses (`add_frame_via_menu`, "Resize Canvas…"), so a plugin-
    /// originated failure reads identically to a host-originated one.
    pub(super) fn run_plugin_panels(&mut self, ui: &mut eframe::egui::Ui, kiosk: bool) {
        let (stylus_detected, bound) = host_context(self);
        let host = host_facts(
            &self.doc,
            stylus_detected,
            bound,
            self.history.top_edit_id(),
        );
        let mut outcomes = Vec::with_capacity(self.plugins.len());
        // A disabled plugin's panel is simply never declared — immediate mode reclaims its space
        // the same frame. `runtime` borrows only `self.plugin_runtime`, disjoint from `plugins`.
        let runtime = &self.plugin_runtime;
        for (i, p) in self.plugins.iter_mut().enumerate() {
            if runtime.get(i).is_some_and(|r| !r.enabled) {
                continue;
            }
            outcomes.push(p.panel(ui, kiosk, &host));
        }
        // Section tracking: on a press frame, the frames-section keys arm iff some panel reported
        // the press inside itself — a press anywhere else (canvas, sidebar, menu) disarms.
        if ui.input(|i| i.pointer.primary_pressed()) {
            self.frames_section_armed = outcomes.iter().any(|o| o.pressed_inside);
        }
        self.drain_panel_outcomes(outcomes);
    }

    /// Applies every `PanelOutcome` a plugin's `panel` or `tick` returned this frame, in order —
    /// the drain half of the two-pass draw-then-drain (or tick-then-drain) shape `run_plugin_panels`
    /// and `handle_keys` both need: `apply_edit`/`switch_active_frame` need the whole of `&mut self`,
    /// which would conflict with `self.plugins`'s still-live mutable borrow if called from inside
    /// the draw/tick loop itself. A returned `PanelOutcome::error` is surfaced via
    /// `flash_error` — the same status-bar channel every other structural trigger already uses
    /// (`add_frame_via_menu`, "Resize Canvas…"), so a plugin-originated failure reads identically to
    /// a host-originated one.
    pub(super) fn drain_panel_outcomes(&mut self, outcomes: Vec<gascii_plugin_api::PanelOutcome>) {
        for outcome in outcomes {
            for edit in outcome.edits {
                self.apply_edit(edit, None);
            }
            for prop in outcome.properties {
                match prop {
                    gascii_plugin_api::DocProperty::ActiveFrame(idx) => {
                        self.switch_active_frame(idx)
                    }
                    gascii_plugin_api::DocProperty::ActiveLayer(idx) => {
                        self.switch_active_layer(idx)
                    }
                    gascii_plugin_api::DocProperty::LoopPlayback(loop_playback) => {
                        // A plain field write, not an `Edit` — matches `Document.loop_playback`'s
                        // own "set-and-forget, never History-tracked" contract (see
                        // `DocProperty::LoopPlayback`'s doc comment). `is_dirty()` covers this via
                        // the saved session-meta snapshot (see `saved_loop_playback`'s doc
                        // comment) — no separate "mark dirty" call needed.
                        self.doc.loop_playback = loop_playback;
                    }
                    gascii_plugin_api::DocProperty::DefaultFrameDuration(duration_ms) => {
                        // Same shape as `LoopPlayback` above — a plain field write, never
                        // `History`-tracked (see `DocProperty::DefaultFrameDuration`'s doc
                        // comment).
                        self.doc.frame_duration_ms = duration_ms;
                    }
                }
            }
            if let Some(msg) = outcome.error {
                self.flash_error(msg);
            }
        }
    }
}
