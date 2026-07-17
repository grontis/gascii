//! The presentation layer: design tokens and the custom-painted widgets built on them.
//!
//! The inverted-selection controls are painted by hand rather than styled from egui's stock
//! widgets — see [`widgets`] — so [`theme::Tokens`] is read directly far more often than
//! `Visuals` is.

pub mod dialog;
pub mod icons;
pub mod kiosk;
pub mod options_bar;
pub mod sidebar;
pub mod status_bar;
pub mod theme;
pub mod titlebar;
pub mod widgets;
