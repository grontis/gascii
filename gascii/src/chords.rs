//! The chord registry: one row per keyboard shortcut in the app (tool-select letters excluded —
//! those already have their own table-driven precedent in `ToolDef`/`tools()`). Closes two gaps a
//! purely hand-written `handle_keys` can't: a chord's on-screen label drifting from what it
//! actually does (`Ctrl+Shift+Z` redo went undocumented in the menu for a long time), and a
//! plugin-registered tool key silently colliding with a global chord with nothing to catch it
//! (`reserved_global_keys`, below).
//!
//! Every chord — whether generically dispatched or hand-written — gets exactly one row here, and
//! therefore exactly one place its label lives: `menu_bar` reads through [`chord_label`] instead of
//! hand-writing the string a second time. `ChordDef::dispatch` says whether `handle_keys`'s shared
//! consume-and-dispatch loop ([`consume_generic_chords`]) owns a row's key consumption, or whether
//! it stays hand-written — every chord with real cross-chord precedence (Undo vs. Redo), a
//! non-`consume_key` event shape (Copy/CopyAll), or a gate the uniform subset doesn't share (F11's
//! unconditional reach, Escape's layered gate, the `[`/`]` steppers' `options_focus` targeting)
//! stays hand-written in `handle_keys`, exactly as its own precedence comments require.

use eframe::egui;

/// One identity per chord in the app. Tool-select letters (`P`/`E`/`T`/`F`/`R`/`L`/`S`/`I`) are
/// deliberately not here — `ToolDef`/`tools()` is already that table for them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChordId {
    Undo,
    Redo,
    Save,
    Copy,
    CopyAll,
    Paste,
    ExportDialog,
    Fit,
    SwapColors,
    ZoomIn,
    ZoomOut,
    ShrinkStamp,
    GrowStamp,
    ToggleFullscreen,
    ExitFullscreenEscape,
    New,
    Open,
    SaveAs,
    ToggleGrid,
    ZoomInAlias,
    ZoomOutAlias,
    SelectAll,
    Cut,
    Deselect,
    HelpOverlay,
}

/// Which of `handle_keys`'s consumption paths owns a chord's key(s).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChordDispatch {
    /// Hand-written elsewhere in `handle_keys` (or, for Copy/CopyAll/Cut/Paste, driven by
    /// `egui::Event::Copy`/`Cut`/`Paste` rather than a `consume_key` pattern at all) — this row
    /// exists only for its label and, where `keys` is `Some`, the reserved-key collision set.
    /// Covers `SelectAll` too: `Ctrl+A` while a widget has focus is `egui::TextEdit`'s own
    /// select-all-text-in-the-field chord (confirmed against the vendored
    /// `text_selection/cursor_range.rs`), so it needs the same `widget_focused`-only gate Undo/Redo
    /// already use — not the uniform subset's ungated `GenericAlways`.
    HandWritten,
    /// Consumed by [`consume_generic_chords`] inside `handle_keys`'s main `ui.input_mut` closure,
    /// unconditionally — Save/Export/Fit and every other row sharing this gate: never suppressed by
    /// widget focus.
    GenericAlways,
    /// Consumed by [`consume_generic_chords`], gated on `!focused` — the same gate the
    /// single-letter tool-select shortcuts already use, so typing into a focused field never fires
    /// it.
    GenericUnfocused,
}

/// One chord: identity, a short human-readable action name (the `?` overlay's own text — see
/// `gascii::ui::help_overlay`), display label (the key-combo string, e.g. `"Ctrl+S"` — what
/// `menu_bar`'s `shortcut_text` calls read), and (where it has one) the literal key pattern.
pub(crate) struct ChordDef {
    pub id: ChordId,
    pub name: &'static str,
    pub label: &'static str,
    /// `None` for a chord with no discrete `consume_key` pattern of its own — an
    /// `egui::Event::Copy`/`Cut`/`Paste`-driven chord, whose real modifier state (if any) is read
    /// off the event itself (see `copy_events`), never matched via `Modifiers`/`Key`.
    pub keys: Option<(egui::Modifiers, egui::Key)>,
    pub dispatch: ChordDispatch,
}

/// Every chord in the app, tool letters excluded. Table order is consumption precedence within a
/// `GenericAlways`/`GenericUnfocused` gate group (mirrors `tools()`'s own `find()`-based one-shot
/// consumption). `Redo` is listed before `Undo` for readability, matching their real precedence —
/// but that precedence is enforced by hand-written code in `handle_keys`, not by this table's
/// order: `egui::Modifiers::matches_logically` would let a plain Ctrl+Z pattern swallow a
/// Ctrl+Shift+Z press if the plain check ran first, regardless of what order any table lists them
/// in.
pub(crate) const CHORDS: &[ChordDef] = &[
    ChordDef {
        id: ChordId::Redo, name: "Redo",
        label: "Ctrl+Shift+Z / Ctrl+Y",
        keys: Some((egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT), egui::Key::Z)),
        dispatch: ChordDispatch::HandWritten,
    },
    ChordDef {
        id: ChordId::Undo, name: "Undo",
        label: "Ctrl+Z",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::Z)),
        dispatch: ChordDispatch::HandWritten,
    },
    // SaveAs (Ctrl+Shift+S) must be consumed before the plain Save (Ctrl+S) pattern, for the exact
    // same reason Redo precedes Undo above: `matches_logically` ignores the extra Shift, so
    // checking Save first would swallow SaveAs's own S key press.
    ChordDef {
        id: ChordId::SaveAs, name: "Save As",
        label: "Ctrl+Shift+S",
        keys: Some((egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT), egui::Key::S)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::Save, name: "Save",
        label: "Ctrl+S",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::S)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::ExportDialog, name: "Export...",
        label: "Ctrl+Shift+E",
        keys: Some((egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT), egui::Key::E)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::Fit, name: "Fit to Window",
        label: "Ctrl+0",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::Num0)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::SwapColors, name: "Swap Colors",
        label: "X",
        keys: Some((egui::Modifiers::NONE, egui::Key::X)),
        dispatch: ChordDispatch::GenericUnfocused,
    },
    ChordDef { id: ChordId::Copy, name: "Copy Selection", label: "Ctrl+C", keys: None, dispatch: ChordDispatch::HandWritten },
    ChordDef { id: ChordId::CopyAll, name: "Copy All as Text", label: "Ctrl+Shift+C", keys: None, dispatch: ChordDispatch::HandWritten },
    ChordDef { id: ChordId::Paste, name: "Paste", label: "Ctrl+V", keys: None, dispatch: ChordDispatch::HandWritten },
    ChordDef {
        id: ChordId::ZoomIn, name: "Zoom In",
        label: "+",
        keys: Some((egui::Modifiers::NONE, egui::Key::Plus)),
        dispatch: ChordDispatch::HandWritten,
    },
    ChordDef {
        id: ChordId::ZoomOut, name: "Zoom Out",
        label: "\u{2212}", // U+2212 MINUS SIGN — matches the View menu's own literal glyph
        keys: Some((egui::Modifiers::NONE, egui::Key::Minus)),
        dispatch: ChordDispatch::HandWritten,
    },
    ChordDef {
        id: ChordId::ShrinkStamp, name: "Shrink Stamp",
        label: "[",
        keys: Some((egui::Modifiers::NONE, egui::Key::OpenBracket)),
        dispatch: ChordDispatch::HandWritten,
    },
    ChordDef {
        id: ChordId::GrowStamp, name: "Grow Stamp",
        label: "]",
        keys: Some((egui::Modifiers::NONE, egui::Key::CloseBracket)),
        dispatch: ChordDispatch::HandWritten,
    },
    ChordDef {
        id: ChordId::ToggleFullscreen, name: "Toggle Full Screen",
        label: "F11",
        keys: Some((egui::Modifiers::NONE, egui::Key::F11)),
        dispatch: ChordDispatch::HandWritten,
    },
    ChordDef {
        id: ChordId::ExitFullscreenEscape, name: "Exit Full Screen",
        label: "Escape",
        keys: Some((egui::Modifiers::NONE, egui::Key::Escape)),
        dispatch: ChordDispatch::HandWritten,
    },
    ChordDef {
        id: ChordId::New, name: "New Document",
        label: "Ctrl+N",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::N)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::Open, name: "Open...",
        label: "Ctrl+O",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::O)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::ZoomInAlias, name: "Zoom In",
        label: "Ctrl+=",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::Equals)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::ZoomOutAlias, name: "Zoom Out",
        label: "Ctrl+\u{2212}",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::Minus)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::ToggleGrid, name: "Toggle Grid",
        label: "G",
        keys: Some((egui::Modifiers::NONE, egui::Key::G)),
        dispatch: ChordDispatch::GenericUnfocused,
    },
    ChordDef {
        id: ChordId::SelectAll, name: "Select All",
        label: "Ctrl+A",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::A)),
        dispatch: ChordDispatch::HandWritten,
    },
    ChordDef { id: ChordId::Cut, name: "Cut", label: "Ctrl+X", keys: None, dispatch: ChordDispatch::HandWritten },
    ChordDef {
        id: ChordId::Deselect, name: "Deselect",
        label: "Ctrl+D",
        keys: Some((egui::Modifiers::COMMAND, egui::Key::D)),
        dispatch: ChordDispatch::GenericAlways,
    },
    ChordDef {
        id: ChordId::HelpOverlay, name: "Keyboard Shortcuts",
        label: "?",
        keys: Some((egui::Modifiers::NONE, egui::Key::Questionmark)),
        dispatch: ChordDispatch::GenericUnfocused,
    },
];

/// Looks up a chord's display label — the single place `menu_bar` (and any future discoverability
/// surface) reads a shortcut string from, so a chord's label can never drift from what `CHORDS`
/// actually says it is. Linear scan: the table is small (under 20 rows) and this only ever runs
/// while building UI, never on a hot path.
pub(crate) fn chord_label(id: ChordId) -> &'static str {
    CHORDS.iter().find(|c| c.id == id).expect("every ChordId has a CHORDS row").label
}

/// Every `(name, label)` pair in `CHORDS`, in table order — the `?` overlay's one source for the
/// "host chord" half of its listing (`gascii::ui::help_overlay`). Tool-letter shortcuts come from
/// `tools()` separately; see that function's own doc comment for why the two are split.
pub(crate) fn chord_rows() -> impl Iterator<Item = (&'static str, &'static str)> {
    CHORDS.iter().map(|c| (c.name, c.label))
}

/// Consumes every chord in `CHORDS` whose `dispatch` matches `gate`, in table order, returning
/// every one that fired this frame. Mirrors `tools()`'s own `find()`-based one-shot consumption
/// loop (`app.rs`'s tool-shortcut dispatch) — "table order is consumption order" is the same rule
/// in both places.
pub(crate) fn consume_generic_chords(i: &mut egui::InputState, gate: ChordDispatch) -> Vec<ChordId> {
    CHORDS
        .iter()
        .filter(|c| c.dispatch == gate)
        .filter_map(|c| {
            let (modifiers, key) = c.keys.expect("a GenericAlways/GenericUnfocused row always carries real keys");
            i.consume_key(modifiers, key).then_some(c.id)
        })
        .collect()
}

/// Every modifier-less (`Modifiers::NONE`) global chord's key — the collision set `build_tools()`'s
/// guard checks a plugin-registered tool key against, so a plugin can never silently claim a key a
/// global chord already owns. Derived from `CHORDS` rather than hand-maintained, so it can never
/// drift from the real chord list the way a second, separately-written list could.
///
/// `Space` is added explicitly rather than derived: `gascii-anim`'s play/pause hold has no
/// `CHORDS` row of its own (it lives entirely inside that plugin crate, driven by `key_down` rather
/// than a `consume_key` pattern this table could represent), so without this a plugin could still
/// silently claim `Space` as a tool shortcut and collide with it undetected — exactly the gap this
/// whole guard exists to close.
pub(crate) fn reserved_global_keys() -> impl Iterator<Item = egui::Key> + 'static {
    CHORDS
        .iter()
        .filter_map(|c| c.keys.and_then(|(m, k)| (m == egui::Modifiers::NONE).then_some(k)))
        .chain(std::iter::once(egui::Key::Space))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chord_id_has_exactly_one_chords_row() {
        let ids = [
            ChordId::Undo,
            ChordId::Redo,
            ChordId::Save,
            ChordId::Copy,
            ChordId::CopyAll,
            ChordId::Paste,
            ChordId::ExportDialog,
            ChordId::Fit,
            ChordId::SwapColors,
            ChordId::ZoomIn,
            ChordId::ZoomOut,
            ChordId::ShrinkStamp,
            ChordId::GrowStamp,
            ChordId::ToggleFullscreen,
            ChordId::ExitFullscreenEscape,
            ChordId::New,
            ChordId::Open,
            ChordId::SaveAs,
            ChordId::ToggleGrid,
            ChordId::ZoomInAlias,
            ChordId::ZoomOutAlias,
            ChordId::SelectAll,
            ChordId::Cut,
            ChordId::Deselect,
            ChordId::HelpOverlay,
        ];
        for id in ids {
            let count = CHORDS.iter().filter(|c| c.id == id).count();
            assert_eq!(count, 1, "{id:?} must have exactly one CHORDS row");
        }
    }

    #[test]
    fn chord_label_finds_every_row_by_id() {
        assert_eq!(chord_label(ChordId::Save), "Ctrl+S");
        assert_eq!(chord_label(ChordId::Redo), "Ctrl+Shift+Z / Ctrl+Y");
    }

    #[test]
    fn chord_rows_yields_one_name_label_pair_per_chords_row_in_table_order() {
        let rows: Vec<(&str, &str)> = chord_rows().collect();
        assert_eq!(rows.len(), CHORDS.len());
        assert_eq!(rows[0], ("Redo", "Ctrl+Shift+Z / Ctrl+Y"));
        assert!(rows.contains(&("Save", "Ctrl+S")));
        assert!(!rows.iter().any(|(name, _)| name.is_empty()), "every chord must have a real display name");
    }

    /// The whole point of deriving `reserved_global_keys` from `CHORDS` rather than hand-maintaining
    /// it separately: every modifier-less chord's key shows up, and nothing else does.
    #[test]
    fn reserved_global_keys_covers_every_none_modifier_row_and_nothing_else() {
        let reserved: std::collections::HashSet<egui::Key> = reserved_global_keys().collect();
        assert!(reserved.contains(&egui::Key::X), "SwapColors (X) must be reserved");
        assert!(reserved.contains(&egui::Key::F11), "ToggleFullscreen (F11) must be reserved");
        assert!(reserved.contains(&egui::Key::Escape), "ExitFullscreenEscape (Escape) must be reserved");
        assert!(
            reserved.contains(&egui::Key::Space),
            "Space (gascii-anim's play/pause hold, which has no CHORDS row of its own) must still be reserved"
        );
        assert!(
            !reserved.contains(&egui::Key::S),
            "Save is Ctrl+S (COMMAND modifier), not a bare key — must not be reserved"
        );
    }

    #[test]
    fn consume_generic_chords_only_consumes_rows_matching_the_requested_gate() {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::S,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        raw.events.push(egui::Event::Key {
            key: egui::Key::X,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut fired_always = Vec::new();
        let mut fired_unfocused = Vec::new();
        let _ = ctx.run_ui(raw, |ui| {
            ui.input_mut(|i| {
                fired_always = consume_generic_chords(i, ChordDispatch::GenericAlways);
                fired_unfocused = consume_generic_chords(i, ChordDispatch::GenericUnfocused);
            });
        });
        assert_eq!(fired_always, vec![ChordId::Save], "only the GenericAlways row (Ctrl+S) should fire here");
        assert_eq!(fired_unfocused, vec![ChordId::SwapColors], "only the GenericUnfocused row (X) should fire here");
    }

    /// `Open`/`SaveAs` dispatch through `handle_keys` opens a real native `rfd::FileDialog` — not
    /// safely drivable end to end in a headless test (the same constraint every existing
    /// `open_file`/`save_file_as` call site already has no direct test for). Pinned at the registry
    /// layer instead: their key patterns really do fire through `consume_generic_chords`'s
    /// `GenericAlways` gate, so the only untested step is the dialog itself.
    #[test]
    fn open_and_save_as_fire_through_the_generic_always_gate() {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(egui::Event::Key {
            key: egui::Key::O,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        let mut fired = Vec::new();
        let _ = ctx.run_ui(raw, |ui| ui.input_mut(|i| fired = consume_generic_chords(i, ChordDispatch::GenericAlways)));
        assert_eq!(fired, vec![ChordId::Open]);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput {
            modifiers: egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
            ..Default::default()
        };
        raw.events.push(egui::Event::Key {
            key: egui::Key::S,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        });
        let mut fired = Vec::new();
        let _ = ctx.run_ui(raw, |ui| ui.input_mut(|i| fired = consume_generic_chords(i, ChordDispatch::GenericAlways)));
        assert_eq!(fired, vec![ChordId::SaveAs]);
    }

    /// The precedence regression the table-order comment above `SaveAs`'s row calls out by name:
    /// `Ctrl+Shift+S` must fire `SaveAs` alone — `Save`'s own `Ctrl+S` pattern (which
    /// `matches_logically` would otherwise also match, ignoring the extra Shift) must NOT also
    /// appear in the fired list. Asserting the full returned `Vec` (not just "SaveAs is present")
    /// is what actually catches a future accidental reordering of the two rows.
    #[test]
    fn ctrl_shift_s_fires_only_save_as_never_also_plain_save() {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput {
            modifiers: egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
            ..Default::default()
        };
        raw.events.push(egui::Event::Key {
            key: egui::Key::S,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
        });
        let mut fired = Vec::new();
        let _ = ctx.run_ui(raw, |ui| ui.input_mut(|i| fired = consume_generic_chords(i, ChordDispatch::GenericAlways)));
        assert_eq!(
            fired,
            vec![ChordId::SaveAs],
            "Save must not also fire alongside SaveAs — the more-specific chord must win outright"
        );
    }
}
