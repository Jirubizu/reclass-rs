//! `reclass` application library: the egui-independent [`app_state`] plus the
//! [`gui`] front-end. The `reclass` binary is a thin dispatcher over it.
//!
//! `unsafe` is confined to [`plugin`], which loads and calls into arbitrary
//! native code; every call site is SAFETY-noted against the module's ABI
//! contract.
#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod app_state;
pub mod gui;
pub mod mcp;
pub mod plugin;
pub mod updater;
