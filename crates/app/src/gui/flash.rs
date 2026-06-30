//! Value-change flash tracking: which rows just changed, so the table can
//! highlight them and fade the highlight out. Keyed by `(root, path)`;
//! egui-independent (time is a plain monotonic `f64` in seconds) so it is
//! unit-testable.

use reclass_core::PathSeg;

pub(super) struct FlashTracker {
    map: std::collections::HashMap<(usize, Vec<PathSeg>), (String, f64)>,
    /// Fade duration in seconds (configurable via Settings).
    pub(super) fade: f64,
}

impl Default for FlashTracker {
    fn default() -> Self {
        Self {
            map: std::collections::HashMap::new(),
            fade: Self::FADE,
        }
    }
}

impl FlashTracker {
    /// Default fade duration in seconds.
    pub(super) const FADE: f64 = 0.6;

    /// A tracker with a custom fade duration (seconds).
    pub(super) fn with_fade(fade: f64) -> Self {
        Self {
            fade,
            ..Self::default()
        }
    }

    /// Reconcile against the current rows: rows whose signature changed since
    /// last frame get their timer reset (flash now); brand-new rows don't flash;
    /// rows no longer present are dropped.
    pub(super) fn update<'a>(
        &mut self,
        entries: impl Iterator<Item = (usize, &'a [PathSeg], String)>,
        now: f64,
    ) {
        let mut next = std::collections::HashMap::new();
        for (root, path, sig) in entries {
            let key = (root, path.to_vec());
            let at = match self.map.remove(&key) {
                Some((last, at)) if last == sig => at, // unchanged: keep fading
                Some(_) => now,                        // changed: flash now
                None => now - self.fade,               // first sight: no flash
            };
            next.insert(key, (sig, at));
        }
        self.map = next;
    }

    /// Highlight strength in `0.0..=1.0` (1 = just changed, 0 = faded out).
    pub(super) fn factor(&self, root: usize, path: &[PathSeg], now: f64) -> f32 {
        match self.map.get(&(root, path.to_vec())) {
            Some((_, at)) => {
                let el = (now - at).max(0.0);
                if el < self.fade {
                    (1.0 - el / self.fade) as f32
                } else {
                    0.0
                }
            }
            None => 0.0,
        }
    }

    /// Whether any row is still mid-fade (so the UI keeps repainting).
    pub(super) fn any_active(&self, now: f64) -> bool {
        self.map.values().any(|(_, at)| now - at < self.fade)
    }
}
