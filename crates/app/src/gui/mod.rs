//! egui front-end (Phases 4–7). All model logic lives in [`AppState`]; this is
//! the immediate-mode view + deferred-action dispatch over it.
//!
//! The view is split across sibling modules: [`panels`] (menu bar, side panel,
//! modal windows), [`table`] (the central node table), [`widgets`] (leaf label
//! helpers + node-kind palettes), [`flash`] (value-change highlighting) and
//! [`settings`] (persisted config). This module owns the app state, the
//! [`Action`] dispatch, and the `eframe` glue.

use std::time::Duration;

use eframe::egui;
use reclass_backend_vmem::{ProcInfo, VmemBackend, list_processes, process_name, select_backend};
use reclass_core::codegen::Language;
use reclass_core::{ClassId, Node, NodeKind, PathSeg};

use crate::app_state::AppState;

mod flash;
mod panels;
mod settings;
mod table;
mod widgets;

use flash::FlashTracker;
use settings::{MAX_RECENT, Settings, load_recent, save_recent};
use widgets::seed_class;

// ReClass-style column palette, tuned for the dark theme.
mod col {
    use eframe::egui::Color32;
    pub const OFFSET: Color32 = Color32::from_rgb(0xD0, 0x6B, 0x6B); // red
    pub const ADDRESS: Color32 = Color32::from_rgb(0x6F, 0xC2, 0x76); // green
    pub const TYPE: Color32 = Color32::from_rgb(0x66, 0xC6, 0xD9); // cyan
    pub const NAME: Color32 = Color32::from_rgb(0x8F, 0xA9, 0xE8); // blue
    pub const VALUE: Color32 = Color32::from_rgb(0xE0, 0xA5, 0x4A); // orange
    pub const HEX: Color32 = Color32::from_rgb(0x9C, 0x9C, 0x9C); // gray
    pub const COMMENT: Color32 = Color32::from_rgb(0x6F, 0xC2, 0x76); // green
}

// Fixed column widths (px) so the virtualized table aligns like a monospace grid.
mod w {
    pub const OFFSET: f32 = 64.0;
    pub const ADDRESS: f32 = 118.0;
    pub const TYPE: f32 = 150.0;
    pub const NAME: f32 = 120.0;
}

/// Run the native egui application.
pub fn run(
    initial_pid: Option<i32>,
    initial_addr: Option<String>,
    initial_project: Option<String>,
) -> anyhow::Result<()> {
    let settings = Settings::load();
    // SAFETY: main thread, single-threaded, before eframe spawns any worker threads.
    unsafe { select_backend(settings.use_kernel) };
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "reclass-rs",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(ReClassApp::new(
                initial_pid,
                initial_addr,
                initial_project,
                settings.use_kernel,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

#[derive(Clone, Copy, PartialEq)]
enum EditField {
    Value,
    Name,
    Comment,
}

struct Editing {
    root: usize,
    path: Vec<PathSeg>,
    field: EditField,
    buf: String,
    /// Whether we've already grabbed keyboard focus for this editor.
    focused: bool,
}

/// Inline class-rename editor state (in the Classes list).
struct ClassRename {
    id: ClassId,
    buf: String,
    focused: bool,
}

/// Modes for the in-app browser: pick a `.ron` file (Open/Save) or a directory
/// to generate a `vmem` project into (GenProject).
#[derive(Clone, Copy, PartialEq)]
enum FileMode {
    Open,
    Save,
    GenProject,
}

/// Minimal, dependency-free file browser state (egui-rendered).
struct FileDialog {
    mode: FileMode,
    dir: std::path::PathBuf,
    filename: String,
    error: Option<String>,
}

/// Deferred mutations collected during rendering, applied after the UI pass to
/// avoid borrow conflicts with the immutable read of `state` while drawing.
enum Action {
    AttachPid(i32),
    RefreshRegions,
    AddClass,
    OpenView(ClassId),
    CloseView(usize),
    SelectView(usize),
    SetExpr(ClassId, String),
    Toggle(usize, Vec<PathSeg>),
    ToggleCollapse(usize, Vec<PathSeg>),
    ExpandPointer {
        owner: ClassId,
        idx: usize,
        root: usize,
        path: Vec<PathSeg>,
    },
    StartEdit(usize, Vec<PathSeg>, EditField, String),
    CommitEdit,
    CancelEdit,
    RenameClass(ClassId, String),
    StartRenameClass(ClassId),
    EndRenameClass,
    RemoveClass(ClassId),
    RemoveSelectedClasses,
    PushNode(ClassId, NodeKind),
    AddBytes(ClassId, usize),
    AddArray(ClassId, NodeKind, usize),
    InsertAfter(ClassId, usize, NodeKind),
    DeleteNode(ClassId, usize),
    DeleteSelected,
    ChangeKind(ClassId, usize, NodeKind),
    SetArrayCount(ClassId, usize, usize),
    ExpandAll,
    CollapseAll,
    Save(String),
    Load(String),
    /// Generate a `vmem`-backed Cargo project into the given directory.
    GenerateProject(String),
}

struct ReClassApp {
    state: AppState,
    editing: Option<Editing>,
    processes: Vec<ProcInfo>,
    proc_filter: String,
    pid_input: String,
    new_class_name: String,
    save_path: String,
    show_side_panel: bool,
    show_memory_map: bool,
    show_codegen: bool,
    codegen_lang: Language,
    codegen_cache: String,
    error: Option<String>,
    add_bytes_n: usize,
    selected: std::collections::HashSet<Vec<PathSeg>>,
    sel_anchor: Option<usize>,
    array_elem: usize,
    array_count: usize,
    selected_classes: std::collections::HashSet<ClassId>,
    class_anchor: Option<usize>,
    renaming_class: Option<ClassRename>,
    flash: FlashTracker,
    /// Whether to show the "kernel module not available" modal.
    show_kernel_unavailable: bool,
    app_start: std::time::Instant,
    now: f64,
    recent: Vec<String>,
    file_dialog: Option<FileDialog>,
    settings: Settings,
    show_settings: bool,
    /// Running MCP server (in-process control surface), or `None` when off.
    mcp: Option<crate::mcp::McpRuntime>,
}

impl ReClassApp {
    fn new(
        initial_pid: Option<i32>,
        initial_addr: Option<String>,
        initial_project: Option<String>,
        use_kernel: bool,
    ) -> Self {
        let mut settings = Settings::load();
        settings.use_kernel = use_kernel;
        let mut state = AppState::new();
        state.engine.set_array_limit(settings.array_cap);
        // seed the starter class with the configured default type so the table
        // shows memory immediately on attach (ReClass-style).
        let c1 = state.add_class("Class1");
        seed_class(&mut state, c1, &settings.default_kind, settings.seed_rows);
        if let Some(addr) = initial_addr {
            let _ = state.set_address_expr(c1, addr);
        }
        let mut app = ReClassApp {
            state,
            editing: None,
            processes: list_processes(),
            proc_filter: String::new(),
            pid_input: String::new(),
            new_class_name: String::new(),
            save_path: "project.ron".to_string(),
            show_side_panel: true,
            show_memory_map: false,
            show_codegen: false,
            codegen_lang: Language::Rust,
            codegen_cache: String::new(),
            error: None,
            add_bytes_n: 1024,
            selected: std::collections::HashSet::new(),
            sel_anchor: None,
            array_elem: 0,
            array_count: 8,
            selected_classes: std::collections::HashSet::new(),
            class_anchor: None,
            renaming_class: None,
            flash: FlashTracker::with_fade(settings.flash_secs as f64),
            app_start: std::time::Instant::now(),
            now: 0.0,
            recent: load_recent(),
            file_dialog: None,
            settings,
            show_kernel_unavailable: false,
            show_settings: false,
            mcp: None,
        };
        if let Some(pid) = initial_pid {
            app.apply(Action::AttachPid(pid));
        }
        if let Some(path) = initial_project {
            app.apply(Action::Load(path));
        }
        app
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::AttachPid(pid) => match VmemBackend::by_pid(pid) {
                Ok(b) => {
                    self.state.set_backend(Box::new(b));
                    // remember the process name so the project can auto-attach on load
                    self.state.project.attach_name = process_name(pid);
                    let label = self.state.project.attach_name.as_deref().unwrap_or("?");
                    self.state.status = format!("attached to {label} (pid {pid})");
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("attach pid {pid}: {e}")),
            },
            Action::RefreshRegions => {
                self.processes = list_processes();
                self.state.refresh_regions();
            }
            Action::AddClass => {
                let name = if self.new_class_name.trim().is_empty() {
                    format!("Class{}", self.state.project.registry.len() + 1)
                } else {
                    std::mem::take(&mut self.new_class_name)
                };
                let cid = self.state.add_class(name);
                seed_class(
                    &mut self.state,
                    cid,
                    &self.settings.default_kind,
                    self.settings.seed_rows,
                );
            }
            Action::OpenView(cid) => self.state.open_view(cid),
            Action::CloseView(i) => {
                self.state.close_view(i);
                self.clear_selection();
            }
            Action::SelectView(i) => {
                self.state.selected_view = i;
                self.clear_selection();
            }
            Action::SetExpr(cid, expr) => {
                let _ = self.state.set_address_expr(cid, expr);
            }
            Action::Toggle(root, path) => self.state.toggle_expand(root, path),
            Action::ToggleCollapse(root, path) => self.state.toggle_collapse(root, path),
            Action::ExpandPointer {
                owner,
                idx,
                root,
                path,
            } => {
                if let Err(e) = self.state.expand_pointer(owner, idx, root, path) {
                    self.error = Some(e);
                }
            }
            Action::StartEdit(root, path, field, buf) => {
                self.editing = Some(Editing {
                    root,
                    path,
                    field,
                    buf,
                    focused: false,
                })
            }
            Action::CancelEdit => self.editing = None,
            Action::CommitEdit => self.commit_edit(),
            Action::RenameClass(cid, name) => {
                let _ = self.state.rename_class(cid, name);
            }
            Action::StartRenameClass(cid) => {
                let buf = self
                    .state
                    .project
                    .registry
                    .name_of(cid)
                    .unwrap_or("")
                    .to_string();
                self.renaming_class = Some(ClassRename {
                    id: cid,
                    buf,
                    focused: false,
                });
            }
            Action::EndRenameClass => self.renaming_class = None,
            Action::RemoveClass(cid) => {
                self.state.remove_class(cid);
                self.selected_classes.remove(&cid);
                self.clear_selection();
            }
            Action::RemoveSelectedClasses => {
                let ids: Vec<ClassId> = self.selected_classes.iter().copied().collect();
                for cid in ids {
                    self.state.remove_class(cid);
                }
                self.selected_classes.clear();
                self.class_anchor = None;
                self.clear_selection();
            }
            Action::PushNode(cid, kind) => {
                let off = self.state.project.registry.size_of(cid);
                if let Err(e) = self
                    .state
                    .push_node(cid, Node::new(format!("field_{off:x}"), kind))
                {
                    self.error = Some(e);
                }
            }
            Action::AddBytes(cid, n) => {
                let _ = self.state.add_bytes(cid, n);
            }
            Action::AddArray(cid, element, count) => {
                if let Err(e) = self.state.add_array(cid, element, count) {
                    self.error = Some(e);
                }
            }
            Action::InsertAfter(cid, idx, kind) => {
                if let Err(e) =
                    self.state
                        .insert_after(cid, idx, Node::new(format!("field_{idx}"), kind))
                {
                    self.error = Some(e);
                }
            }
            Action::DeleteNode(cid, idx) => {
                let _ = self.state.delete_node(cid, idx);
            }
            Action::DeleteSelected => self.delete_selected(),
            Action::ChangeKind(cid, idx, kind) => {
                if let Err(e) = self.state.change_kind(cid, idx, kind) {
                    self.error = Some(e);
                }
            }
            Action::SetArrayCount(cid, idx, n) => {
                let _ = self.state.set_array_count(cid, idx, n);
            }
            Action::ExpandAll => self.state.expand_all(),
            Action::CollapseAll => self.state.collapse_all(),
            Action::Save(path) => {
                if let Err(e) = self.state.save(&path) {
                    self.error = Some(e);
                } else {
                    self.state.status = format!("saved {path}");
                    self.remember(&path);
                }
            }
            Action::Load(path) => {
                if let Err(e) = self.state.load(&path) {
                    self.error = Some(e);
                } else {
                    self.state.status = format!("loaded {path}");
                    self.clear_selection();
                    self.remember(&path);
                    // auto-attach to the saved process name, if any
                    if let Some(name) = self.state.project.attach_name.clone() {
                        match VmemBackend::by_name(&name) {
                            Ok(b) => {
                                self.state.set_backend(Box::new(b));
                                self.state.status = format!("loaded {path}; attached to {name}");
                            }
                            Err(_) => {
                                self.state.status = format!("loaded {path}; '{name}' not running");
                            }
                        }
                    }
                }
            }
            Action::GenerateProject(dir) => match self.state.generate_project(&dir) {
                Ok(n) => {
                    self.state.status = format!("generated project ({n} files) in {dir}");
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("generate project: {e}")),
            },
        }
    }

    fn clear_selection(&mut self) {
        self.selected.clear();
        self.sel_anchor = None;
    }

    /// Delete every selected node. Resolves each selected path to its owning
    /// `(class, index)`, then deletes per class in descending index order so
    /// earlier deletes don't shift later ones.
    fn delete_selected(&mut self) {
        let Some(view_class) = self.state.selected_class() else {
            return;
        };
        let targets: Vec<(ClassId, usize)> = self
            .selected
            .iter()
            .filter_map(|p| self.state.resolve_owner(view_class, p))
            .collect();
        self.state.delete_many(&targets);
        self.clear_selection();
    }

    fn commit_edit(&mut self) {
        let Some(ed) = self.editing.take() else {
            return;
        };
        let Some(view_class) = self.state.selected_class() else {
            return;
        };
        let Some((owner, idx)) = self.state.resolve_owner(view_class, &ed.path) else {
            return;
        };
        match ed.field {
            EditField::Name => {
                let _ = self.state.rename_node(owner, idx, ed.buf);
            }
            EditField::Comment => {
                let _ = self.state.set_comment(owner, idx, ed.buf);
            }
            EditField::Value => {
                // value writes need the live address+kind; resolve from rows
                let rows = self.state.compute_rows();
                if let Some(row) = rows.iter().find(|r| r.root == ed.root && r.path == ed.path)
                    && let Err(e) = self.state.write_value(row.address, &row.kind, &ed.buf)
                {
                    self.error = Some(e);
                }
            }
        }
    }

    /// Open the in-app file browser. Starts in the current project's directory
    /// (else `$HOME`, else cwd); Save mode prefills the current file name.
    fn open_file_dialog(&mut self, mode: FileMode) {
        let start = std::path::Path::new(&self.save_path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty() && p.is_dir())
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let filename = match mode {
            FileMode::Save => std::path::Path::new(&self.save_path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project.ron".to_string()),
            FileMode::Open | FileMode::GenProject => String::new(),
        };
        self.file_dialog = Some(FileDialog {
            mode,
            dir: start,
            filename,
            error: None,
        });
    }

    /// Record `path` as the current project and push it to the front of the
    /// persisted recent list (deduped, capped, written to disk).
    fn remember(&mut self, path: &str) {
        self.save_path = path.to_string();
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_string());
        self.recent.truncate(MAX_RECENT);
        save_recent(&self.recent);
    }

    /// Reconcile the running MCP server with settings: (re)start on enable or
    /// port change, stop on disable. Cheap no-op when already in sync.
    fn sync_mcp(&mut self, ctx: &egui::Context) {
        let desired = self.settings.mcp_enabled.then_some(self.settings.mcp_port);
        let current = self.mcp.as_ref().map(crate::mcp::McpRuntime::port);
        if desired == current {
            return;
        }
        if let Some(rt) = self.mcp.take() {
            rt.stop();
        }
        if let Some(port) = desired {
            let ctx = ctx.clone();
            match crate::mcp::start(port, move || ctx.request_repaint()) {
                Ok(rt) => {
                    self.mcp = Some(rt);
                    self.state.status = format!("MCP server on 127.0.0.1:{port}");
                }
                Err(e) => {
                    self.error = Some(format!("MCP: {e}"));
                    // avoid retrying every frame on a hard failure (e.g. port busy)
                    self.settings.mcp_enabled = false;
                    self.settings.save();
                }
            }
        }
    }

    /// Apply every pending MCP request against live state, replying to each.
    fn drain_mcp(&mut self) {
        let Some(rt) = self.mcp.as_ref() else {
            return;
        };
        while let Some(call) = rt.try_recv() {
            let result = crate::mcp::dispatch(&mut self.state, &call.op);
            let _ = call.reply.send(result);
        }
    }
}

impl eframe::App for ReClassApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let hz = self.state.project.window.refresh_hz.max(1);
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_secs_f32(1.0 / hz as f32));
        self.sync_mcp(&ctx);
        self.drain_mcp();

        let rows = self.state.compute_rows();
        // value-change flash tracking (fades over FlashTracker::FADE seconds)
        self.now = self.app_start.elapsed().as_secs_f64();
        self.flash.fade = self.settings.flash_secs as f64;
        self.flash.update(
            rows.iter().map(|r| {
                (
                    r.root,
                    r.path.as_slice(),
                    format!("{}\u{1}{}", r.value, r.hex),
                )
            }),
            self.now,
        );
        if self.flash.any_active(self.now) {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        let mut actions: Vec<Action> = Vec::new();
        self.menu_bar(ui, &mut actions);
        if self.show_side_panel {
            self.side_panel(ui, &mut actions);
        }
        self.central(ui, &rows, &mut actions);
        self.memory_map_window(&ctx);
        self.codegen_window(&ctx);
        self.file_dialog_window(&ctx, &mut actions);
        self.settings_window(&ctx);
        self.kernel_unavailable_window(&ctx);

        for a in actions {
            self.apply(a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::flash::FlashTracker;
    use super::settings::Settings;
    use super::widgets::{asm_size_kinds, seed_class};
    use crate::app_state::AppState;
    use reclass_core::{IntWidth, NodeKind, PathSeg};

    #[test]
    fn asm_sizes_match_keywords() {
        let sizes: Vec<usize> = asm_size_kinds()
            .iter()
            .map(|(_, k)| k.fixed_size())
            .collect();
        assert_eq!(sizes, vec![1, 2, 4, 8, 10, 16, 32, 64]);
    }

    #[test]
    fn flash_tracker_fades_after_change() {
        let mut f = FlashTracker::default();
        let p = vec![PathSeg::Node(0)];
        let fade = FlashTracker::FADE;

        // first sighting must not flash
        f.update(std::iter::once((0usize, p.as_slice(), "1".into())), 0.0);
        assert_eq!(f.factor(0, &p, 0.0), 0.0);

        // unchanged value: still no flash
        f.update(std::iter::once((0usize, p.as_slice(), "1".into())), 0.1);
        assert_eq!(f.factor(0, &p, 0.1), 0.0);

        // changed value: full flash, then fades linearly to 0
        f.update(std::iter::once((0usize, p.as_slice(), "2".into())), 1.0);
        assert!((f.factor(0, &p, 1.0) - 1.0).abs() < 1e-6);
        assert!((f.factor(0, &p, 1.0 + fade / 2.0) - 0.5).abs() < 0.02);
        assert_eq!(f.factor(0, &p, 1.0 + fade + 0.1), 0.0);
        assert!(f.any_active(1.0));
        assert!(!f.any_active(1.0 + fade + 0.1));

        // row gone -> entry dropped
        f.update(std::iter::empty(), 5.0);
        assert_eq!(f.factor(0, &p, 5.0), 0.0);
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        // full round-trip preserves every field
        let s = Settings {
            flash_enabled: false,
            flash_color: [1, 2, 3],
            flash_secs: 1.25,
            default_kind: NodeKind::Int(IntWidth::W64),
            seed_rows: 4,
            array_cap: 512,
            mcp_enabled: true,
            mcp_port: 4001,
            use_kernel: true,
        };
        let ron = ron::ser::to_string_pretty(&s, ron::ser::PrettyConfig::default()).unwrap();
        let back: Settings = ron::from_str(&ron).unwrap();
        assert!(s == back);

        // #[serde(default)] fills missing fields from Default
        let partial: Settings = ron::from_str("(flash_secs: 2.0)").unwrap();
        assert_eq!(partial.flash_secs, 2.0);
        assert_eq!(partial.default_kind, NodeKind::Hex(IntWidth::W64));
        assert_eq!(partial.array_cap, 256);
        assert!(partial.flash_enabled);
    }

    #[test]
    fn seed_class_uses_default_kind() {
        let mut state = AppState::new();
        let c = state.add_class("C");
        seed_class(&mut state, c, &NodeKind::Int(IntWidth::W32), 5);
        let class = state.project.registry.get(c).unwrap();
        assert_eq!(class.nodes.len(), 5);
        assert!(
            class
                .nodes
                .iter()
                .all(|n| n.kind == NodeKind::Int(IntWidth::W32))
        );
    }

    #[test]
    fn flash_tracker_custom_fade() {
        let mut f = FlashTracker::with_fade(2.0);
        let p = vec![PathSeg::Node(0)];
        f.update(std::iter::once((0usize, p.as_slice(), "a".into())), 0.0);
        f.update(std::iter::once((0usize, p.as_slice(), "b".into())), 10.0);
        // half-way through the 2s fade
        assert!((f.factor(0, &p, 11.0) - 0.5).abs() < 0.02);
        assert!(f.any_active(11.5));
        assert!(!f.any_active(12.1));
    }
}
