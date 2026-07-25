//! Reference reclass-rs plugin — a **snapshot change logger**.
//!
//! Demonstrates the plugin API end to end: `on_snapshot` (diff consecutive
//! snapshots into a bounded log), `has_window`/`show_window` (display the log),
//! and a context-menu entry (`on_context_menu`). Build it as a dynamic library
//! and drop the result into a `plugins/` directory next to the `reclass`
//! binary — see the module docs of `reclass::plugin` for the build contract.
#![deny(rust_2018_idioms)]

use std::collections::HashMap;

use reclass::plugin::*;

/// Cap on retained log lines; oldest are dropped past this.
const MAX_LOG: usize = 200;

/// Logs value changes it sees between snapshots.
#[derive(Default)]
struct SnapshotLogger {
    /// Last seen value per row, keyed by `(root, path)`.
    last: HashMap<String, String>,
    /// Recent change lines (bounded ring, newest last).
    log: Vec<String>,
}

impl SnapshotLogger {
    /// Stable per-row key across ticks (root index + node path).
    fn key(row: &Row) -> String {
        format!("{}:{:?}", row.root, row.path)
    }

    fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > MAX_LOG {
            let excess = self.log.len() - MAX_LOG;
            self.log.drain(0..excess);
        }
    }
}

impl HostPlugin for SnapshotLogger {
    fn name(&self) -> &str {
        "Snapshot Logger"
    }

    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn on_snapshot(&mut self, rows: &[Row], _state: &AppState) -> Vec<PluginAction> {
        for row in rows {
            let key = Self::key(row);
            if let Some(prev) = self.last.get(&key)
                && prev != &row.value
            {
                let line = format!("{}: {} -> {}", row.name, prev, row.value);
                self.push_log(line);
            }
            self.last.insert(key, row.value.clone());
        }
        // Read-only observer: no host mutations.
        Vec::new()
    }

    fn has_window(&self) -> bool {
        true
    }

    fn show_window(&mut self, ctx: &egui::Context, _state: &AppState, open: &mut bool) {
        egui::Window::new("Snapshot Logger")
            .open(open)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("{} changes", self.log.len()));
                    if ui.button("Clear").clicked() {
                        self.log.clear();
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.log.is_empty() {
                        ui.label("No value changes observed yet.");
                    }
                    for line in self.log.iter().rev() {
                        ui.label(line);
                    }
                });
            });
    }

    fn context_menu_entries(&self) -> &[(&str, &str)] {
        &[("mark", "Log: mark this field")]
    }

    fn on_context_menu(
        &mut self,
        id: &str,
        _class: ClassId,
        idx: usize,
        _state: &AppState,
    ) -> Vec<PluginAction> {
        if id == "mark" {
            self.push_log(format!("-- marked node #{idx} --"));
        }
        Vec::new()
    }
}

reclass::reclass_plugin_create!(SnapshotLogger);
