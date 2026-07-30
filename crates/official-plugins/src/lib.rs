//! Bundled first-party reclass-rs plugins, shipped as a single dynamic library.
//!
//! Each module is a self-contained [`HostPlugin`](reclass::plugin::HostPlugin)
//! implementation registered via
//! [`reclass_plugin_create_all!`](macro@reclass::reclass_plugin_create_all).
#![deny(rust_2018_idioms)]

mod auto_attach;
mod cheat_table;
mod copy_as;
mod hex_dump;
mod pointer_summary;
mod scheduled_sampler;
mod sentinel_watch;
mod structure_diff;

reclass::reclass_plugin_create_all!(
    pointer_summary::PointerSummary,
    sentinel_watch::SentinelWatch,
    auto_attach::AutoAttach,
    scheduled_sampler::ScheduledSampler,
    structure_diff::StructureDiff,
    hex_dump::HexDump,
    copy_as::CopyAs,
    cheat_table::CheatTableExporter,
);
