//! Auto-attach to a process by name. Scans `/proc` for the configured name
//! every 30 ticks while nothing is attached.
//!
//! Does *not* re-attach after the target restarts: `AppState::attached()`
//! stays true while the backend exists, whether or not the process behind it
//! is still alive, so nothing here can observe the exit. Detecting that needs
//! a liveness check on the backend, which does not exist yet.

use std::path::PathBuf;

use reclass::plugin::*;

#[derive(Default)]
pub struct AutoAttach {
    /// Process name to look for (from `/proc/<pid>/comm`).
    target: String,
    /// Avoid spamming the attach action — only emit when the pid changes.
    last_seen_pid: Option<i32>,
    /// Throttle: skip this many ticks between /proc scans.
    throttle: u32,
}

impl HostPlugin for AutoAttach {
    fn name(&self) -> &str {
        "Auto-attach"
    }
    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn on_pre_apply(&mut self, state: &AppState) -> Vec<PluginAction> {
        if state.attached() || self.target.is_empty() {
            return Vec::new();
        }
        self.throttle = self.throttle.wrapping_add(1);
        if self.throttle < 30 {
            return Vec::new();
        }
        self.throttle = 0;

        // Scan /proc for a matching comm name (Linux-specific).
        if let Some(pid) = find_by_comm(&self.target)
            && self.last_seen_pid != Some(pid)
        {
            self.last_seen_pid = Some(pid);
            return vec![PluginAction::AttachPid(pid)];
        }
        Vec::new()
    }

    fn has_window(&self) -> bool {
        true
    }
    fn show_window(&mut self, ctx: &egui::Context, state: &AppState, open: &mut bool) {
        egui::Window::new("Auto-attach")
            .open(open)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label("Process name:");
                ui.text_edit_singleline(&mut self.target);
                if state.attached() {
                    ui.label("Currently attached.");
                } else if !self.target.is_empty() {
                    ui.label(format!("Waiting for process '{}'…", self.target));
                } else {
                    ui.label("Enter a process name to auto-attach.");
                }
            });
    }
}

/// Scan `/proc/*/comm` for a process whose name matches `target`.
fn find_by_comm(target: &str) -> Option<i32> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        // `continue`, not `?`: /proc is full of non-numeric entries
        // (`cpuinfo`, `meminfo`, `self`, …) and readdir order is arbitrary.
        // Propagating here abandoned the whole scan at the first one, so the
        // target was usually never found at all.
        let Some(pid) = file_name.to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let comm_path = PathBuf::from(format!("/proc/{pid}/comm"));
        if let Ok(name) = std::fs::read_to_string(&comm_path)
            && name.trim() == target
        {
            return Some(pid);
        }
    }
    None
}
