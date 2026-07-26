//! Periodically save timestamped project snapshots to disk. Configure the
//! interval (in refresh ticks, ~60 Hz) and output directory.

use std::time::{SystemTime, UNIX_EPOCH};

use reclass::plugin::*;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ScheduledSampler {
    /// Output directory for saved projects.
    dir: String,
    /// Ticks between saves. Never 0 — see [`Default`] and `set_interval`.
    interval: u64,
    /// Whether sampling is active.
    enabled: bool,
    /// Running tick counter.
    #[serde(skip)]
    tick: u64,
}

impl Default for ScheduledSampler {
    fn default() -> Self {
        Self {
            dir: String::new(),
            interval: Self::DEFAULT_INTERVAL,
            enabled: false,
            tick: 0,
        }
    }
}

impl ScheduledSampler {
    /// One save per second at the ~60 Hz refresh rate.
    const DEFAULT_INTERVAL: u64 = 60;

    /// The only writer of `interval`. A derived `Default` left it 0, and the
    /// tick check divided by it — safe today only because egui's slider
    /// happens to clamp an out-of-range value into 1..=3600 before the
    /// enable checkbox can be reached. Clamping here does not depend on that.
    fn set_interval(&mut self, ticks: u64) {
        self.interval = ticks.max(1);
    }
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
                self.set_interval(intv);
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

    fn save_settings(&self) -> Option<String> {
        save_json(self)
    }
    fn load_settings(&mut self, data: &str) -> bool {
        if !load_json(self, data) {
            return false;
        }
        // Deserializing writes `interval` directly, bypassing `set_interval`;
        // re-run the clamp so a hand-edited or older blob cannot leave 0 here
        // for the `is_multiple_of` divide.
        self.set_interval(self.interval);
        true
    }
}
