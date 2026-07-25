//! Auto-attach to a process by name and re-attach when it restarts. Scans
//! `/proc` for the configured name each tick (if not already attached).

use std::path::PathBuf;

use reclass::plugin::*;

#[derive(Default)]
pub struct AutoAttach {
    /// Process name to look for (from `/proc/<pid>/comm`).
    target: String,
    /// Whether to re-attach after the process exits.
    reattach: bool,
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
                ui.checkbox(&mut self.reattach, "Re-attach on restart");
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
        let pid_str = file_name.to_str()?;
        let pid: i32 = pid_str.parse().ok()?;
        let comm_path = PathBuf::from(format!("/proc/{pid}/comm"));
        if let Ok(name) = std::fs::read_to_string(&comm_path)
            && name.trim() == target
        {
            return Some(pid);
        }
    }
    None
}
