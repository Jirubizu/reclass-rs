//! egui front-end (Phases 4–7). All model logic lives in [`AppState`]; this is
//! the immediate-mode view + deferred-action dispatch over it.

use std::time::Duration;

use eframe::egui;
use reclass_backend_vmem::{ProcInfo, VmemBackend, list_processes, process_name};
use reclass_core::codegen::Language;
use reclass_core::{ClassId, IntWidth, Node, NodeKind, PathSeg, Row, TextEncoding};

use crate::app_state::AppState;

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
pub fn run(initial_pid: Option<i32>, initial_addr: Option<String>) -> anyhow::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "reclass-rs",
        options,
        Box::new(move |_cc| Ok(Box::new(ReClassApp::new(initial_pid, initial_addr)))),
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

/// Open vs Save for the in-app file browser.
#[derive(Clone, Copy, PartialEq)]
enum FileMode {
    Open,
    Save,
}

/// Minimal, dependency-free file browser state (egui-rendered).
struct FileDialog {
    mode: FileMode,
    dir: std::path::PathBuf,
    filename: String,
    error: Option<String>,
}

/// Most recent projects to remember.
const MAX_RECENT: usize = 10;

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
fn load_recent() -> Vec<String> {
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
fn save_recent(recent: &[String]) {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(recent_file(), recent.join("\n"));
}

fn settings_file() -> std::path::PathBuf {
    config_dir().join("settings.ron")
}

/// User configuration, persisted to `~/.config/reclass-rs/settings.ron`.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    /// Whether value-change highlighting is on at all.
    flash_enabled: bool,
    /// Value-change highlight color (sRGB).
    flash_color: [u8; 3],
    /// Highlight fade duration, in seconds.
    flash_secs: f32,
    /// Default node type for newly-seeded fields (e.g. Hex64 vs Int64).
    default_kind: NodeKind,
    /// Number of `default_kind` rows a fresh class is seeded with.
    seed_rows: usize,
    /// Max array elements rendered per array node (render/perf cap).
    array_cap: usize,
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
    fn flash_color(&self) -> egui::Color32 {
        let [r, g, b] = self.flash_color;
        egui::Color32::from_rgb(r, g, b)
    }

    /// Load from disk, falling back to defaults on any error (missing/corrupt).
    fn load() -> Self {
        std::fs::read_to_string(settings_file())
            .ok()
            .and_then(|s| ron::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist to disk (best-effort; errors ignored).
    fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        if let Ok(s) = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) {
            let _ = std::fs::write(settings_file(), s);
        }
    }
}

/// Seed `cid` with `rows` fields of `kind` so a fresh class shows memory at once.
fn seed_class(state: &mut AppState, cid: ClassId, kind: &NodeKind, rows: usize) {
    for i in 0..rows {
        let _ = state.push_node(cid, Node::new(format!("field_{i}"), kind.clone()));
    }
}

/// Tracks which rows' values just changed so the UI can flash them and fade the
/// highlight out. Keyed by `(root, path)`; egui-independent (time is a plain
/// monotonic `f64` in seconds) so it is unit-testable.
struct FlashTracker {
    map: std::collections::HashMap<(usize, Vec<PathSeg>), (String, f64)>,
    /// Fade duration in seconds (configurable via Settings).
    fade: f64,
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
    const FADE: f64 = 0.6;

    /// A tracker with a custom fade duration (seconds).
    fn with_fade(fade: f64) -> Self {
        Self {
            fade,
            ..Self::default()
        }
    }

    /// Reconcile against the current rows: rows whose signature changed since
    /// last frame get their timer reset (flash now); brand-new rows don't flash;
    /// rows no longer present are dropped.
    fn update<'a>(
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
    fn factor(&self, root: usize, path: &[PathSeg], now: f64) -> f32 {
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
    fn any_active(&self, now: f64) -> bool {
        self.map.values().any(|(_, at)| now - at < self.fade)
    }
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
    app_start: std::time::Instant,
    now: f64,
    recent: Vec<String>,
    file_dialog: Option<FileDialog>,
    settings: Settings,
    show_settings: bool,
}

impl ReClassApp {
    fn new(initial_pid: Option<i32>, initial_addr: Option<String>) -> Self {
        let settings = Settings::load();
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
            show_settings: false,
        };
        if let Some(pid) = initial_pid {
            app.apply(Action::AttachPid(pid));
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
                let _ = self
                    .state
                    .push_node(cid, Node::new(format!("field_{off:x}"), kind));
            }
            Action::AddBytes(cid, n) => {
                let _ = self.state.add_bytes(cid, n);
            }
            Action::AddArray(cid, element, count) => {
                let _ = self.state.add_array(cid, element, count);
            }
            Action::InsertAfter(cid, idx, kind) => {
                let _ = self
                    .state
                    .insert_after(cid, idx, Node::new(format!("field_{idx}"), kind));
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
}

impl eframe::App for ReClassApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let hz = self.state.project.window.refresh_hz.max(1);
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_secs_f32(1.0 / hz as f32));

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

        for a in actions {
            self.apply(a);
        }
    }
}

impl ReClassApp {
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
            FileMode::Open => String::new(),
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

    /// Render the in-app file browser (when open) and push `Load`/`Save` on confirm.
    fn file_dialog_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(mut fd) = self.file_dialog.take() else {
            return;
        };
        let title = match fd.mode {
            FileMode::Open => "Open project",
            FileMode::Save => "Save project as",
        };
        let mut window_open = true;
        let mut keep = true;
        let mut confirm = false;
        egui::Window::new(title)
            .open(&mut window_open)
            .collapsible(false)
            .resizable(true)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("\u{2b06}")
                        .on_hover_text("Parent directory")
                        .clicked()
                    {
                        fd.dir = fd
                            .dir
                            .parent()
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_else(|| fd.dir.clone());
                    }
                    ui.monospace(fd.dir.display().to_string());
                });
                ui.separator();

                // subdirectories first, then *.ron files
                let mut dirs: Vec<String> = Vec::new();
                let mut files: Vec<String> = Vec::new();
                match std::fs::read_dir(&fd.dir) {
                    Ok(rd) => {
                        for entry in rd.flatten() {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            if name.starts_with('.') {
                                continue;
                            }
                            if entry.path().is_dir() {
                                dirs.push(name);
                            } else if name.ends_with(".ron") {
                                files.push(name);
                            }
                        }
                        fd.error = None;
                    }
                    Err(e) => fd.error = Some(format!("cannot read directory: {e}")),
                }
                dirs.sort();
                files.sort();

                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for d in &dirs {
                            if ui
                                .selectable_label(false, format!("\u{1f4c1} {d}"))
                                .clicked()
                            {
                                fd.dir.push(d);
                            }
                        }
                        for f in &files {
                            let resp =
                                ui.selectable_label(fd.filename == *f, format!("\u{1f4c4} {f}"));
                            if resp.clicked() {
                                fd.filename = f.clone();
                            }
                            if resp.double_clicked() {
                                fd.filename = f.clone();
                                confirm = true;
                            }
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("File:");
                    let r = ui.text_edit_singleline(&mut fd.filename);
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirm = true;
                    }
                    let label = match fd.mode {
                        FileMode::Open => "Open",
                        FileMode::Save => "Save",
                    };
                    if ui.button(label).clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
                if let Some(e) = &fd.error {
                    ui.colored_label(col::OFFSET, e);
                }
            });

        if confirm && !fd.filename.trim().is_empty() {
            let mut name = fd.filename.trim().to_string();
            if !name.ends_with(".ron") {
                name.push_str(".ron");
            }
            let path = fd.dir.join(&name).to_string_lossy().into_owned();
            match fd.mode {
                FileMode::Open => actions.push(Action::Load(path)),
                FileMode::Save => actions.push(Action::Save(path)),
            }
            keep = false;
        }
        if window_open && keep {
            self.file_dialog = Some(fd);
        }
    }
    fn menu_bar(&mut self, root_ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::top("menu").show(root_ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open project…").clicked() {
                        self.open_file_dialog(FileMode::Open);
                        ui.close();
                    }
                    ui.menu_button("Open recent", |ui| {
                        if self.recent.is_empty() {
                            ui.label("(none)");
                        }
                        for p in self.recent.clone() {
                            if ui.button(&p).clicked() {
                                actions.push(Action::Load(p));
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    let has_path = !self.save_path.trim().is_empty();
                    if ui
                        .add_enabled(has_path, egui::Button::new("Save"))
                        .clicked()
                    {
                        actions.push(Action::Save(self.save_path.clone()));
                        ui.close();
                    }
                    if ui.button("Save as…").clicked() {
                        self.open_file_dialog(FileMode::Save);
                        ui.close();
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.show_side_panel, "Classes panel");
                    ui.checkbox(&mut self.show_memory_map, "Memory map");
                    ui.checkbox(&mut self.show_codegen, "Code generation");
                    ui.separator();
                    ui.checkbox(&mut self.show_settings, "Settings");
                });
                ui.separator();
                ui.label("Refresh Hz:");
                ui.add(
                    egui::DragValue::new(&mut self.state.project.window.refresh_hz).range(1..=120),
                );
                ui.separator();
                let dot = if self.state.attached() { "🟢" } else { "⚪" };
                ui.label(format!("{dot} {}", self.state.status));
                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::RED, format!("⚠ {err}"));
                }
            });
        });
    }

    fn side_panel(&mut self, root_ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::left("side")
            .default_size(260.0)
            .show(root_ui, |ui| {
                ui.heading("Process");
                ui.horizontal(|ui| {
                    ui.label("PID:");
                    ui.text_edit_singleline(&mut self.pid_input);
                    if ui.button("Attach").clicked() {
                        if let Ok(pid) = self.pid_input.trim().parse::<i32>() {
                            actions.push(Action::AttachPid(pid));
                        } else {
                            self.error = Some("bad pid".to_string());
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    ui.text_edit_singleline(&mut self.proc_filter);
                    if ui.button("⟳").clicked() {
                        actions.push(Action::RefreshRegions);
                    }
                });
                let filter = self.proc_filter.to_lowercase();
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .id_salt("procs")
                    .show(ui, |ui| {
                        for p in self
                            .processes
                            .iter()
                            .filter(|p| {
                                filter.is_empty() || p.name.to_lowercase().contains(&filter)
                            })
                            .take(400)
                        {
                            if ui
                                .selectable_label(false, format!("{:>7}  {}", p.pid, p.name))
                                .clicked()
                            {
                                actions.push(Action::AttachPid(p.pid));
                            }
                        }
                    });

                ui.separator();
                ui.heading("Classes");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.new_class_name);
                    if ui.button("+ Add").clicked() {
                        actions.push(Action::AddClass);
                    }
                });
                let ids = self.state.project.registry.ids();
                egui::ScrollArea::vertical()
                    .id_salt("classes")
                    .show(ui, |ui| {
                        for (i, id) in ids.iter().enumerate() {
                            let id = *id;
                            // inline rename editor for this class?
                            if self.renaming_class.as_ref().map(|r| r.id) == Some(id) {
                                if let Some(r) = self.renaming_class.as_mut() {
                                    let resp = ui.text_edit_singleline(&mut r.buf);
                                    if !r.focused {
                                        resp.request_focus();
                                        r.focused = true;
                                    }
                                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                        actions.push(Action::EndRenameClass);
                                    } else if resp.lost_focus() {
                                        actions.push(Action::RenameClass(id, r.buf.clone()));
                                        actions.push(Action::EndRenameClass);
                                    }
                                }
                                continue;
                            }
                            let name = self
                                .state
                                .project
                                .registry
                                .name_of(id)
                                .unwrap_or("?")
                                .to_string();
                            let size = self.state.project.registry.size_of(id);
                            let selected = self.selected_classes.contains(&id);
                            let resp = ui.add(egui::Button::selectable(
                                selected,
                                format!("{name}  (0x{size:X})"),
                            ));
                            if resp.clicked() {
                                let mods = ui.input(|i| i.modifiers);
                                if mods.shift {
                                    if let Some(a) = self.class_anchor.filter(|&a| a < ids.len()) {
                                        let (lo, hi) = if a <= i { (a, i) } else { (i, a) };
                                        self.selected_classes.clear();
                                        for &c in &ids[lo..=hi] {
                                            self.selected_classes.insert(c);
                                        }
                                    } else {
                                        self.selected_classes.insert(id);
                                        self.class_anchor = Some(i);
                                    }
                                } else if mods.command {
                                    if !self.selected_classes.remove(&id) {
                                        self.selected_classes.insert(id);
                                    }
                                    self.class_anchor = Some(i);
                                } else {
                                    self.selected_classes.clear();
                                    self.selected_classes.insert(id);
                                    self.class_anchor = Some(i);
                                    actions.push(Action::OpenView(id));
                                }
                            }
                            resp.context_menu(|ui| {
                                if ui.button("Open").clicked() {
                                    actions.push(Action::OpenView(id));
                                    ui.close();
                                }
                                if ui.button("Rename…").clicked() {
                                    actions.push(Action::StartRenameClass(id));
                                    ui.close();
                                }
                                if ui.button("Delete").clicked() {
                                    actions.push(Action::RemoveClass(id));
                                    ui.close();
                                }
                                let n = self.selected_classes.len();
                                if n > 1 && ui.button(format!("Delete selected ({n})")).clicked() {
                                    actions.push(Action::RemoveSelectedClasses);
                                    ui.close();
                                }
                            });
                        }
                    });
            });
    }

    fn central(&mut self, root_ui: &mut egui::Ui, rows: &[Row], actions: &mut Vec<Action>) {
        egui::CentralPanel::default().show(root_ui, |ui| {
            // tab strip
            ui.horizontal_wrapped(|ui| {
                let views: Vec<(usize, ClassId)> = self
                    .state
                    .project
                    .views
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i, v.class_id))
                    .collect();
                for (i, cid) in views {
                    let name = self
                        .state
                        .project
                        .registry
                        .name_of(cid)
                        .unwrap_or("?")
                        .to_string();
                    let selected = i == self.state.selected_view;
                    if ui.selectable_label(selected, &name).clicked() {
                        actions.push(Action::SelectView(i));
                    }
                    if ui.small_button("✕").clicked() {
                        actions.push(Action::CloseView(i));
                    }
                }
            });
            ui.separator();

            let Some(view_class) = self.state.selected_class() else {
                ui.label("No class open.");
                return;
            };
            let view_idx = self.state.selected_view;

            // class name editor
            let mut cname = self
                .state
                .project
                .registry
                .name_of(view_class)
                .unwrap_or("")
                .to_string();
            ui.horizontal(|ui| {
                ui.label("Class:");
                if ui
                    .add(egui::TextEdit::singleline(&mut cname).desired_width(200.0))
                    .changed()
                {
                    actions.push(Action::RenameClass(view_class, cname.clone()));
                }
                ui.label(format!(
                    "size 0x{:X}",
                    self.state.project.registry.size_of(view_class)
                ));
            });

            // address bar
            let mut expr = self
                .state
                .project
                .registry
                .get(view_class)
                .map(|c| c.address_expr.clone())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label("Address:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut expr)
                        .desired_width(360.0)
                        .hint_text("<module.so> + 0x10 | [0xADDR] + 0x8"),
                );
                if resp.changed() {
                    actions.push(Action::SetExpr(view_class, expr.clone()));
                }
                let st = self
                    .state
                    .view_status
                    .get(view_idx)
                    .cloned()
                    .unwrap_or_default();
                match &st.error {
                    Some(e) => {
                        ui.colored_label(egui::Color32::RED, e);
                    }
                    None => {
                        let readable = self.state.addr_is_readable(st.base);
                        let col = if readable {
                            egui::Color32::GREEN
                        } else {
                            egui::Color32::YELLOW
                        };
                        ui.colored_label(col, format!("= 0x{:X}", st.base));
                        if !readable && st.base != 0 {
                            ui.colored_label(egui::Color32::YELLOW, "(unmapped)");
                        }
                    }
                }
            });
            ui.separator();

            // add-field toolbar (works even when the class is empty)
            ui.horizontal_wrapped(|ui| {
                ui.label("Add field:");
                for (label, kind) in [
                    ("Hex32", NodeKind::Hex(IntWidth::W32)),
                    ("Hex64", NodeKind::Hex(IntWidth::W64)),
                    ("Int32", NodeKind::Int(IntWidth::W32)),
                    ("Float", NodeKind::Float32),
                    ("Pointer", NodeKind::Pointer),
                ] {
                    if ui.button(label).clicked() {
                        actions.push(Action::PushNode(view_class, kind));
                    }
                }
                ui.menu_button("More…", |ui| {
                    for (label, kind) in scalar_kinds() {
                        if ui.button(label).clicked() {
                            actions.push(Action::PushNode(view_class, kind));
                            ui.close();
                        }
                    }
                });
                ui.menu_button("asm…", |ui| {
                    for (label, kind) in asm_size_kinds() {
                        if ui.button(label).clicked() {
                            actions.push(Action::PushNode(view_class, kind));
                            ui.close();
                        }
                    }
                });
                ui.separator();
                ui.label("Add bytes:");
                ui.add(
                    egui::DragValue::new(&mut self.add_bytes_n)
                        .range(1..=1 << 20)
                        .speed(8.0),
                );
                if ui.button("Add").clicked() {
                    actions.push(Action::AddBytes(view_class, self.add_bytes_n));
                }
                for n in [64usize, 256, 1024, 4096] {
                    if ui.button(format!("+{n}")).clicked() {
                        actions.push(Action::AddBytes(view_class, n));
                    }
                }
                ui.separator();
                ui.label("Array:");
                let elems = array_elem_kinds();
                let cur = self.array_elem.min(elems.len() - 1);
                egui::ComboBox::from_id_salt("arr_elem")
                    .selected_text(elems[cur].0)
                    .show_ui(ui, |ui| {
                        for (i, (label, _)) in elems.iter().enumerate() {
                            ui.selectable_value(&mut self.array_elem, i, *label);
                        }
                    });
                ui.label("×");
                ui.add(
                    egui::DragValue::new(&mut self.array_count)
                        .range(1..=1 << 20)
                        .speed(1.0),
                );
                if ui.button("Add array").clicked() {
                    let elem = elems[cur].1.clone();
                    actions.push(Action::AddArray(view_class, elem, self.array_count));
                }
                ui.separator();
                if ui.button("Expand all").clicked() {
                    actions.push(Action::ExpandAll);
                }
                if ui.button("Collapse all").clicked() {
                    actions.push(Action::CollapseAll);
                }
            });
            ui.separator();

            // Delete key removes the selected rows (when not editing a field)
            if self.editing.is_none()
                && !self.selected.is_empty()
                && ui.input(|i| i.key_pressed(egui::Key::Delete))
            {
                actions.push(Action::DeleteSelected);
            }

            self.node_table(ui, view_class, rows, actions);
        });
    }

    fn node_table(
        &mut self,
        ui: &mut egui::Ui,
        view_class: ClassId,
        rows: &[Row],
        actions: &mut Vec<Action>,
    ) {
        let sel = self.state.selected_view;
        let visible: Vec<&Row> = rows.iter().filter(|r| r.root == sel).collect();
        let row_h = ui.spacing().interact_size.y;

        // header, left-aligned to the same fixed column widths
        ui.horizontal(|ui| {
            for (wid, name) in [
                (w::OFFSET, "Offset"),
                (w::ADDRESS, "Address"),
                (w::TYPE, "Type"),
                (w::NAME, "Name"),
            ] {
                ui.allocate_ui_with_layout(
                    egui::vec2(wid, row_h),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| ui.label(egui::RichText::new(name).strong()),
                );
            }
            // Value / Bytes / Comment are content-sized in the rows below
            ui.label(egui::RichText::new("Value").strong());
            ui.separator();
            ui.label(egui::RichText::new("Bytes / Comment").strong());
        });
        ui.separator();

        if visible.is_empty() {
            ui.weak("(no fields — use the Add field / Add bytes / Array controls above)");
            return;
        }

        // Vertically virtualized (only on-screen rows are laid out) and
        // horizontally scrollable so nothing is cut off in a narrow window.
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .id_salt("nodes")
            .show_rows(ui, row_h, visible.len(), |ui, range| {
                for i in range {
                    self.node_row(ui, view_class, &visible, i, row_h, actions);
                }
            });
    }

    fn node_row(
        &mut self,
        ui: &mut egui::Ui,
        view_class: ClassId,
        visible: &[&Row],
        idx: usize,
        row_h: f32,
        actions: &mut Vec<Action>,
    ) {
        let row = visible[idx];
        // fade a highlight into the value/bytes when they just changed
        let flash = if self.settings.flash_enabled {
            self.flash.factor(row.root, &row.path, self.now)
        } else {
            0.0
        };
        let flash_col = self.settings.flash_color();
        let value_color = mix(flash_col, col::VALUE, flash);
        let bytes_color = mix(flash_col, col::HEX, flash);
        ui.horizontal(|ui| {
            ui.set_height(row_h);
            // offset cell doubles as the row selector
            let selected = self.selected.contains(&row.path);
            let off = egui::Button::selectable(
                selected,
                egui::RichText::new(format!("0x{:04X}", row.offset))
                    .monospace()
                    .color(col::OFFSET),
            );
            let off_resp = ui.add_sized([w::OFFSET, row_h], off);
            if off_resp.clicked() {
                let mods = ui.input(|i| i.modifiers);
                self.select_row(visible, idx, mods);
            }
            off_resp.context_menu(|ui| {
                self.row_context_menu(ui, view_class, row, actions);
            });
            cell_label(
                ui,
                w::ADDRESS,
                row_h,
                format!("0x{:012X}", row.address),
                col::ADDRESS,
            );

            // type cell: indent + expand toggle + type menu (opens on left-click)
            ui.allocate_ui_with_layout(
                egui::vec2(w::TYPE, row_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add_space(row.depth as f32 * 12.0);
                    if row.expandable {
                        let tri = if row.expanded { "▼" } else { "▶" };
                        if ui.small_button(tri).clicked() {
                            match &row.kind {
                                NodeKind::Pointer => {
                                    if let Some((owner, idx)) =
                                        self.state.resolve_owner(view_class, &row.path)
                                    {
                                        actions.push(Action::ExpandPointer {
                                            owner,
                                            idx,
                                            root: row.root,
                                            path: row.path.clone(),
                                        });
                                    }
                                }
                                NodeKind::ClassPtr { .. } => {
                                    actions.push(Action::Toggle(row.root, row.path.clone()));
                                }
                                // arrays & inline class instances collapse/expand
                                _ => {
                                    actions
                                        .push(Action::ToggleCollapse(row.root, row.path.clone()));
                                }
                            }
                        }
                    }
                    ui.menu_button(
                        egui::RichText::new(&row.type_label)
                            .monospace()
                            .color(col::TYPE),
                        |ui| {
                            self.type_change_menu(ui, view_class, row, actions);
                        },
                    );
                },
            );

            self.edit_cell(
                ui,
                row,
                EditField::Name,
                &row.name,
                Some(w::NAME),
                row_h,
                col::NAME,
                actions,
            );
            // Value / Bytes / Comment are content-sized so the full text shows
            if row.kind.is_editable() {
                self.edit_cell(
                    ui,
                    row,
                    EditField::Value,
                    &row.value,
                    None,
                    row_h,
                    value_color,
                    actions,
                );
            } else {
                flow_label(ui, &row.value, value_color);
            }
            ui.separator();
            flow_label(ui, &row.hex, bytes_color);
            ui.separator();
            self.edit_cell(
                ui,
                row,
                EditField::Comment,
                &row.comment,
                None,
                row_h,
                col::COMMENT,
                actions,
            );
        });
    }

    /// Update the selection set from a click on row `idx` (plain = replace,
    /// Ctrl/Cmd = toggle, Shift = range from the anchor).
    fn select_row(&mut self, visible: &[&Row], idx: usize, mods: egui::Modifiers) {
        let path = visible[idx].path.clone();
        if mods.shift {
            if let Some(a) = self.sel_anchor.filter(|&a| a < visible.len()) {
                let (lo, hi) = if a <= idx { (a, idx) } else { (idx, a) };
                self.selected.clear();
                for r in &visible[lo..=hi] {
                    self.selected.insert(r.path.clone());
                }
            } else {
                self.selected.insert(path);
                self.sel_anchor = Some(idx);
            }
        } else if mods.command {
            if !self.selected.remove(&path) {
                self.selected.insert(path);
            }
            self.sel_anchor = Some(idx);
        } else {
            self.selected.clear();
            self.selected.insert(path);
            self.sel_anchor = Some(idx);
        }
    }

    /// Render an inline editor (sized) if this row+field is being edited,
    /// otherwise a double-clickable sized label.
    #[allow(clippy::too_many_arguments)]
    fn edit_cell(
        &mut self,
        ui: &mut egui::Ui,
        row: &Row,
        field: EditField,
        text: &str,
        width: Option<f32>,
        height: f32,
        color: egui::Color32,
        actions: &mut Vec<Action>,
    ) {
        let is_editing = self
            .editing
            .as_ref()
            .is_some_and(|e| e.root == row.root && e.path == row.path && e.field == field);
        if is_editing {
            if let Some(ed) = self.editing.as_mut() {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut ed.buf).desired_width(width.unwrap_or(180.0)),
                );
                if !ed.focused {
                    resp.request_focus();
                    ed.focused = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    actions.push(Action::CancelEdit);
                } else if resp.lost_focus() {
                    actions.push(Action::CommitEdit);
                }
            }
        } else {
            let display = if text.is_empty() { "—" } else { text };
            let rt = egui::RichText::new(display).monospace().color(color);
            let resp = match width {
                // fixed-width, ellipsized (aligned columns: Name)
                Some(w) => {
                    let label = egui::Label::new(rt).truncate().sense(egui::Sense::click());
                    ui.allocate_ui_with_layout(
                        egui::vec2(w, height),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| ui.add(label),
                    )
                    .inner
                }
                // content-sized, never truncated (Value/Comment show in full)
                None => ui.add(
                    egui::Label::new(rt)
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .sense(egui::Sense::click()),
                ),
            };
            if resp.double_clicked() {
                let buf = strip_quotes(text);
                actions.push(Action::StartEdit(row.root, row.path.clone(), field, buf));
            }
        }
    }

    /// Left-click menu on the Type cell: change the node's type only.
    fn type_change_menu(
        &mut self,
        ui: &mut egui::Ui,
        view_class: ClassId,
        row: &Row,
        actions: &mut Vec<Action>,
    ) {
        let owner = self.state.resolve_owner(view_class, &row.path);
        for (label, kind) in scalar_kinds() {
            if ui.button(label).clicked() {
                if let Some((cls, idx)) = owner {
                    actions.push(Action::ChangeKind(cls, idx, kind));
                }
                ui.close();
            }
        }
        ui.menu_button("asm sizes", |ui| {
            for (label, kind) in asm_size_kinds() {
                if ui.button(label).clicked() {
                    if let Some((cls, idx)) = owner {
                        actions.push(Action::ChangeKind(cls, idx, kind));
                    }
                    ui.close();
                }
            }
        });
        ui.menu_button("Array…", |ui| {
            ui.horizontal(|ui| {
                ui.label("count");
                ui.add(egui::DragValue::new(&mut self.array_count).range(1..=1 << 20));
            });
            ui.separator();
            for (label, ekind) in array_elem_kinds() {
                if ui
                    .button(format!("{label} × {}", self.array_count))
                    .clicked()
                {
                    if let Some((cls, idx)) = owner {
                        actions.push(Action::ChangeKind(
                            cls,
                            idx,
                            NodeKind::Array {
                                element: Box::new(ekind),
                                count: self.array_count,
                            },
                        ));
                    }
                    ui.close();
                }
            }
        });
        ui.menu_button("Class instance", |ui| {
            for id in self.state.project.registry.ids() {
                let name = self
                    .state
                    .project
                    .registry
                    .name_of(id)
                    .unwrap_or("?")
                    .to_string();
                if ui.button(&name).clicked() {
                    if let Some((cls, idx)) = owner {
                        actions.push(Action::ChangeKind(
                            cls,
                            idx,
                            NodeKind::ClassInstance { class_id: id },
                        ));
                    }
                    ui.close();
                }
            }
        });
        ui.menu_button("Class pointer", |ui| {
            for id in self.state.project.registry.ids() {
                let name = self
                    .state
                    .project
                    .registry
                    .name_of(id)
                    .unwrap_or("?")
                    .to_string();
                if ui.button(&name).clicked() {
                    if let Some((cls, idx)) = owner {
                        actions.push(Action::ChangeKind(
                            cls,
                            idx,
                            NodeKind::ClassPtr { class_id: id },
                        ));
                    }
                    ui.close();
                }
            }
        });
        if matches!(row.kind, NodeKind::Array { .. }) {
            ui.menu_button("Array length…", |ui| {
                ui.add(egui::DragValue::new(&mut self.array_count).range(1..=1 << 20));
                if ui
                    .button(format!("Set length = {}", self.array_count))
                    .clicked()
                {
                    if let Some((cls, idx)) = owner {
                        actions.push(Action::SetArrayCount(cls, idx, self.array_count));
                    }
                    ui.close();
                }
            });
        }
    }

    /// Right-click context menu on a row: rename, structure edits, and (for
    /// pointer/instance nodes) growing the target class without opening it.
    fn row_context_menu(
        &self,
        ui: &mut egui::Ui,
        view_class: ClassId,
        row: &Row,
        actions: &mut Vec<Action>,
    ) {
        let owner = self.state.resolve_owner(view_class, &row.path);
        if ui.button("Rename…").clicked() {
            actions.push(Action::StartEdit(
                row.root,
                row.path.clone(),
                EditField::Name,
                row.name.clone(),
            ));
            ui.close();
        }
        if ui.button("Edit comment…").clicked() {
            actions.push(Action::StartEdit(
                row.root,
                row.path.clone(),
                EditField::Comment,
                row.comment.clone(),
            ));
            ui.close();
        }
        ui.separator();
        if ui.button("Insert Int32 below").clicked() {
            if let Some((cls, idx)) = owner {
                actions.push(Action::InsertAfter(cls, idx, NodeKind::Int(IntWidth::W32)));
            }
            ui.close();
        }
        if ui.button("Delete node").clicked() {
            if let Some((cls, idx)) = owner {
                actions.push(Action::DeleteNode(cls, idx));
            }
            ui.close();
        }
        if !self.selected.is_empty()
            && ui
                .button(format!("Delete selected ({})", self.selected.len()))
                .clicked()
        {
            actions.push(Action::DeleteSelected);
            ui.close();
        }
        let target = match &row.kind {
            NodeKind::ClassPtr { class_id } | NodeKind::ClassInstance { class_id } => {
                Some(*class_id)
            }
            _ => None,
        };
        if let Some(tc) = target {
            ui.separator();
            ui.menu_button("Add bytes to target", |ui| {
                for n in [64usize, 256, 1024, 4096] {
                    if ui.button(format!("+{n}")).clicked() {
                        actions.push(Action::AddBytes(tc, n));
                        ui.close();
                    }
                }
            });
        }
    }

    fn memory_map_window(&mut self, ctx: &egui::Context) {
        if !self.show_memory_map {
            return;
        }
        let mut open = self.show_memory_map;
        egui::Window::new("Memory map")
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("{} regions", self.state.regions.len()));
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("maps")
                        .num_columns(4)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Start");
                            ui.strong("End");
                            ui.strong("Perms");
                            ui.strong("Path");
                            ui.end_row();
                            for r in &self.state.regions {
                                ui.monospace(format!("0x{:012X}", r.start));
                                ui.monospace(format!("0x{:012X}", r.end));
                                ui.monospace(r.perms.to_string());
                                ui.label(r.path.as_deref().unwrap_or(""));
                                ui.end_row();
                            }
                        });
                });
            });
        self.show_memory_map = open;
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        let mut changed = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Value-change highlight");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .checkbox(&mut self.settings.flash_enabled, "enabled")
                                .changed();
                            ui.add_enabled_ui(self.settings.flash_enabled, |ui| {
                                changed |= ui
                                    .color_edit_button_srgb(&mut self.settings.flash_color)
                                    .changed();
                            });
                        });
                        ui.end_row();

                        ui.label("Fade duration");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.settings.flash_secs)
                                    .range(0.1..=5.0)
                                    .speed(0.05)
                                    .suffix(" s"),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Default field type");
                        let cur = scalar_kinds()
                            .into_iter()
                            .find(|(_, k)| *k == self.settings.default_kind)
                            .map(|(l, _)| l)
                            .unwrap_or("(custom)");
                        egui::ComboBox::from_id_salt("default_kind")
                            .selected_text(cur)
                            .show_ui(ui, |ui| {
                                for (label, kind) in scalar_kinds() {
                                    changed |= ui
                                        .selectable_value(
                                            &mut self.settings.default_kind,
                                            kind,
                                            label,
                                        )
                                        .changed();
                                }
                            });
                        ui.end_row();

                        ui.label("Seed rows (new class)");
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.settings.seed_rows).range(0..=256))
                            .changed();
                        ui.end_row();

                        ui.label("Max array elements");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.settings.array_cap).range(16..=8192),
                            )
                            .changed();
                        ui.end_row();
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Reset to defaults").clicked() {
                        self.settings = Settings::default();
                        changed = true;
                    }
                    ui.weak(format!("saved to {}", settings_file().display()));
                });
                ui.weak("Type / seed rows apply to newly created classes.");
            });
        self.show_settings = open;
        if changed {
            self.state.engine.set_array_limit(self.settings.array_cap);
            self.settings.save();
        }
    }

    fn codegen_window(&mut self, ctx: &egui::Context) {
        if !self.show_codegen {
            return;
        }
        let mut open = self.show_codegen;
        let mut regen = self.codegen_cache.is_empty();
        egui::Window::new("Code generation")
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (label, lang) in [
                        ("Rust", Language::Rust),
                        ("C", Language::C),
                        ("C++", Language::Cpp),
                    ] {
                        if ui
                            .selectable_label(self.codegen_lang == lang, label)
                            .clicked()
                        {
                            self.codegen_lang = lang;
                            regen = true;
                        }
                    }
                    if ui.button("Regenerate").clicked() {
                        regen = true;
                    }
                });
                if regen {
                    self.codegen_cache = self.state.codegen(self.codegen_lang);
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.codegen_cache.as_str())
                            .code_editor()
                            .desired_width(f32::INFINITY),
                    );
                });
            });
        self.show_codegen = open;
    }
}

/// A fixed-width, monospace, colored label cell (non-editable columns).
fn cell_label(ui: &mut egui::Ui, width: f32, height: f32, text: String, color: egui::Color32) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).monospace().color(color)).truncate());
        },
    );
}

/// A content-sized, monospace, colored, single-line label that is never
/// truncated — the full text is shown (the table scrolls horizontally for it).
fn flow_label(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    let display = if text.is_empty() { "—" } else { text };
    ui.add(
        egui::Label::new(egui::RichText::new(display).monospace().color(color))
            .wrap_mode(egui::TextWrapMode::Extend),
    );
}

/// Blend `flash` into `base` by `t` (t=1 → flash, t=0 → base). Used to fade the
/// value-changed highlight back to the normal column color.
fn mix(flash: egui::Color32, base: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let c = |a: u8, b: u8| (a as f32 * t + b as f32 * (1.0 - t)).round() as u8;
    egui::Color32::from_rgb(
        c(flash.r(), base.r()),
        c(flash.g(), base.g()),
        c(flash.b(), base.b()),
    )
}

fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

fn scalar_kinds() -> Vec<(&'static str, NodeKind)> {
    vec![
        ("Hex8", NodeKind::Hex(IntWidth::W8)),
        ("Hex16", NodeKind::Hex(IntWidth::W16)),
        ("Hex32", NodeKind::Hex(IntWidth::W32)),
        ("Hex64", NodeKind::Hex(IntWidth::W64)),
        ("Int8", NodeKind::Int(IntWidth::W8)),
        ("Int16", NodeKind::Int(IntWidth::W16)),
        ("Int32", NodeKind::Int(IntWidth::W32)),
        ("Int64", NodeKind::Int(IntWidth::W64)),
        ("UInt8", NodeKind::UInt(IntWidth::W8)),
        ("UInt16", NodeKind::UInt(IntWidth::W16)),
        ("UInt32", NodeKind::UInt(IntWidth::W32)),
        ("UInt64", NodeKind::UInt(IntWidth::W64)),
        ("Float", NodeKind::Float32),
        ("Double", NodeKind::Float64),
        ("Bool", NodeKind::Bool),
        ("Vec2", NodeKind::Vec2),
        ("Vec3", NodeKind::Vec3),
        ("Vec4", NodeKind::Vec4),
        (
            "Text[32]",
            NodeKind::Text {
                encoding: TextEncoding::Utf8,
                len: 32,
            },
        ),
        (
            "WText[32]",
            NodeKind::Text {
                encoding: TextEncoding::Utf16,
                len: 32,
            },
        ),
        ("Pointer", NodeKind::Pointer),
        ("FnPtr", NodeKind::FunctionPtr),
        ("Padding[8]", NodeKind::Padding(8)),
        ("Unknown[8]", NodeKind::Unknown(8)),
    ]
}

/// Assembly data-size keywords as fixed-size fields: 1/2/4/8 bytes map to
/// editable `Hex` words, 10/16/32/64 to raw `Unknown` blocks.
fn asm_size_kinds() -> [(&'static str, NodeKind); 8] {
    [
        ("byte / DB (1)", NodeKind::Hex(IntWidth::W8)),
        ("word / DW (2)", NodeKind::Hex(IntWidth::W16)),
        ("dword / DD (4)", NodeKind::Hex(IntWidth::W32)),
        ("qword / DQ (8)", NodeKind::Hex(IntWidth::W64)),
        ("tword / DT (10)", NodeKind::Unknown(10)),
        ("oword / DO (16)", NodeKind::Unknown(16)),
        ("yword / DY (32)", NodeKind::Unknown(32)),
        ("zword / DZ (64)", NodeKind::Unknown(64)),
    ]
}

/// Element types offered by the toolbar array builder.
fn array_elem_kinds() -> [(&'static str, NodeKind); 8] {
    [
        ("byte", NodeKind::Hex(IntWidth::W8)),
        ("word", NodeKind::Hex(IntWidth::W16)),
        ("dword", NodeKind::Hex(IntWidth::W32)),
        ("qword", NodeKind::Hex(IntWidth::W64)),
        ("Int32", NodeKind::Int(IntWidth::W32)),
        ("UInt32", NodeKind::UInt(IntWidth::W32)),
        ("Float", NodeKind::Float32),
        ("Pointer", NodeKind::Pointer),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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
