//! `reclass` application library: the egui-independent [`app_state`] plus the
//! optional `gui` (egui) and `tui` (ratatui) front-ends. The `reclass` binary
//! is a thin dispatcher over these.
//!
//! `unsafe` is confined to [`plugin`], which loads and calls into arbitrary
//! native code; every call site is SAFETY-noted against the module's ABI
//! contract.
#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod app_state;
#[cfg(feature = "gui")]
pub mod gui;
pub mod mcp;
#[cfg(feature = "gui")]
pub mod plugin;
#[cfg(feature = "tui")]
pub mod tui;
