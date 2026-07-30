//! File I/O: opening/saving `.gascii` documents, the atomic-write primitive, and the terminal
//! Ctrl+C / window-close-request handling that also funnels through a save-or-discard decision.

use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui;
use gascii_core::{load_str, save_string, History};

use super::{GasciiApp, PendingConfirm};

/// Terminal Ctrl+C presses received over the process lifetime. Written by the signal-handler
/// thread, drained by `handle_ctrl_c` on the UI thread each frame.
pub(super) static CTRL_C_PRESSES: AtomicU32 = AtomicU32::new(0);

/// What a batch of new Ctrl+C presses should do. A first press asks for a normal close — the same
/// path as the window's close button, unsaved-changes veto included. A press arriving while that
/// veto dialog is already up means the user is insisting: close without saving.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CtrlCResponse {
    RequestClose,
    ForceClose,
}

/// Pure escalation rule for `handle_ctrl_c`: `count` is the process-lifetime press total, `seen`
/// how many have already been acted on, `close_confirm_up` whether the close veto dialog is
/// currently showing.
pub(super) fn ctrl_c_response(count: u32, seen: u32, close_confirm_up: bool) -> Option<CtrlCResponse> {
    if count == seen {
        return None;
    }
    Some(if close_confirm_up { CtrlCResponse::ForceClose } else { CtrlCResponse::RequestClose })
}

/// Writes `contents` to `path` via write-to-a-sibling-temp-file-then-rename, rather than a direct
/// `std::fs::write`. An interrupted write (disk full, power loss, crash mid-write) to `path`
/// directly can leave a truncated/corrupt file behind, clobbering a previously-good save with no
/// way back; writing to a temp file first and only renaming it into place once the write fully
/// succeeds means `path` either keeps its old contents or gets the new ones, never something
/// in-between. The temp file lives next to `path` (same directory) so the final rename is a
/// same-filesystem move, not a copy.
pub(super) fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
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

impl GasciiApp {
    /// Records `path` at the front of the recent-files list, de-duplicated and capped at 8.
    pub(crate) fn note_recent_file(&mut self, path: &std::path::Path) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(8);
    }

    /// Reads and parses a `.gascii` file picked via a native dialog.
    pub(crate) fn open_file(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("GASCII", &["gascii"]).pick_file() else {
            return;
        };
        self.open_path(&path);
    }

    /// Reads and parses a `.gascii` file at `path` (the native-dialog and Recent-Files entry
    /// points both funnel through here). A freshly loaded document starts with an empty undo
    /// history — there is no `before` state for its cells prior to the load. A failed open drops
    /// `path` from `recent_files` rather than leaving a dead entry behind.
    pub(crate) fn open_path(&mut self, path: &std::path::Path) {
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
                    self.saved_loop_playback = self.doc.loop_playback;
                    self.saved_frame_duration_ms = self.doc.frame_duration_ms;
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

    pub(crate) fn save_file(&mut self) {
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

    pub(crate) fn save_file_as(&mut self) {
        // Flush first — see `save_file`'s comment. Also reachable directly via the "Save As"
        // toolbar button, not only through `save_file`'s delegation.
        self.flush_all();
        let Some(path) = rfd::FileDialog::new().add_filter("GASCII", &["gascii"]).save_file() else {
            return;
        };
        self.write_gascii(&path);
    }

    pub(super) fn write_gascii(&mut self, path: &std::path::Path) {
        match write_atomic(path, save_string(&self.doc).as_bytes()) {
            Ok(()) => {
                self.current_path = Some(path.to_path_buf());
                self.last_error = None;
                self.saved_marker = self.history.top_edit_id();
                self.saved_loop_playback = self.doc.loop_playback;
                self.saved_frame_duration_ms = self.doc.frame_duration_ms;
                self.note_recent_file(path);
            }
            Err(e) => self.last_error = Some(format!("failed to save {}: {e}", path.display())),
        }
    }

    /// Drains terminal Ctrl+C presses once per frame, just before `handle_close_request` so a
    /// forced repeat press has `force_close` set ahead of the close request it re-triggers. First
    /// press: a normal close request, identical to the window's close button. Repeat press while
    /// the veto dialog is up: close without saving.
    pub(super) fn handle_ctrl_c(&mut self, ctx: &egui::Context) {
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
    pub(super) fn handle_close_request(&mut self, ctx: &egui::Context) {
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
    pub(super) fn close_now(&mut self, ctx: &egui::Context) {
        self.force_close = true;
        self.confirm = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}
