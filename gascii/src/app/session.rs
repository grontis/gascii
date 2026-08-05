//! The flush/trigger machinery: applying edits, committing or ending a slot's cross-frame session,
//! undo/redo, and the selection/clipboard actions built on top of them. Every structural trigger in
//! the app (Save, Export, Resize, a dialog opening, a rebind) routes document access through this
//! subsystem rather than touching `self.doc`/`self.slots` directly.

use eframe::egui;
use gascii_core::{clear_document, duplicate_frame, CellEdit, CellPatch, Edit, FrameOpError, ToolEvent, ToolResponse};

use super::{make_tool, Binding, GasciiApp, ToolKind};

/// Whether the document has changed since the last save/load: true whenever the undo stack's
/// current top-edit id doesn't match the id recorded at that save/load. Pulled out as a pure
/// function, mirroring `is_own_clipboard_text`, so the comparison is unit-testable without a live
/// `GasciiApp`; `GasciiApp::is_dirty` is the thin method wrapping it.
pub(super) fn edit_marker_differs(current: Option<u64>, saved: Option<u64>) -> bool {
    current != saved
}

/// The binding a pasted float lands in: whichever is already bound to Selection (L wins if both),
/// else L, rebound.
///
/// Never R by default: a paste is a keyboard command, the keyboard's tool is L's, and silently
/// rebinding the right button out from under the user is worse than rebinding the left. Pure, so the
/// choice is testable without a `GasciiApp` (following `is_own_clipboard_text`'s precedent).
pub(super) fn paste_target(l: ToolKind, r: ToolKind) -> Binding {
    if l == ToolKind::Selection {
        Binding::L
    } else if r == ToolKind::Selection {
        Binding::R
    } else {
        Binding::L
    }
}

/// Whether this kind can hold a cross-frame Session (uncommitted work outliving a single stroke —
/// a Text burst, a floating stamp). The one place that fact lives: `flush_slot`, `end_session`,
/// the document-swap reset, and the takeover in `begin_gesture` all consult it, so a future
/// session-holding kind is a one-line change here rather than a four-site hunt.
pub(crate) fn holds_session(kind: ToolKind) -> bool {
    super::tool_def(kind).holds_session
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
pub(super) fn order_for(stroke_owner: Option<Binding>) -> [Binding; 2] {
    match stroke_owner {
        Some(b) => [b, b.other()],
        None => [Binding::L, Binding::R],
    }
}

/// Whether a pasted `Event::Paste` text is still the app's own copy: the OS clipboard is "ours"
/// exactly when `internal`'s own flattening still matches what came back on paste. Pulled out of
/// `paste_text` as a pure function so the copy/paste reconciliation decision is unit-testable
/// without constructing a full `GasciiApp`.
pub(super) fn is_own_clipboard_text(text: &str, internal: Option<&CellPatch>) -> bool {
    internal.is_some_and(|p| p.to_text() == text)
}

impl GasciiApp {
    /// Whether the document has unsaved changes: either the undo stack's current top edit doesn't
    /// match the one recorded at the last successful save or load, or a non-`Edit`-tracked
    /// session-meta property (Loop, the default frame duration) has moved since that same
    /// checkpoint. A brand-new document is clean by construction on every side of this comparison.
    pub(crate) fn is_dirty(&self) -> bool {
        edit_marker_differs(self.history.top_edit_id(), self.saved_marker)
            || self.doc.loop_playback != self.saved_loop_playback
            || self.doc.frame_duration_ms != self.saved_frame_duration_ms
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
        // app -> doc: seeds doc's cursors before the edit applies — see `active_frame`'s field doc
        // comment for the full round trip; `active_layer` mirrors it exactly.
        self.doc.set_active_frame(self.active_frame);
        self.doc.set_active_layer(self.active_layer);
        self.history.apply(&mut self.doc, edit);
        // doc -> app: some Edit kinds shift doc's cursors as a side effect of applying, independent
        // of the seed above — resync so neither field ever drifts from doc's own cursors.
        self.active_frame = self.doc.active_frame();
        self.active_layer = self.doc.active_layer();
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
    /// Ctrl+Z, Resize, Clear, rebinding a tool). Not an `Edit` — only `frame_ops`'s structural ops
    /// touch `History`; see `switch_active_layer` for the layer twin of this exact shape. A no-op
    /// if `idx` is out of range or already active.
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

    /// The layer twin of `switch_active_frame`: same flush-first, no-op-if-unchanged-or-out-of-
    /// range contract, only `layer_ops`'s structural ops touch `History` here either.
    pub(crate) fn switch_active_layer(&mut self, idx: usize) {
        if idx == self.active_layer {
            return;
        }
        self.flush_all();
        if self.doc.set_active_layer(idx) {
            self.active_layer = idx;
            self.resync_slots(None);
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
        if self.refuse_edit_during_playback() {
            return;
        }
        self.flush_all();
        if let Some(edit) = clear_document(&self.doc) {
            self.apply_edit(edit, None);
        }
    }

    /// Duplicates the active frame — the Animation menu's "Add Frame" action, and the
    /// frames-section Ctrl+D (both entry points must behave identically at every boundary). Calls
    /// `frame_ops::duplicate_frame` directly through `apply_edit` — the same shape every other
    /// menu-triggered structural edit in this app already uses. Flushes first, same trigger-table
    /// discipline as Clear/Resize/Save.
    pub(crate) fn add_frame_via_menu(&mut self) {
        if self.refuse_edit_during_playback() {
            return;
        }
        self.flush_all();
        match duplicate_frame(&self.doc, self.doc.active_frame()) {
            Ok(edit) => {
                self.apply_edit(edit, None);
                self.last_error = None;
            }
            // Matches "Resize Canvas…"'s own convention: a specific, readable message per error
            // variant, not a raw `{e:?}` dump.
            Err(FrameOpError::TooManyFrames { max, .. }) => {
                self.flash_error(format!("add frame: exceeds the {max} maximum"));
            }
            Err(FrameOpError::TotalCellBudgetExceeded { .. }) => {
                self.flash_error("add frame: exceeds the maximum total cell budget");
            }
            Err(FrameOpError::TooManyLayers { max, .. }) => {
                self.flash_error(format!("add frame: exceeds the {max} maximum layer count"));
            }
            Err(FrameOpError::IndexOutOfBounds { .. } | FrameOpError::LastFrame) => {
                // Unreachable from this call site: `duplicate_frame` is always given
                // `self.doc.active_frame()`, a provably in-range index, and never returns
                // `LastFrame` (that's `remove_frame`'s own error).
                self.flash_error("add frame: unexpected error");
            }
        }
    }

    /// Frames-section Delete: removes the active frame as one undoable step. Refuses the last
    /// frame with a readable message — unlike the timeline's own Delete button, a keypress has no
    /// disabled state to communicate the boundary with.
    pub(crate) fn delete_active_frame(&mut self) {
        if self.refuse_edit_during_playback() {
            return;
        }
        self.flush_all();
        match gascii_core::remove_frame(&self.doc, self.doc.active_frame()) {
            Ok(edit) => self.apply_edit(edit, None),
            Err(FrameOpError::LastFrame) => self.flash_error("delete frame: a document must keep at least one frame"),
            Err(_) => self.flash_error("delete frame: unexpected error"),
        }
    }

    /// Frames-section Ctrl+C: snapshots the active frame into `frame_clipboard` (the structured
    /// source `paste_frame` inserts from) AND writes the frame's text export to the OS clipboard.
    /// The OS write is load-bearing, not a courtesy: egui-winit only synthesizes `Event::Paste`
    /// when its own clipboard read returns non-empty text (confirmed against `is_paste_command`'s
    /// branch in egui-winit's `on_keyboard_input`) — without it, a following Ctrl+V would never
    /// produce any event for `handle_keys` to see at all. Flushes first so a pending burst/float
    /// is part of what's copied.
    pub(crate) fn copy_active_frame(&mut self, ctx: &egui::Context) {
        self.flush_all();
        let idx = self.doc.active_frame();
        self.frame_clipboard = self.doc.frame(idx).cloned();
        let text = gascii_core::export_frame_text(&self.doc, idx).unwrap_or_default();
        // A fully blank frame trims to an empty string, which egui-winit's paste path would skip
        // exactly like an empty clipboard — substitute a readable marker so Ctrl+V still fires.
        let text = if text.trim().is_empty() { format!("[gascii frame {}]", idx + 1) } else { text };
        ctx.copy_text(text);
    }

    /// Frames-section Ctrl+V: inserts the copied frame right after the active one (which selects
    /// it, like every frame add). Refuses when the document's layer structure changed since the
    /// copy — every frame must carry exactly the document's layer count, the one invariant
    /// `add_frame`'s own cap checks don't cover.
    pub(crate) fn paste_frame(&mut self) {
        if self.refuse_edit_during_playback() {
            return;
        }
        let Some(frame) = self.frame_clipboard.clone() else {
            self.flash_error("paste frame: no frame has been copied");
            return;
        };
        if frame.layers.len() != self.doc.layer_count() {
            self.flash_error("paste frame: the document's layer structure changed since the copy");
            return;
        }
        self.flush_all();
        match gascii_core::add_frame(&self.doc, self.doc.active_frame() + 1, frame) {
            Ok(edit) => self.apply_edit(edit, None),
            Err(FrameOpError::TooManyFrames { max, .. }) => {
                self.flash_error(format!("paste frame: exceeds the {max} maximum"));
            }
            Err(FrameOpError::TotalCellBudgetExceeded { .. }) => {
                self.flash_error("paste frame: exceeds the maximum total cell budget");
            }
            Err(FrameOpError::TooManyLayers { max, .. }) => {
                self.flash_error(format!("paste frame: exceeds the {max} maximum layer count"));
            }
            Err(_) => self.flash_error("paste frame: unexpected error"),
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
        if self.refuse_edit_during_playback() {
            return;
        }
        self.flush_all();
        if self.history.undo(&mut self.doc) {
            // doc -> app: undo restores doc's active cursors from the undone Edit's own
            // snapshot — see `active_frame`'s field doc comment.
            self.active_frame = self.doc.active_frame();
            self.active_layer = self.doc.active_layer();
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
        if self.refuse_edit_during_playback() {
            return;
        }
        if self.history.can_redo() {
            self.history.redo(&mut self.doc);
            // doc -> app: same resync as `request_undo` — see `active_frame`'s field doc comment.
            self.active_frame = self.doc.active_frame();
            self.active_layer = self.doc.active_layer();
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
        let patch = CellPatch::from_region(&self.doc, rect, self.active_layer);
        ctx.copy_text(patch.to_text());
        self.internal_clipboard = Some(patch);
    }

    /// `Ctrl+X`/Edit ▸ Cut: copies the live selection (`copy_selection`), then deletes it — one
    /// atomic change, never "copies but doesn't delete" for even a frame. Mirrors `canvas.rs`'s
    /// existing Selection-Delete-key branch exactly, just triggered from here instead of that
    /// per-frame key routing. A no-op unless a Selection binding has a region defined, same as
    /// `copy_selection`.
    pub(crate) fn cut_selection(&mut self, ctx: &egui::Context) {
        if self.refuse_edit_during_playback() {
            return;
        }
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

    /// Edit ▸ Deselect (keyboard: Escape, via `canvas.rs`'s own Selection-Escape branch — the
    /// identical pair this performs): clears the marquee/keyboard claim without deleting document
    /// content. A no-op unless a Selection binding currently holds the keyboard.
    ///
    /// "Without deleting content" means the document, not an uncommitted float: `ToolEvent::Cancel`
    /// discards a lifted-but-not-dropped float outright rather than committing it, matching
    /// the Selection-Escape precedent exactly (that branch is deliberately non-flushing so
    /// Escape-as-abort can discard an in-progress move).
    pub(crate) fn deselect(&mut self) {
        let Some(b) = self.selection_slot() else {
            return;
        };
        let tctx = crate::canvas::tool_ctx(self, b);
        self.slots[b.ix()].tool.update(ToolEvent::Cancel, &tctx, &self.doc);
        self.release_keyboard(b);
    }

    /// `Ctrl+D`/Edit ▸ Duplicate Selection: re-stamps a copy of the selected region as a floating
    /// stamp one cell down-right of the source, leaving the original committed content in place.
    /// The float is immediately moveable and commits wherever it's dropped, exactly like a paste
    /// (`accept_stamp` with no source region, so dropping never blanks the original). A no-op
    /// unless a Selection binding has a region defined, same as `copy_selection`.
    pub(crate) fn duplicate_selection(&mut self) {
        if self.refuse_edit_during_playback() {
            return;
        }
        let Some(b) = self.selection_slot() else {
            return;
        };
        // A lifted float's cells must be in `self.doc` before capturing the region — same rule as
        // `copy_selection` (a Ctrl+D mid-move duplicates the float's landed position).
        self.flush_all();
        let Some(rect) = self.slots[b.ix()].tool.selection_overlay().and_then(|v| v.marquee) else {
            return;
        };
        let patch = CellPatch::from_region(&self.doc, rect, self.active_layer);
        // One cell down-right (clamped inside the document) so the copy reads as a copy instead of
        // invisibly covering its source cell-for-cell.
        let at = (
            (rect.x0 + 1).min(self.doc.width.saturating_sub(1)),
            (rect.y0 + 1).min(self.doc.height.saturating_sub(1)),
        );
        self.slots[b.ix()].tool.accept_stamp(patch, at, &self.doc);
    }

    /// The sidebar's "Recolor Selection" button: recolors the selected region's painted cells
    /// (active layer) to the active colors, as one undoable edit. Deliberately an explicit action
    /// — changing a color well never recolors a selection by itself. Per cell: a glyph's text
    /// color takes the FG well; the background takes the BG well ONLY when that well holds an
    /// actual color — a transparent BG well means "no background change", never "wipe every
    /// background", since transparent is the well's untouched default. Blank cells stay blank —
    /// recolor changes what's painted, it never fills empty space.
    pub(crate) fn recolor_selection(&mut self) {
        if self.refuse_edit_during_playback() {
            return;
        }
        let Some(b) = self.selection_slot() else {
            return;
        };
        // Same flush-before-reading rule as `copy_selection`.
        self.flush_all();
        let Some(rect) = self.slots[b.ix()].tool.selection_overlay().and_then(|v| v.marquee) else {
            return;
        };
        let (frame, layer) = (self.active_frame, self.active_layer);
        let write_bg = self.active_bg.3 > 0;
        let mut cells = Vec::new();
        for y in rect.y0..=rect.y1 {
            for x in rect.x0..=rect.x1 {
                let Some(&before) = self.doc.cell_at(frame, layer, x, y) else { continue };
                if before.is_blank() {
                    continue;
                }
                let mut after = before;
                if before.ch != ' ' {
                    after.fg = self.active_fg;
                }
                if write_bg {
                    after.bg = self.active_bg;
                }
                if after != before {
                    cells.push(CellEdit { frame, layer, x, y, before, after });
                }
            }
        }
        if cells.is_empty() {
            return; // nothing changed: no empty undo entry
        }
        self.apply_edit(Edit::Cells(cells), Some(b));
    }

    /// Reconciles a pasted `Event::Paste` text against the internal clipboard: if it matches the
    /// internal patch's own flattening, the OS clipboard still holds our own colored copy, so that
    /// gets pasted; otherwise the text came from elsewhere and is treated as external plain text,
    /// width-validated per character. Either way, the result lands as a floating Selection stamp
    /// anchored at the hovered cell (or the origin if nothing is hovered).
    pub(crate) fn paste_text(&mut self, text: &str) {
        if self.refuse_edit_during_playback() {
            return;
        }
        if self.stroke_in_progress() {
            // Another tool's pointer gesture (drag) owns the canvas right now. `set_tool` below
            // would refuse to switch to Selection while `stroke_active` is true, silently leaving
            // whatever tool is mid-gesture active — landing the pasted stamp on `accept_stamp`
            // would then hit that tool's default no-op and discard the clipboard content with no
            // trace. Skip the paste outright and say so, rather than silently losing it.
            self.flash_error("paste ignored: a drag is in progress");
            return;
        }
        self.flush_all(); // drop any current float before reading self.doc / switching tools
        let patch = if is_own_clipboard_text(text, self.internal_clipboard.as_ref()) {
            self.internal_clipboard.clone().expect("is_own_clipboard_text implies Some")
        } else {
            let (patch, dropped) =
                CellPatch::from_external_text(text, self.active_fg, self.active_bg);
            if dropped > 0 {
                self.flash_error(format!("paste: {dropped} character(s) rejected"));
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
    pub(super) fn reset_cross_frame_tool(&mut self) {
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
        // The document about to replace `self.doc` always starts at frame/layer 0 (`Document::new`,
        // `load_str`) — these cursors must follow, or the first stroke afterward is built against a
        // stale, likely out-of-range index and silently no-ops.
        self.active_frame = 0;
        self.active_layer = 0;
    }
}
