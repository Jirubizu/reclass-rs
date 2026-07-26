//! Sentinel Watch — mark fields whose value must never change ("sentinel" /
//! magic-number detection). On each snapshot, any deviation is flagged in a
//! window. No parser; pure value-watch against the first-observed baseline.

use std::collections::HashMap;

use reclass::plugin::*;

/// One watched field.
#[derive(Clone)]
struct Sentinel {
    /// Display name of the owning class.
    class_name: String,
    /// Display name of the node.
    node_name: String,
    /// The first-observed stable value.
    expected: String,
}

#[derive(Default)]
pub struct SentinelWatch {
    sentinels: HashMap<(ClassId, usize), Sentinel>,
    violations: Vec<String>,
    /// Resets when a snapshot is the first after marking (capture expected).
    pending: Vec<(ClassId, usize)>,
}

impl HostPlugin for SentinelWatch {
    fn name(&self) -> &str {
        "Sentinel Watch"
    }
    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn on_snapshot(&mut self, rows: &[Row], state: &AppState) -> Vec<PluginAction> {
        self.violations.clear();
        let registry = state.registry();

        for row in rows {
            let Some((class, idx)) =
                state.resolve_owner(state.view_class(row.root).unwrap_or_default(), &row.path)
            else {
                continue;
            };
            let key = (class, idx);
            if self.pending.contains(&key) {
                // First observation after marking — set expected.
                if let Some(cls) = registry.get(class) {
                    let node = &cls.nodes[idx];
                    self.sentinels.insert(
                        key,
                        Sentinel {
                            class_name: cls.name.clone(),
                            node_name: node.name.clone(),
                            expected: row.value.clone(),
                        },
                    );
                }
            } else if let Some(s) = self.sentinels.get(&key)
                && s.expected != row.value
            {
                self.violations.push(format!(
                    "SENTINEL VIOLATION: {}.{}: was {}, now {} @ 0x{:X}",
                    s.class_name, s.node_name, s.expected, row.value, row.address,
                ));
            }
        }
        self.pending.clear();
        Vec::new()
    }

    fn has_window(&self) -> bool {
        true
    }
    fn show_window(&mut self, ctx: &egui::Context, _state: &AppState, open: &mut bool) {
        egui::Window::new("Sentinel Watch")
            .open(open)
            .resizable(true)
            .default_size([560.0, 420.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} sentinel{} active",
                    self.sentinels.len(),
                    if self.sentinels.len() == 1 { "" } else { "s" },
                ));
                if ui.button("Clear all sentinels").clicked() {
                    self.sentinels.clear();
                    self.violations.clear();
                }
                ui.separator();
                if !self.violations.is_empty() {
                    ui.colored_label(
                        egui::Color32::RED,
                        format!("{} violation(s):", self.violations.len()),
                    );
                    for v in &self.violations {
                        ui.colored_label(egui::Color32::RED, v);
                    }
                }
                if self.sentinels.is_empty() {
                    ui.label("No sentinels. Right-click a row → 'Mark as sentinel'.");
                } else {
                    egui::ScrollArea::both()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            for s in self.sentinels.values() {
                                ui.label(format!(
                                    "{}:{} = {}",
                                    s.class_name, s.node_name, s.expected
                                ));
                            }
                        });
                }
            });
    }

    fn context_menu_entries(&self) -> &[(&str, &str)] {
        &[("mark_sentinel", "Mark as sentinel")]
    }
    fn on_context_menu(
        &mut self,
        id: &str,
        class: ClassId,
        idx: usize,
        _state: &AppState,
    ) -> Vec<PluginAction> {
        if id == "mark_sentinel" {
            self.pending.push((class, idx));
        }
        Vec::new()
    }
}
