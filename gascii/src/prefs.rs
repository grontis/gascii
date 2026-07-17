//! Cross-restart preferences: theme, per-binding tool + stamps, RECENT glyphs, recent files,
//! export settings, the grid toggle. Everything here is app-side — no serde derive lives on any
//! `gascii-core` type, so a future core enum change can never poison a stored prefs blob (an
//! unrecognized value just falls back to its default, never an error).
//!
//! Deliberately NOT persisted: the document itself, undo history, zoom/pan, `options_focus`, the
//! active colors/glyph. Those are session state, not preferences.

use std::path::PathBuf;

use eframe::egui;
use gascii_core::BrushShape;
use serde::{Deserialize, Serialize};

use crate::app::{tool_def, Binding, ExportFormat, ExportSettings, GasciiApp, ToolKind, SIZED_TOOL_COUNT, TOOLS};

const KEY: &str = "gascii_prefs";

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct Prefs {
    theme: String,
    slots: Vec<SlotPrefs>,
    recent_glyphs: String,
    recent_files: Vec<PathBuf>,
    export: ExportPrefs,
    show_grid: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct SlotPrefs {
    kind: String,
    /// `(size, shape)` per sized-tool slot, in `sized_slot` order.
    stamps: Vec<(u16, u8)>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct ExportPrefs {
    format: String,
    scale: u8,
    transparent: bool,
    trim: bool,
}

fn tool_kind_to_str(kind: ToolKind) -> &'static str {
    tool_def(kind).name
}

fn tool_kind_from_str(s: &str) -> Option<ToolKind> {
    TOOLS.iter().find(|d| d.name == s).map(|d| d.kind)
}

fn shape_to_u8(shape: BrushShape) -> u8 {
    match shape {
        BrushShape::Raw => 0,
        BrushShape::Square => 1,
        BrushShape::Circle => 2,
    }
}

/// Unknown values (a file written by a future version with a shape this build doesn't know)
/// fall back to the default rather than erroring.
fn shape_from_u8(v: u8) -> BrushShape {
    match v {
        0 => BrushShape::Raw,
        1 => BrushShape::Square,
        2 => BrushShape::Circle,
        _ => BrushShape::default(),
    }
}

fn format_to_str(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Text => "text",
        ExportFormat::Png => "png",
    }
}

fn format_from_str(s: &str) -> ExportFormat {
    match s {
        "png" => ExportFormat::Png,
        _ => ExportFormat::Text,
    }
}

/// Snaps a persisted export scale to the nearest value the Export dialog's segmented control
/// actually offers (`{1, 2, 4}`), rather than a plain range clamp — a stored `3` (hand-edited or
/// from a future version) would otherwise load as a valid `cell_px()` with no segment selected.
fn nearest_export_scale(scale: u8) -> u8 {
    const PRESETS: [u8; 3] = [1, 2, 4];
    PRESETS
        .iter()
        .copied()
        .min_by_key(|&p| (p as i16 - scale as i16).abs())
        .unwrap_or(1)
}

fn theme_str(pref: egui::ThemePreference) -> String {
    match pref {
        egui::ThemePreference::Light => "light",
        egui::ThemePreference::Dark => "dark",
        egui::ThemePreference::System => "system",
    }
    .to_owned()
}

fn theme_pref_from_str(s: &str) -> egui::ThemePreference {
    match s {
        "light" => egui::ThemePreference::Light,
        "dark" => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    }
}

impl Prefs {
    pub(crate) fn from_app(app: &GasciiApp) -> Self {
        let slots = Binding::ALL
            .iter()
            .map(|&b| {
                let slot = app.slot(b);
                let stamps = (0..SIZED_TOOL_COUNT)
                    .map(|i| (slot.stamps[i].size, shape_to_u8(slot.stamps[i].shape)))
                    .collect();
                SlotPrefs { kind: tool_kind_to_str(slot.kind).to_owned(), stamps }
            })
            .collect();
        Prefs {
            theme: theme_str(app.theme_pref),
            slots,
            recent_glyphs: app.recent_glyphs.iter().collect(),
            recent_files: app.recent_files.clone(),
            export: ExportPrefs {
                format: format_to_str(app.export.format).to_owned(),
                scale: app.export.scale,
                transparent: app.export.transparent,
                trim: app.export.trim,
            },
            show_grid: app.show_grid,
        }
    }

    /// Applies these prefs onto a freshly-constructed `app`. Never fails: an unrecognized tool
    /// name, out-of-range shape byte, or malformed slot list is simply skipped or defaulted rather
    /// than erroring — a stored prefs blob must never be able to break startup. Only sets
    /// `app.theme_pref` — the caller (`GasciiApp::new`) is responsible for pushing it onto the
    /// `egui::Context` once, since this function deliberately takes no `Context` at all.
    pub(crate) fn apply_to(&self, app: &mut GasciiApp) {
        app.theme_pref = theme_pref_from_str(&self.theme);

        for (b, slot_prefs) in Binding::ALL.iter().zip(self.slots.iter()) {
            if let Some(kind) = tool_kind_from_str(&slot_prefs.kind) {
                app.bind(*b, kind);
            }
            let slot = &mut app.slots[b.ix()];
            for (i, &(size, shape_byte)) in slot_prefs.stamps.iter().enumerate().take(SIZED_TOOL_COUNT) {
                slot.stamps[i].size = size.clamp(1, gascii_core::MAX_TOOL_SIZE);
                slot.stamps[i].shape = shape_from_u8(shape_byte);
            }
        }

        // Stored order is most-recent-first (matching `app.recent_glyphs` itself), but
        // `push_recent` always inserts its argument at the front — replaying in stored order would
        // reverse it, so the oldest char is pushed first and the most recent last.
        for ch in self.recent_glyphs.chars().rev() {
            crate::app::push_recent(&mut app.recent_glyphs, ch);
        }

        // Replayed through `note_recent_file` rather than assigned directly, so a corrupt or
        // hand-edited prefs blob (duplicates, more than 8 entries) can't bypass its dedup/cap
        // invariant — same recent-first reconstruction trick as `recent_glyphs` above: oldest
        // first so the most recent ends up at index 0.
        for path in self.recent_files.iter().rev() {
            app.note_recent_file(path);
        }
        app.export = ExportSettings {
            format: format_from_str(&self.export.format),
            scale: nearest_export_scale(self.export.scale),
            transparent: self.export.transparent,
            trim: self.export.trim,
        };
        app.show_grid = self.show_grid;
    }
}

/// Loads prefs from `storage`, applying them onto `app`. Malformed/missing JSON silently keeps
/// `app`'s freshly-constructed defaults rather than surfacing an error — a broken prefs file must
/// never block startup.
pub(crate) fn load(storage: Option<&dyn eframe::Storage>, app: &mut GasciiApp) {
    let Some(storage) = storage else { return };
    let Some(raw) = storage.get_string(KEY) else { return };
    if let Ok(prefs) = serde_json::from_str::<Prefs>(&raw) {
        prefs.apply_to(app);
    }
}

pub(crate) fn save(storage: &mut dyn eframe::Storage, app: &GasciiApp) {
    let prefs = Prefs::from_app(app);
    if let Ok(json) = serde_json::to_string(&prefs) {
        storage.set_string(KEY, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_round_trips_through_its_u8_mapping() {
        for shape in [BrushShape::Raw, BrushShape::Square, BrushShape::Circle] {
            assert_eq!(shape_from_u8(shape_to_u8(shape)), shape);
        }
    }

    #[test]
    fn an_unrecognized_shape_byte_falls_back_to_the_default() {
        assert_eq!(shape_from_u8(255), BrushShape::default());
    }

    #[test]
    fn tool_kind_round_trips_through_its_str_mapping_for_every_kind() {
        for def in TOOLS.iter() {
            assert_eq!(tool_kind_from_str(tool_kind_to_str(def.kind)), Some(def.kind));
        }
    }

    #[test]
    fn an_unrecognized_tool_name_yields_none() {
        assert_eq!(tool_kind_from_str("NotARealTool"), None);
    }

    #[test]
    fn export_format_round_trips_through_its_str_mapping() {
        for format in [ExportFormat::Text, ExportFormat::Png] {
            assert_eq!(format_from_str(format_to_str(format)), format);
        }
    }

    #[test]
    fn an_unrecognized_export_format_string_falls_back_to_text() {
        assert_eq!(format_from_str("something_else"), ExportFormat::Text);
    }

    #[test]
    fn prefs_round_trip_through_json_preserves_every_field() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Brush);
        app.slots[Binding::L.ix()].stamps[crate::app::sized_slot(ToolKind::Brush).unwrap()] =
            crate::app::StampSettings { size: 7, shape: BrushShape::Circle };
        app.recent_glyphs = vec!['a', 'b', 'c'];
        app.recent_files = vec![PathBuf::from("a.gascii"), PathBuf::from("b.gascii")];
        app.export = ExportSettings { format: ExportFormat::Png, scale: 2, transparent: true, trim: false };
        app.show_grid = true;
        app.theme_pref = egui::ThemePreference::Dark;

        let prefs = Prefs::from_app(&app);
        let json = serde_json::to_string(&prefs).unwrap();
        let back: Prefs = serde_json::from_str(&json).unwrap();

        let mut restored = GasciiApp::headless();
        back.apply_to(&mut restored);

        assert_eq!(restored.theme_pref, egui::ThemePreference::Dark);
        assert_eq!(restored.slot(Binding::L).kind, ToolKind::Brush);
        assert_eq!(
            restored.slot(Binding::L).stamps[crate::app::sized_slot(ToolKind::Brush).unwrap()],
            crate::app::StampSettings { size: 7, shape: BrushShape::Circle }
        );
        assert_eq!(restored.recent_glyphs, vec!['a', 'b', 'c']);
        assert_eq!(restored.recent_files, vec![PathBuf::from("a.gascii"), PathBuf::from("b.gascii")]);
        assert_eq!(restored.export, ExportSettings { format: ExportFormat::Png, scale: 2, transparent: true, trim: false });
        assert!(restored.show_grid);
    }

    #[test]
    fn malformed_json_leaves_a_fresh_apps_defaults_untouched() {
        struct FakeStorage(String);
        impl eframe::Storage for FakeStorage {
            fn get_string(&self, key: &str) -> Option<String> {
                (key == KEY).then(|| self.0.clone())
            }
            fn set_string(&mut self, _key: &str, _value: String) {}
            fn remove_string(&mut self, _key: &str) {}
            fn flush(&mut self) {}
        }
        let mut app = GasciiApp::headless();
        let before_kind = app.slot(Binding::L).kind;
        let storage: Box<dyn eframe::Storage> = Box::new(FakeStorage("not json at all".to_string()));
        load(Some(storage.as_ref()), &mut app);
        assert_eq!(app.slot(Binding::L).kind, before_kind, "malformed prefs must not perturb defaults");
    }

    #[test]
    fn a_corrupt_recent_files_list_is_capped_and_deduplicated_on_load() {
        let mut app = GasciiApp::headless();
        let mut prefs = Prefs::from_app(&app);
        // Stored most-recent-first, 10 unique entries — more than the 8-entry cap, which a bare
        // assignment (the pre-fix behavior) would let straight through.
        prefs.recent_files = (0..10).map(|i| PathBuf::from(format!("{i}.gascii"))).collect();
        // Plus a stray duplicate of an already-listed path tacked onto the end.
        prefs.recent_files.push(PathBuf::from("5.gascii"));
        prefs.apply_to(&mut app);

        assert_eq!(app.recent_files.len(), 8, "must cap at 8 entries on load, not just on note_recent_file");
        assert_eq!(
            app.recent_files,
            (0..8).map(|i| PathBuf::from(format!("{i}.gascii"))).collect::<Vec<_>>(),
            "replay through note_recent_file preserves most-recent-first order and drops the oldest overflow"
        );
        let dup = PathBuf::from("5.gascii");
        assert_eq!(
            app.recent_files.iter().filter(|p| **p == dup).count(),
            1,
            "a duplicate path must not survive the load"
        );
    }

    #[test]
    fn an_export_scale_with_no_matching_preset_snaps_to_the_nearest_one() {
        for (stored, expected) in [(0u8, 1u8), (1, 1), (2, 2), (3, 2), (4, 4), (5, 4), (200, 4)] {
            assert_eq!(nearest_export_scale(stored), expected, "stored scale {stored}");
        }
    }

    #[test]
    fn an_out_of_range_stamp_size_is_clamped_rather_than_stored_invalid() {
        let mut app = GasciiApp::headless();
        let mut prefs = Prefs::from_app(&app);
        prefs.slots[Binding::L.ix()].stamps[0] = (u16::MAX, 0);
        prefs.apply_to(&mut app);
        let slot0 = crate::app::sized_slot(app.slot(Binding::L).kind).unwrap();
        assert!(app.slot(Binding::L).stamps[slot0].size <= gascii_core::MAX_TOOL_SIZE);
    }

    /// A single hostile-but-well-typed prefs JSON blob combining several adversarial values at
    /// once (unrecognized tool names on both slots, an out-of-range stamp size, an unrecognized
    /// shape byte, a non-{1,2,4} export scale, an unrecognized export format, an oversized/
    /// duplicated `recent_files` list, and a bogus theme string) must not panic and must load into
    /// a fully sane, usable app -- every individual fallback already has its own narrow test, but
    /// none of them exercises more than one hostile field at a time, so a fallback that only works
    /// in isolation (e.g. because it accidentally reads a sibling field's already-sanitized value)
    /// would slip through the existing suite.
    #[test]
    fn a_json_blob_with_many_simultaneously_hostile_fields_loads_without_panicking_and_sanitizes_every_one() {
        let json = serde_json::json!({
            "theme": "not_a_real_theme",
            "slots": [
                { "kind": "TotallyMadeUpTool", "stamps": [[65535, 200], [0, 255], [3, 1], [3, 1]] },
                { "kind": "AlsoNotARealTool", "stamps": [[1, 0], [1, 0], [1, 0], [1, 0]] }
            ],
            "recent_glyphs": "abcXYZ123",
            "recent_files": [
                "0.gascii", "1.gascii", "2.gascii", "3.gascii", "4.gascii",
                "5.gascii", "6.gascii", "7.gascii", "8.gascii", "9.gascii", "5.gascii"
            ],
            "export": { "format": "not_a_real_format", "scale": 200, "transparent": true, "trim": false },
            "show_grid": true
        })
        .to_string();

        let mut app = GasciiApp::headless();
        let before_kind = app.slot(Binding::L).kind;

        // Must not panic while deserializing or applying.
        let prefs: Prefs = serde_json::from_str(&json).expect("well-typed JSON, even with hostile values, must parse");
        prefs.apply_to(&mut app);

        assert_eq!(app.theme_pref, egui::ThemePreference::System, "an unrecognized theme string falls back to System");
        // Unrecognized tool names leave the slot's kind at whatever the fresh app already had.
        assert_eq!(app.slot(Binding::L).kind, before_kind, "an unrecognized tool name must not change the bound kind");
        assert_eq!(app.slot(Binding::R).kind, app.slot(Binding::R).kind, "sanity: R slot still holds a valid kind");
        for b in Binding::ALL {
            for stamp in &app.slot(b).stamps {
                assert!(stamp.size >= 1 && stamp.size <= gascii_core::MAX_TOOL_SIZE, "every stamp size must be in range");
            }
        }
        assert_eq!(app.export.format, ExportFormat::Text, "an unrecognized export format falls back to Text");
        assert_eq!(app.export.scale, 4, "scale 200 snaps to the nearest offered preset");
        assert_eq!(app.recent_files.len(), 8, "an oversized/duplicated recent_files list is still capped at 8 on load");
        assert_eq!(
            app.recent_files.iter().collect::<std::collections::HashSet<_>>().len(),
            8,
            "no duplicate path may survive the load"
        );
        assert!(app.show_grid);
    }

    /// Acceptance criterion: "fullscreen state is never persisted" (and the same for the stylus
    /// session fields). `Prefs::from_app` simply never reads `app.stylus_detected`/
    /// `app.brush_pressure`/`app.pinch_zoom_accum`/`app.kiosk_last_fit_size`/
    /// `app.pressure_stamp_size`, and there is no `fullscreen` field on `Prefs` at all — pinned at
    /// the serialization boundary so a future accidental `#[derive(Serialize)]` field addition
    /// would break this test rather than silently start persisting session-only state.
    #[test]
    fn saved_prefs_json_never_mentions_fullscreen_or_any_stylus_session_field() {
        let mut app = GasciiApp::headless();
        app.stylus_detected = true;
        app.brush_pressure = true;
        app.pinch_zoom_accum = 1.4;

        let json = serde_json::to_string(&Prefs::from_app(&app)).unwrap();
        for forbidden in [
            "fullscreen",
            "stylus_detected",
            "brush_pressure",
            "pinch_zoom_accum",
            "kiosk_last_fit_size",
            "pressure_stamp_size",
        ] {
            assert!(!json.contains(forbidden), "prefs JSON must never mention {forbidden:?}: {json}");
        }
    }

    /// The load-side half of the same guarantee: even a hostile/forward-compatible prefs blob that
    /// smuggles in extra `fullscreen`/`stylus_detected`/`brush_pressure` keys (e.g. hand-edited, or
    /// written by some future build) must be silently ignored by serde's default
    /// unknown-field-tolerant deserialization, and `apply_to` must never set the corresponding
    /// session-only fields on a freshly-constructed app.
    #[test]
    fn a_hostile_prefs_blob_with_extra_fullscreen_and_stylus_fields_never_touches_session_only_state() {
        let json = serde_json::json!({
            "theme": "dark",
            "slots": [
                { "kind": "Pencil", "stamps": [[1, 0], [1, 0], [1, 0], [1, 0]] },
                { "kind": "Eraser", "stamps": [[1, 0], [1, 0], [1, 0], [1, 0]] }
            ],
            "recent_glyphs": "",
            "recent_files": [],
            "export": { "format": "text", "scale": 1, "transparent": false, "trim": false },
            "show_grid": false,
            "fullscreen": true,
            "stylus_detected": true,
            "brush_pressure": true
        })
        .to_string();

        let prefs: Prefs =
            serde_json::from_str(&json).expect("unknown extra fields must not fail to parse");
        let mut app = GasciiApp::headless();
        assert!(!app.stylus_detected && !app.brush_pressure, "sanity: session defaults are off");

        prefs.apply_to(&mut app);

        assert!(!app.stylus_detected, "apply_to must never set stylus_detected — it isn't a prefs field");
        assert!(!app.brush_pressure, "apply_to must never set brush_pressure — it isn't a prefs field");
    }
}
