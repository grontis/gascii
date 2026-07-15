//! The presentation layer: design tokens and the custom-painted widgets built on them.
//!
//! Everything here implements `gascii-designs-v1/GASCII Design Spec.dc.html`. The spec's inverted
//! -selection controls are painted by hand rather than styled from egui's stock widgets — see
//! [`widgets`] — so [`theme::Tokens`] is read directly far more often than `Visuals` is.

pub mod icons;
pub mod options_bar;
pub mod sidebar;
pub mod status_bar;
pub mod theme;
pub mod titlebar;
pub mod widgets;
