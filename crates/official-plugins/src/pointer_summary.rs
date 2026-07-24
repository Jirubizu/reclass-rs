//! Per-snapshot summary of pointer targets: for every `ClassPtr` row, show
//! which class it points to and the first few field values. A discoverability
//! window — no host mutations.

use std::collections::HashMap;

use reclass::plugin::*;

#[derive(Default)]
pub struct PointerSummary {
    /// `(name, target_class, values…)` entries, rebuilt each snapshot.
    lines: Vec<String>,
    /// Observed pointer values to detect changes.
    last: HashMap<String, (u64, String)>,
}

impl HostPlugin for PointerSummary {
    fn name(&self) -> &str {
        "Pointer Summary"
    }
    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn on_snapshot(&mut self, rows: &[Row], state: &AppState) -> Vec<PluginAction> {
        let registry = state.registry();
        self.lines.clear();

        for row in rows {
            let class_id = match &row.kind {
                NodeKind::ClassPtr { class_id } => *class_id,
                _ => continue,
            };
            let target = registry.name_of(class_id).unwrap_or("?");
            // peek: first few field values one level deep (we only have `rows` for
            // the current view; deeper targets aren't expanded so we show just the
            // class name + address).
            let prev = self
                .last
                .get(&row.name)
                .map(|(v, _)| *v != row.address)
                .unwrap_or(false);
            let prefix = if prev { "* " } else { "  " };
            let line = format!("{prefix}{} → {target} @ 0x{:X}", row.name, row.address);
            self.lines.push(line);
            self.last
                .insert(row.name.clone(), (row.address, row.value.clone()));
        }
        Vec::new()
    }

    fn has_window(&self) -> bool {
        true
    }

    fn show_window(&mut self, ctx: &egui::Context, _state: &AppState, open: &mut bool) {
        egui::Window::new("Pointer Summary")
            .open(open)
            .resizable(true)
            .show(ctx, |ui| {
                if self.lines.is_empty() {
                    ui.label("No ClassPtr rows in the current snapshot.");
                } else {
                    ui.label(format!(
                        "{} pointer{} tracked:",
                        self.lines.len(),
                        if self.lines.len() == 1 { "" } else { "s" }
                    ));
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for line in &self.lines {
                            ui.label(line);
                        }
                    });
                }
            });
    }
}
