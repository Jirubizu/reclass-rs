//! Side-by-side diff of two consecutive snapshots: which fields changed,
//! from what to what. Freezes the "before" state on the first snapshot after
//! enabling, then diffs every subsequent snapshot against it.

use std::collections::HashMap;

use reclass::plugin::*;

/// Keyed by `(root, path_string)`.
type RowKey = (usize, String);

#[derive(Clone)]
struct SnapshotEntry {
    name: String,
    address: u64,
    value: String,
    type_label: String,
}

#[derive(Default)]
pub struct StructureDiff {
    /// Frozen "before" snapshot.
    before: Option<(
        HashMap<RowKey, SnapshotEntry>,
        String, // timestamp label
    )>,
    /// Diff lines from the most recent snapshot.
    diffs: Vec<String>,
    /// Whether diffing is active (first snapshot after toggle -> freeze).
    enabled: bool,
}

impl HostPlugin for StructureDiff {
    fn name(&self) -> &str {
        "Structure Diff"
    }
    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn on_snapshot(&mut self, rows: &[Row], _state: &AppState) -> Vec<PluginAction> {
        self.diffs.clear();

        let now: HashMap<RowKey, SnapshotEntry> = rows
            .iter()
            .map(|r| {
                (
                    (r.root, format!("{:?}", r.path)),
                    SnapshotEntry {
                        name: r.name.clone(),
                        address: r.address,
                        value: r.value.clone(),
                        type_label: r.type_label.clone(),
                    },
                )
            })
            .collect();

        if self.enabled {
            if let Some((before, _)) = &self.before {
                // Compute diff: changed, added, removed.
                for (key, now_e) in &now {
                    match before.get(key) {
                        Some(b) if b.value != now_e.value => {
                            self.diffs.push(format!(
                                "~ {} ({}) @ 0x{:X}: {} → {}",
                                now_e.name, now_e.type_label, now_e.address, b.value, now_e.value
                            ));
                        }
                        Some(_) => {}
                        None => {
                            self.diffs.push(format!(
                                "+ {} ({}) @ 0x{:X}: {}",
                                now_e.name, now_e.type_label, now_e.address, now_e.value
                            ));
                        }
                    }
                }
                for (key, before_e) in before {
                    if !now.contains_key(key) {
                        self.diffs
                            .push(format!("- {} {}", before_e.name, before_e.type_label));
                    }
                }
            } else {
                // First tick after enabling — freeze baseline.
                self.before = Some((now, "baseline".into()));
            }
        }
        Vec::new()
    }

    fn has_window(&self) -> bool {
        true
    }
    fn show_window(&mut self, ctx: &egui::Context, _state: &AppState, open: &mut bool) {
        egui::Window::new("Structure Diff")
            .open(open)
            .resizable(true)
            .show(ctx, |ui| {
                ui.checkbox(&mut self.enabled, "Track changes");
                if let Some((_, label)) = &self.before {
                    ui.label(format!("Baseline: {}", label));
                }
                if ui.button("Freeze new baseline").clicked() {
                    self.before = None; // will freeze next tick
                }
                ui.separator();
                if !self.enabled {
                    ui.label("Enable tracking to diff against a frozen baseline.");
                } else if self.diffs.is_empty() && self.before.is_some() {
                    ui.label("No changes since baseline.");
                } else if self.diffs.is_empty() {
                    ui.label("Capturing baseline on next snapshot…");
                } else {
                    ui.label(format!("{} change(s):", self.diffs.len()));
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for line in &self.diffs {
                            ui.label(line);
                        }
                    });
                }
            });
    }
}
