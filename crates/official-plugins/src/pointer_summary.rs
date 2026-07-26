//! Per-snapshot summary of pointer targets: for every `ClassPtr` row, show
//! which class it points to and the first few field values. A discoverability
//! window — no host mutations.

use std::collections::HashMap;

use reclass::plugin::*;

#[derive(Default)]
pub struct PointerSummary {
    /// `(name, target_class, values…)` entries, rebuilt each snapshot.
    lines: Vec<String>,
    /// Last observed target address per pointer, to mark the ones that moved.
    ///
    /// Keyed by the row's identity `(root, path)`, not its display name: two
    /// fields called `next` in different classes or views are different
    /// pointers, and sharing a key made each one report the other's moves.
    last: HashMap<(usize, Vec<PathSeg>), u64>,
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

        // Rebuilt rather than updated in place: an entry for a row that is no
        // longer in the snapshot can never match again, and leaving it behind
        // grows the map for the life of the session.
        let mut seen = HashMap::with_capacity(self.last.len());
        for row in rows {
            let class_id = match &row.kind {
                NodeKind::ClassPtr { class_id } => *class_id,
                _ => continue,
            };
            let target = registry.name_of(class_id).unwrap_or("?");
            // peek: first few field values one level deep (we only have `rows` for
            // the current view; deeper targets aren't expanded so we show just the
            // class name + address).
            let key = (row.root, row.path.clone());
            let moved = self.last.get(&key).is_some_and(|prev| *prev != row.address);
            let prefix = if moved { "* " } else { "  " };
            self.lines.push(format!(
                "{prefix}{} → {target} @ 0x{:X}",
                row.name, row.address
            ));
            seen.insert(key, row.address);
        }
        self.last = seen;
        Vec::new()
    }

    fn has_window(&self) -> bool {
        true
    }

    fn show_window(&mut self, ctx: &egui::Context, _state: &AppState, open: &mut bool) {
        egui::Window::new("Pointer Summary")
            .open(open)
            .resizable(true)
            // A definite size is what makes `auto_shrink([false, …])` safe:
            // an auto-sized window has screen-sized available space, so a
            // scroll area told not to shrink stretches it to the display.
            .default_size([560.0, 420.0])
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
                    egui::ScrollArea::both()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            for line in &self.lines {
                                ui.label(line);
                            }
                        });
                }
            });
    }
}
