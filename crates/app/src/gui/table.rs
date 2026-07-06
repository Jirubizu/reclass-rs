//! The central panel: tab strip, class/address bar, add-field toolbar, and the
//! virtualized node table with inline editing, selection, and type menus.

use eframe::egui;
use reclass_core::{ClassId, IntWidth, NodeKind, Row};

use super::widgets::{
    array_elem_kinds, asm_size_kinds, cell_label, flow_label, mix, scalar_kinds, strip_quotes,
};
use super::{Action, EditField, ReClassApp, col, w};

impl ReClassApp {
    pub(super) fn central(
        &mut self,
        root_ui: &mut egui::Ui,
        rows: &[Row],
        actions: &mut Vec<Action>,
    ) {
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
                    let type_resp = ui
                        .menu_button(
                            egui::RichText::new(&row.type_label)
                                .monospace()
                                .color(col::TYPE),
                            |ui| {
                                self.type_change_menu(ui, view_class, row, actions);
                            },
                        )
                        .response;
                    // Distinct popup id so right-click (context menu) and left-click
                    // (type menu) don't share state and open together.
                    egui::Popup::context_menu(&type_resp)
                        .id(type_resp.id.with("row_ctx"))
                        .show(|ui| {
                            self.row_context_menu(ui, view_class, row, actions);
                        });
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
            if ui.button("View as root").clicked() {
                // Root the target class where this node currently lives: deref the
                // pointer for ClassPtr, use the instance address for ClassInstance.
                // ponytail: snapshots this node's address; re-invoke if the parent
                // base moves.
                let expr = match &row.kind {
                    NodeKind::ClassPtr { .. } => format!("[0x{:X}]", row.address),
                    _ => format!("0x{:X}", row.address),
                };
                actions.push(Action::SetExpr(tc, expr));
                actions.push(Action::OpenView(tc));
                ui.close();
            }
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
}
