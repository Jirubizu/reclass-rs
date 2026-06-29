//! `reclass-core` — the UI- and `vmem`-independent heart of `reclass-rs`.
//!
//! It models ReClass-style classes as ordered lists of typed [`Node`]s, derives
//! field offsets, parses per-class address expressions, and runs the live read
//! loop ([`engine`]) over an abstract [`MemoryBackend`]. Everything here is
//! testable with the in-memory [`MockBackend`] — no live process required.
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod backend;
pub mod class;
pub mod codegen;
pub mod engine;
pub mod expr;
pub mod node;
pub mod project;

#[cfg(feature = "mock")]
pub use backend::MockBackend;
pub use backend::{MemError, MemoryBackend, Perms, Region, ScatterReq};
pub use class::{Class, ClassId, ClassRegistry, RegistryError};
pub use engine::{Engine, ExpandState, PathSeg, Root, Row};
pub use expr::{AddrExpr, BinOp, ExprError};
pub use node::{AddrInfo, EditErr, FmtCtx, IntWidth, Node, NodeKind, TextEncoding};
