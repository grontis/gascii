use egui::{Ui, Vec2};
use gascii_core::{Buildup, DensityBrush, DensityMode, Fixed, Ramp};
use gascii_plugin_api::{OptionsGeom, Plugin, PluginHost, PluginToolCapabilities};

use crate::theme;
use crate::widgets::{self, size};

/// This plugin's own name for its one tool — the identity string the host's registry merge and
/// `PluginHost::is_bound` both key off.
pub const BRUSH: &str = "Brush";

/// Short display names for the built-in ramps — matches `gascii::ui::sidebar::ramp_label`'s exact
/// mapping so the segmented control reads identically pre- and post-migration.
fn ramp_label(name: &str) -> &str {
    match name {
        "ASCII shading" => "ASCII",
        "Block shades" => "Blocks",
        other => other,
    }
}

/// Owns the density brush's app-global state: the built-in ramps, which one is active, the
/// intensity source, and the stylus-pressure opt-in. All four used to live directly on
/// `GasciiApp`; none of them are persisted (`prefs.rs` never touched them pre-migration either).
pub struct BrushPlugin {
    ramps: Vec<Ramp>,
    active_ramp: usize,
    density_mode: DensityMode,
    brush_pressure: bool,
}

impl Default for BrushPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl BrushPlugin {
    pub fn new() -> Self {
        Self {
            ramps: gascii_core::builtin_ramps(),
            active_ramp: 0,
            density_mode: DensityMode::Fixed(Fixed(1.0)),
            brush_pressure: false,
        }
    }

    // Introspection/mutation surface beyond the `Plugin` trait: genuinely small and useful for a
    // host or test that needs to read/drive this plugin's own state directly (e.g. confirming it
    // survives being rendered through two different chrome geometries unchanged), not gated behind
    // `#[cfg(test)]` since `Plugin::as_any_mut` downcasting needs the concrete type regardless of
    // which crate is doing the downcasting.
    pub fn active_ramp(&self) -> usize {
        self.active_ramp
    }

    pub fn set_active_ramp(&mut self, i: usize) {
        self.active_ramp = i;
    }

    pub fn density_mode(&self) -> DensityMode {
        self.density_mode
    }

    pub fn set_density_mode(&mut self, mode: DensityMode) {
        self.density_mode = mode;
    }

    pub fn pressure_enabled(&self) -> bool {
        self.brush_pressure
    }

    pub fn set_pressure_enabled(&mut self, enabled: bool) {
        self.brush_pressure = enabled;
    }
}

impl Plugin for BrushPlugin {
    fn register_tools(&self) -> Vec<PluginToolCapabilities> {
        vec![PluginToolCapabilities {
            name: BRUSH,
            key: egui::Key::B,
            tip: "Paint density ramps",
            make: || Box::new(DensityBrush::new()),
            sized: true,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            pressure_sizeable: true,
            wants_extra_ctx: true,
        }]
    }

    /// Ramp, intensity mode/level and the pressure toggle — app-global state both bindings' brushes
    /// share, shown once whichever binding holds the Brush (`sidebar::binding_options_geom`'s
    /// plugin-slot dedup, not this method, is what guarantees "once").
    fn options_ui(&mut self, tool_name: &str, ui: &mut Ui, geom: OptionsGeom, host: &dyn PluginHost) {
        if tool_name != BRUSH {
            return;
        }
        let t = theme::current(ui.ctx());
        widgets::micro_label(ui, "BRUSH");

        let mut ramp = self.active_ramp;
        let names: Vec<(usize, &str)> =
            self.ramps.iter().enumerate().map(|(i, r)| (i, ramp_label(r.name))).collect();
        if widgets::segmented(ui, &mut ramp, &names, false) {
            self.active_ramp = ramp;
        }

        // The slider+percent-label half of the Fixed/Buildup row; factored out since only whether
        // it shares a `ui.horizontal` with the segmented control (kiosk) or gets its own (the
        // normal sidebar) differs between the two chrome modes.
        let render_slider = |ui: &mut Ui, density_mode: &mut DensityMode, changed: bool| {
            let mut level = match *density_mode {
                DensityMode::Fixed(Fixed(l)) => l,
                DensityMode::Buildup(_) => 1.0,
            };
            let slider = ui.add_sized(
                Vec2::new(100.0, geom.brush_slider_h),
                egui::Slider::new(&mut level, 0.0..=1.0).show_value(false),
            );
            if slider.changed() || changed {
                *density_mode = DensityMode::Fixed(Fixed(level));
            }
            ui.label(
                egui::RichText::new(format!("{:.0}%", level * 100.0))
                    .font(widgets::mono_id(size::LABEL))
                    .color(t.fg_secondary),
            );
        };

        let mut buildup = matches!(self.density_mode, DensityMode::Buildup(_));
        let modes = [(false, "Fixed"), (true, "Buildup")];
        if geom.wrap_brush_mode {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let changed = widgets::segmented(ui, &mut buildup, &modes, false);
                if buildup {
                    if changed {
                        self.density_mode = DensityMode::Buildup(Buildup);
                    }
                } else {
                    render_slider(ui, &mut self.density_mode, changed);
                }
            });
        } else {
            let changed = widgets::segmented(ui, &mut buildup, &modes, false);
            if buildup {
                if changed {
                    self.density_mode = DensityMode::Buildup(Buildup);
                }
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    render_slider(ui, &mut self.density_mode, changed);
                });
            }
        }

        // Only shown once a stylus contact has actually been observed this session — no point
        // offering a pressure toggle before there is any pressure signal to drive it.
        if host.stylus_detected() {
            widgets::checkbox(ui, &mut self.brush_pressure, "Pressure");
        }
    }

    /// Number keys `1`-`9` -> Fixed intensity 0.1-0.9, `0` -> 1.0. Only consumed while Brush is
    /// bound and no widget has focus — `tick` is now called unconditionally every frame, so this
    /// plugin checks `focused` itself (the host's own call-site gate used to do this, but a
    /// playback clock needs `tick` even while a field has focus, so the gate moved here) — pressing
    /// a digit implicitly switches into Fixed mode at that level even if Buildup was active, since
    /// reaching for a number key expresses "I want this exact intensity now."
    fn tick(&mut self, ui: &mut Ui, focused: bool, host: &dyn PluginHost) {
        if focused || !host.is_bound(BRUSH) {
            return;
        }
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
            DIGIT_KEYS.iter().find(|&&(key, _)| i.consume_key(egui::Modifiers::NONE, key)).map(|&(_, level)| level)
        });
        if let Some(level) = level {
            self.density_mode = DensityMode::Fixed(Fixed(level));
        }
    }

    fn extra_tool_ctx(&self, tool_name: &str) -> Option<(DensityMode, Vec<char>)> {
        (tool_name == BRUSH).then(|| (self.density_mode, self.ramps[self.active_ramp].chars.clone()))
    }

    /// The "Pressure" checkbox's own opt-in — distinct from Brush's static `pressure_sizeable`
    /// capability, which just says the tool supports a pressure override at all.
    fn pressure_override_enabled(&self, tool_name: &str) -> bool {
        tool_name == BRUSH && self.brush_pressure
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeHost {
        stylus: bool,
        bound: bool,
        doc: gascii_core::Document,
    }
    impl FakeHost {
        fn new(stylus: bool, bound: bool) -> Self {
            Self { stylus, bound, doc: gascii_core::Document::default_document() }
        }
    }
    impl PluginHost for FakeHost {
        fn stylus_detected(&self) -> bool {
            self.stylus
        }
        fn is_bound(&self, tool_name: &str) -> bool {
            self.bound && tool_name == BRUSH
        }
        fn document(&self) -> &gascii_core::Document {
            &self.doc
        }
    }

    fn geom() -> OptionsGeom {
        OptionsGeom { stepper_h: 26.0, shape_indent: 0.0, item_spacing_y: Some(6.0), wrap_brush_mode: false, brush_slider_h: 20.0 }
    }

    /// The merged tool row must carry exactly the pre-migration literal `ToolDef` row's capability
    /// values — a transcription slip here would silently change Brush's registry-visible behavior.
    #[test]
    fn register_tools_returns_the_expected_single_brush_row() {
        let rows = BrushPlugin::new().register_tools();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.name, "Brush");
        assert_eq!(r.key, egui::Key::B);
        assert!(r.sized);
        assert!(!r.holds_session);
        assert!(r.shows_hover);
        assert!(!r.stamps_glyph);
        assert!(!r.suppresses_shortcuts);
        assert!(r.kiosk_visible);
        assert!(r.pressure_sizeable);
        assert!(r.wants_extra_ctx);
    }

    /// Two `BrushPlugin`s must never share state — the property a process-global design would have
    /// violated, and the one `GasciiApp::headless()`-based tests lean on to not cross-contaminate.
    #[test]
    fn brush_plugin_state_is_isolated_per_instance() {
        let mut a = BrushPlugin::new();
        let b = BrushPlugin::new();
        a.set_active_ramp(1);
        a.set_density_mode(DensityMode::Buildup(Buildup));
        a.set_pressure_enabled(true);

        assert_eq!(b.active_ramp(), 0);
        assert!(matches!(b.density_mode(), DensityMode::Fixed(_)));
        assert!(!b.pressure_enabled());
    }

    /// `extra_tool_ctx` must answer only for its own tool name, and must reflect live state (not a
    /// snapshot taken at construction).
    #[test]
    fn extra_tool_ctx_answers_only_for_brush_and_reflects_live_state() {
        let mut p = BrushPlugin::new();
        p.set_active_ramp(1);
        assert!(p.extra_tool_ctx("Pencil").is_none());
        let (density, ramp) = p.extra_tool_ctx("Brush").expect("Brush wants extra context");
        assert!(matches!(density, DensityMode::Fixed(_)));
        assert_eq!(ramp, gascii_core::builtin_ramps()[1].chars);
    }

    /// `tick` must only react while Brush is actually bound, and must switch into Fixed mode at the
    /// pressed digit's level even starting from Buildup.
    #[test]
    fn tick_sets_fixed_intensity_from_a_digit_key_only_while_bound() {
        let ctx = egui::Context::default();
        let mut p = BrushPlugin::new();
        p.set_density_mode(DensityMode::Buildup(Buildup));

        // Not bound: no reaction even with a matching key event.
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Num5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, false, &FakeHost::new(false, false)));
        assert!(matches!(p.density_mode(), DensityMode::Buildup(_)), "unbound tick must not react");

        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Num5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, false, &FakeHost::new(false, true)));
        match p.density_mode() {
            DensityMode::Fixed(Fixed(level)) => assert!((level - 0.5).abs() < 1e-4),
            other => panic!("expected Fixed(0.5), got {other:?}"),
        }
    }

    /// The relocated gate: `tick` is now called unconditionally every frame, so Brush must check
    /// `focused` itself and stay silent even while bound — the property the host-side call-site
    /// gate used to guarantee for free.
    #[test]
    fn tick_does_not_react_to_a_digit_key_while_focused_even_when_bound() {
        let ctx = egui::Context::default();
        let mut p = BrushPlugin::new();
        p.set_density_mode(DensityMode::Buildup(Buildup));

        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Num5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| p.tick(ui, true, &FakeHost::new(false, true)));
        assert!(matches!(p.density_mode(), DensityMode::Buildup(_)), "a focused tick must not react even while bound");
    }

    /// `pressure_override_enabled` must answer only for Brush's own name, and must reflect the
    /// live `set_pressure_enabled` toggle rather than a snapshot.
    #[test]
    fn pressure_override_enabled_tracks_the_live_toggle_and_only_answers_for_brush() {
        let mut p = BrushPlugin::new();
        assert!(!p.pressure_override_enabled(BRUSH), "off by default");
        assert!(!p.pressure_override_enabled("Pencil"));
        p.set_pressure_enabled(true);
        assert!(p.pressure_override_enabled(BRUSH));
        assert!(!p.pressure_override_enabled("Pencil"), "must not answer for a foreign tool name");
    }

    /// `options_ui` must be a true no-op for any tool name other than Brush's own — the guard every
    /// per-tool `options_ui` in this design relies on.
    #[test]
    fn options_ui_is_a_no_op_for_a_foreign_tool_name() {
        let ctx = egui::Context::default();
        let mut p = BrushPlugin::new();
        p.set_active_ramp(1);
        let host = FakeHost::new(false, false);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            p.options_ui("Pencil", ui, geom(), &host);
        });
        assert_eq!(p.active_ramp(), 1, "a foreign tool name must not touch this plugin's own state");
    }
}
