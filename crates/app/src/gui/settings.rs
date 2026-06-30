//! Persisted user settings and per-user config paths (config dir, recent list).

use eframe::egui;
use reclass_core::{IntWidth, NodeKind};

/// Most recent projects to remember.
pub(super) const MAX_RECENT: usize = 10;

/// Per-user config directory: `$XDG_CONFIG_HOME/reclass-rs`, else
/// `$HOME/.config/reclass-rs`, else `./.reclass-rs`.
fn config_dir() -> std::path::PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return std::path::PathBuf::from(x).join("reclass-rs");
    }
    if let Some(h) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(h)
            .join(".config")
            .join("reclass-rs");
    }
    std::path::PathBuf::from(".reclass-rs")
}

fn recent_file() -> std::path::PathBuf {
    config_dir().join("recent.txt")
}

/// Load the recent-projects list (most-recent first), one path per line.
pub(super) fn load_recent() -> Vec<String> {
    std::fs::read_to_string(recent_file())
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .take(MAX_RECENT)
                .collect()
        })
        .unwrap_or_default()
}

/// Persist the recent-projects list (best-effort; errors ignored).
pub(super) fn save_recent(recent: &[String]) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(recent_file(), recent.join("\n"));
}

pub(super) fn settings_file() -> std::path::PathBuf {
    config_dir().join("settings.ron")
}

/// User configuration, persisted to `~/.config/reclass-rs/settings.ron`.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct Settings {
    /// Whether value-change highlighting is on at all.
    pub(super) flash_enabled: bool,
    /// Value-change highlight color (sRGB).
    pub(super) flash_color: [u8; 3],
    /// Highlight fade duration, in seconds.
    pub(super) flash_secs: f32,
    /// Default node type for newly-seeded fields (e.g. Hex64 vs Int64).
    pub(super) default_kind: NodeKind,
    /// Number of `default_kind` rows a fresh class is seeded with.
    pub(super) seed_rows: usize,
    /// Max array elements rendered per array node (render/perf cap).
    pub(super) array_cap: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            flash_enabled: true,
            flash_color: [0xFF, 0x40, 0x40],
            flash_secs: 0.6,
            default_kind: NodeKind::Hex(IntWidth::W64),
            seed_rows: 16,
            array_cap: 256,
        }
    }
}

impl Settings {
    pub(super) fn flash_color(&self) -> egui::Color32 {
        let [r, g, b] = self.flash_color;
        egui::Color32::from_rgb(r, g, b)
    }

    /// Load from disk, falling back to defaults on any error (missing/corrupt).
    pub(super) fn load() -> Self {
        std::fs::read_to_string(settings_file())
            .ok()
            .and_then(|s| ron::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist to disk (best-effort; errors ignored).
    pub(super) fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        if let Ok(s) = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) {
            let _ = std::fs::write(settings_file(), s);
        }
    }
}
