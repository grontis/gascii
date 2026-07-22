//! The density brush's plugin-owned half: app-global state (ramps, active ramp, intensity mode,
//! the stylus-pressure opt-in) and its options-panel UI and digit-key shortcut. The `Tool` impl
//! itself (`gascii_core::DensityBrush`) stays in `gascii-core`, unchanged — only what used to live
//! directly on `GasciiApp` moves here.

mod plugin;
mod theme;
mod widgets;

pub use plugin::{BrushPlugin, BRUSH};
