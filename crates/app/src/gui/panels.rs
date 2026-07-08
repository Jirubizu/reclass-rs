//! Menu bar, left side panel (process picker + class list), and the modal
//! windows (file browser, memory map, settings, code generation).

use eframe::egui;
use reclass_core::codegen::Language;

use super::settings::{Settings, settings_file};
use super::widgets::scalar_kinds;
use super::{Action, FileMode, ReClassApp, col};

impl ReClassApp {
    /// Render the in-app file browser (when open) and push `Load`/`Save` on confirm.
    pub(super) fn file_dialog_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(mut fd) = self.file_dialog.take() else {
            return;
        };
        let title = match fd.mode {
            FileMode::Open => "Open project",
            FileMode::Save => "Save project as",
            FileMode::GenProject => "Generate vmem project into…",
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
                            } else if name.ends_with(".ron") && fd.mode != FileMode::GenProject {
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
                    if fd.mode == FileMode::GenProject {
                        if ui.button("Generate here").clicked() {
                            confirm = true;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                        ui.weak("creates Cargo.toml + src/ here");
                    } else {
                        ui.label("File:");
                        let r = ui.text_edit_singleline(&mut fd.filename);
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            confirm = true;
                        }
                        let label = if fd.mode == FileMode::Open {
                            "Open"
                        } else {
                            "Save"
                        };
                        if ui.button(label).clicked() {
                            confirm = true;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    }
                });
                if let Some(e) = &fd.error {
                    ui.colored_label(col::OFFSET, e);
                }
            });

        if confirm {
            match fd.mode {
                FileMode::GenProject => {
                    let dir = fd.dir.to_string_lossy().into_owned();
                    actions.push(Action::GenerateProject(dir));
                    keep = false;
                }
                FileMode::Open | FileMode::Save if !fd.filename.trim().is_empty() => {
                    let mut name = fd.filename.trim().to_string();
                    if !name.ends_with(".ron") {
                        name.push_str(".ron");
                    }
                    let path = fd.dir.join(&name).to_string_lossy().into_owned();
                    if fd.mode == FileMode::Open {
                        actions.push(Action::Load(path));
                    } else {
                        actions.push(Action::Save(path));
                    }
                    keep = false;
                }
                _ => {}
            }
        }
        if window_open && keep {
            self.file_dialog = Some(fd);
        }
    }

    pub(super) fn menu_bar(&mut self, root_ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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
                    ui.separator();
                    if ui.button("Generate vmem project…").clicked() {
                        self.open_file_dialog(FileMode::GenProject);
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

    pub(super) fn side_panel(&mut self, root_ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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

    pub(super) fn memory_map_window(&mut self, ctx: &egui::Context) {
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

    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
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

                        ui.label("MCP control server");
                        ui.horizontal(|ui| {
                            changed |= ui
                                .checkbox(&mut self.settings.mcp_enabled, "enabled")
                                .changed();
                            ui.add_enabled_ui(self.settings.mcp_enabled, |ui| {
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut self.settings.mcp_port)
                                            .range(1..=65535)
                                            .prefix("port "),
                                    )
                                    .changed();
                            });
                        });
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
                ui.weak("MCP server binds to 127.0.0.1 (loopback only).");
            });
        self.show_settings = open;
        if changed {
            self.state.engine.set_array_limit(self.settings.array_cap);
            self.settings.save();
        }
    }

    pub(super) fn codegen_window(&mut self, ctx: &egui::Context) {
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
