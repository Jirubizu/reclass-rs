//! `reclass` application library: the egui-independent [`app_state`] plus the
//! optional `gui` (egui) and `tui` (ratatui) front-ends. The `reclass` binary
//! is a thin dispatcher over these.

pub mod app_state;
#[cfg(feature = "gui")]
pub mod gui;
pub mod mcp;
#[cfg(feature = "tui")]
pub mod tui;
