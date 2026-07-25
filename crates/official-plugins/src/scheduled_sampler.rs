//! Periodically save timestamped project snapshots to disk. Configure the
//! interval (in refresh ticks, ~60 Hz) and output directory.

use std::time::{SystemTime, UNIX_EPOCH};

use reclass::plugin::*;

#[derive(Default)]
pub struct ScheduledSampler {
    /// Output directory for saved projects.
    dir: String,
    /// Ticks between saves.
    interval: u64,
    /// Whether sampling is active.
    enabled: bool,
    /// Running tick counter.
    tick: u64,
}

impl HostPlugin for ScheduledSampler {
    fn name(&self) -> &str {
        "Scheduled Sampler"
    }
    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn on_pre_apply(&mut self, _state: &AppState) -> Vec<PluginAction> {
        if !self.enabled || self.dir.is_empty() {
            return Vec::new();
        }
        self.tick = self.tick.wrapping_add(1);
        if !self.tick.is_multiple_of(self.interval) {
            return Vec::new();
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let path = format!("{}/snapshot_{ts}.ron", self.dir.trim_end_matches('/'));
        vec![PluginAction::SaveProject { path }]
    }

    fn has_window(&self) -> bool {
        true
    }
    fn show_window(&mut self, ctx: &egui::Context, _state: &AppState, open: &mut bool) {
        egui::Window::new("Scheduled Sampler")
            .open(open)
            .resizable(true)
            .show(ctx, |ui| {
                ui.checkbox(&mut self.enabled, "Enabled");
                ui.label("Output directory:");
                ui.text_edit_singleline(&mut self.dir);
                let mut intv = self.interval;
                ui.add(egui::Slider::new(&mut intv, 1..=3600).text("Interval (ticks)"));
                self.interval = intv;
                ui.label(format!(
                    "~{}s between saves at 60 Hz",
                    self.interval as f64 / 60.0
                ));
                if self.enabled {
                    ui.label(format!(
                        "Next save in {} ticks",
                        self.interval - (self.tick % self.interval)
                    ));
                }
            });
    }
}
