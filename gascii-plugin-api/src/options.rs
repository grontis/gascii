/// Layout geometry a plugin's `options_ui` renders against — the real deltas between the host's
/// two chrome modes (the normal sidebar and kiosk's touch chrome): stepper height, the SHAPE row's
/// indent, whether the per-binding row group gets an explicit vertical-spacing override, whether a
/// Fixed/Buildup-style mode control shares a row with its slider, and the slider's own height.
#[derive(Clone, Copy)]
pub struct OptionsGeom {
    pub stepper_h: f32,
    pub shape_indent: f32,
    /// Overrides the per-binding row group's vertical item spacing; `None` inherits whatever the
    /// caller's `Ui` already has set.
    pub item_spacing_y: Option<f32>,
    /// Whether a segmented mode control (Brush's Fixed/Buildup) shares one `ui.horizontal` with its
    /// companion slider and percent label (kiosk), or stands on its own line above them (the
    /// normal sidebar). Chrome-generic despite the name of the one control that currently uses it.
    pub inline_controls: bool,
    /// A full-width slider control's height.
    pub slider_h: f32,
}
