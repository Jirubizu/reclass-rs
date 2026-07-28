//! The pointer-scan window: run [`reclass_core::scan`] off the UI thread and
//! turn a result into a class address expression.
//!
//! The scan is a background job because it is not fast. `crates/core/benches/scan.rs`
//! measures ~66 ms over 1 MiB of pointer-dense heap at the default depth of 4,
//! and ~660 ms at depth 8 — the backwards walk's queue grows with depth, not
//! the memory pass. On a real multi-gigabyte process that is seconds, which is
//! far past what a render loop can absorb.
//!
//! The worker re-attaches by pid rather than borrowing the app's backend:
//! `MemoryBackend` has no `Send` bound (it is held as `Box<dyn MemoryBackend>`
//! and used only from the UI thread), so nothing crosses the thread boundary
//! here except a pid, an address, and a config.

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use reclass_backend_vmem::VmemBackend;
use reclass_core::scan::{PointerPath, ScanConfig, scan_pointers};

use super::{Action, ReClassApp};

/// What the scan window is currently showing.
#[derive(Debug, Default)]
pub(super) enum ScanState {
    /// Nothing has been run yet.
    #[default]
    Idle,
    /// A worker is running; the flag is set when the user abandons it.
    Running,
    /// Finished, with the paths found (possibly none).
    Done(Vec<PointerPath>),
    /// Finished badly — attach failed, or the target's maps could not be read.
    Failed(String),
}

/// Handle on the scan worker.
pub(super) struct ScanJob {
    state: Arc<Mutex<ScanState>>,
    /// Set when the user abandons a run, so a late worker drops its result
    /// instead of overwriting whatever the window shows by then.
    abandoned: Arc<AtomicBool>,
    /// Window visibility.
    pub open: bool,
    /// Target address / expression input.
    pub target: String,
    /// Bounds the user can widen, at the cost measured in the module docs.
    pub depth: usize,
    /// Largest `+off` allowed at each hop.
    pub max_offset: u64,
    /// Result cap.
    pub max_results: usize,
}

impl Default for ScanJob {
    fn default() -> Self {
        let d = ScanConfig::default();
        ScanJob {
            state: Arc::new(Mutex::new(ScanState::Idle)),
            abandoned: Arc::new(AtomicBool::new(false)),
            open: false,
            target: String::new(),
            depth: d.max_depth,
            max_offset: d.max_offset,
            max_results: d.max_results,
        }
    }
}

impl ScanJob {
    /// Whether a worker is in flight.
    fn running(&self) -> bool {
        matches!(*self.state.lock(), ScanState::Running)
    }

    /// Spawn a scan of `target` in `pid`, replacing any previous result.
    ///
    /// `repaint` wakes the UI when the worker finishes; without it the window
    /// would sit on "scanning…" until the next unrelated frame.
    fn start(
        &mut self,
        pid: i32,
        target: u64,
        cfg: ScanConfig,
        repaint: impl Fn() + Send + 'static,
    ) {
        // A previous run may still be alive; disown its result rather than
        // waiting for it.
        self.abandoned.store(true, Ordering::Relaxed);
        self.abandoned = Arc::new(AtomicBool::new(false));

        let state = Arc::clone(&self.state);
        let abandoned = Arc::clone(&self.abandoned);
        *state.lock() = ScanState::Running;

        std::thread::spawn(move || {
            let outcome = match VmemBackend::by_pid(pid) {
                Ok(be) => match scan_pointers(&be, target, &cfg) {
                    Ok(paths) => ScanState::Done(paths),
                    Err(e) => ScanState::Failed(e.to_string()),
                },
                Err(e) => ScanState::Failed(format!("attach: {e}")),
            };
            if !abandoned.load(Ordering::Relaxed) {
                *state.lock() = outcome;
                repaint();
            }
        });
    }

    /// Abandon the running worker. It finishes anyway — `scan_pointers` has no
    /// cancel hook — but its result is discarded.
    //
    // ponytail: no real cancellation. Bounded runs make it a non-issue; if the
    // bounds ever get raised, thread an `&AtomicBool` into `walk_back`'s loop.
    fn abandon(&mut self) {
        self.abandoned.store(true, Ordering::Relaxed);
        *self.state.lock() = ScanState::Idle;
    }
}

impl ReClassApp {
    /// The *Pointer scan* window.
    pub(super) fn scan_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.scan.open {
            return;
        }
        let mut open = self.scan.open;
        egui::Window::new("Pointer scan")
            .open(&mut open)
            .resizable(true)
            .default_width(560.0)
            .show(ctx, |ui| self.scan_body(ui, actions));
        self.scan.open = open;
    }

    fn scan_body(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(pid) = self.state.attached_pid() else {
            ui.label("Attach to a process first.");
            return;
        };
        let running = self.scan.running();

        ui.horizontal(|ui| {
            ui.label("Target:");
            ui.add(
                egui::TextEdit::singleline(&mut self.scan.target)
                    .desired_width(280.0)
                    .hint_text("0x7f… or an address expression"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("depth");
            ui.add(egui::DragValue::new(&mut self.scan.depth).range(1..=8));
            ui.label("max offset");
            ui.add(
                egui::DragValue::new(&mut self.scan.max_offset)
                    .range(0..=0x10000u64)
                    .hexadecimal(1, false, true),
            );
            ui.label("results");
            ui.add(egui::DragValue::new(&mut self.scan.max_results).range(1..=1024));
        });
        ui.weak("Depth costs the most: each extra hop multiplies the search, not the memory pass.");

        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running, egui::Button::new("Scan"))
                .clicked()
            {
                self.launch_scan(pid, ui.ctx().clone());
            }
            if running {
                ui.spinner();
                if ui.button("Abandon").clicked() {
                    self.scan.abandon();
                }
            }
        });
        ui.separator();

        // Cloned out of the lock so the result list can push actions without
        // holding the mutex across UI code.
        let snapshot = match &*self.scan.state.lock() {
            ScanState::Done(paths) => Some(Ok(paths.clone())),
            ScanState::Failed(e) => Some(Err(e.clone())),
            ScanState::Idle | ScanState::Running => None,
        };
        match snapshot {
            None if running => {
                ui.label("scanning…");
            }
            None => {
                ui.weak("No scan run yet.");
            }
            Some(Err(e)) => {
                ui.colored_label(egui::Color32::RED, e);
            }
            Some(Ok(paths)) if paths.is_empty() => {
                ui.label("No static path found. Try a greater depth or max offset.");
            }
            Some(Ok(paths)) => self.scan_results(ui, &paths, actions),
        }
    }

    /// Resolve the target box and spawn the worker.
    fn launch_scan(&mut self, pid: i32, ctx: egui::Context) {
        let Some(backend) = self.state.backend() else {
            return;
        };
        let target = match reclass_core::AddrExpr::resolve(&self.scan.target, backend) {
            Ok(a) => a,
            Err(e) => {
                self.error = Some(format!("scan target: {e}"));
                return;
            }
        };
        let cfg = ScanConfig {
            max_depth: self.scan.depth,
            max_offset: self.scan.max_offset,
            max_results: self.scan.max_results,
            pointer_bytes: self.state.registry().pointer_bytes(),
        };
        self.scan
            .start(pid, target, cfg, move || ctx.request_repaint());
    }

    fn scan_results(
        &mut self,
        ui: &mut egui::Ui,
        paths: &[PointerPath],
        actions: &mut Vec<Action>,
    ) {
        let class = self.state.selected_class();
        ui.label(format!("{} path(s), shortest first:", paths.len()));
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for p in paths {
                    let expr = p.to_expr();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(class.is_some(), egui::Button::new("Use"))
                            .on_hover_text("Set this as the selected class's address")
                            .clicked()
                            && let Some(c) = class
                        {
                            actions.push(Action::SetExpr(c, expr.clone()));
                        }
                        if ui.button("Copy").clicked() {
                            self.pending_clipboard = Some(expr.clone());
                        }
                        ui.monospace(&expr);
                    });
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_job_is_idle_and_carries_the_core_defaults() {
        let j = ScanJob::default();
        let d = ScanConfig::default();
        assert!(!j.running());
        assert!(!j.open);
        assert_eq!(j.depth, d.max_depth);
        assert_eq!(j.max_offset, d.max_offset);
        assert_eq!(j.max_results, d.max_results);
    }

    #[test]
    fn abandoning_clears_the_result_and_disowns_the_worker() {
        let mut j = ScanJob::default();
        *j.state.lock() = ScanState::Done(vec![PointerPath {
            module: "a.so".into(),
            base_offset: 0,
            offsets: Vec::new(),
        }]);
        j.abandon();
        assert!(matches!(*j.state.lock(), ScanState::Idle));
        assert!(j.abandoned.load(Ordering::Relaxed));
    }

    #[test]
    fn a_disowned_worker_cannot_overwrite_a_later_state() {
        // The flag the worker checks is swapped, not just set, so a second run
        // started while the first is in flight is not immediately disowned by
        // the first one's abandonment.
        let mut j = ScanJob::default();
        let first = Arc::clone(&j.abandoned);
        j.abandoned.store(true, Ordering::Relaxed);
        j.abandoned = Arc::new(AtomicBool::new(false));
        assert!(first.load(Ordering::Relaxed), "the old worker is disowned");
        assert!(
            !j.abandoned.load(Ordering::Relaxed),
            "the new worker is not"
        );
        j.abandon();
    }
}
