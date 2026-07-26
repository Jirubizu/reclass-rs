//! Cheat Engine `.CT` XML exporter. Captures rows from the latest snapshot
//! and maps scalar fields to CheatTable entries with absolute addresses.

use reclass::plugin::*;

/// Map a `NodeKind` to a CE variable type string (`vttype`).
fn ce_type(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Hex(IntWidth::W8) | NodeKind::UInt(IntWidth::W8) => "Byte",
        NodeKind::Hex(IntWidth::W16) | NodeKind::UInt(IntWidth::W16) => "2 Bytes",
        NodeKind::Hex(IntWidth::W32) | NodeKind::UInt(IntWidth::W32) => "4 Bytes",
        NodeKind::Hex(IntWidth::W64) | NodeKind::UInt(IntWidth::W64) => "8 Bytes",
        NodeKind::Int(IntWidth::W8) => "Byte",
        NodeKind::Int(IntWidth::W16) => "2 Bytes",
        NodeKind::Int(IntWidth::W32) => "4 Bytes",
        NodeKind::Int(IntWidth::W64) => "8 Bytes",
        NodeKind::Float32 => "Float",
        NodeKind::Float64 => "Double",
        NodeKind::Bool => "Byte",
        NodeKind::Vec2 => "Float",
        NodeKind::Vec3 => "Float",
        NodeKind::Vec4 => "Float",
        _ => "4 Bytes",
    }
}

/// XML-escape a string.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CheatTableExporter {
    /// Path for the output .CT file.
    path: String,
    /// Rows from the last snapshot (for export).
    #[serde(skip)]
    last_rows: Vec<CeRow>,
    /// Outcome of the last export, shown in the window.
    #[serde(skip)]
    status: Option<Result<String, String>>,
}

#[derive(Clone)]
struct CeRow {
    description: String,
    address: String,
    ce_type: String,
}

impl HostPlugin for CheatTableExporter {
    fn name(&self) -> &str {
        "Cheat Table Exporter"
    }
    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn on_snapshot(&mut self, rows: &[Row], state: &AppState) -> Vec<PluginAction> {
        let registry = state.registry();
        self.last_rows = rows
            .iter()
            .filter(|r| {
                // Only include addressable scalar nodes.
                matches!(
                    r.kind,
                    NodeKind::Hex(_)
                        | NodeKind::Int(_)
                        | NodeKind::UInt(_)
                        | NodeKind::Float32
                        | NodeKind::Float64
                        | NodeKind::Bool
                        | NodeKind::Vec2
                        | NodeKind::Vec3
                        | NodeKind::Vec4
                )
            })
            .map(|r| {
                let class_name = state
                    .view_class(r.root)
                    .and_then(|cid| registry.name_of(cid))
                    .unwrap_or("?");
                CeRow {
                    description: format!("{}.{}", class_name, r.name),
                    address: format!("{:#018X}", r.address),
                    ce_type: ce_type(&r.kind).to_string(),
                }
            })
            .collect();
        Vec::new()
    }

    fn has_window(&self) -> bool {
        true
    }
    fn show_window(&mut self, ctx: &egui::Context, _state: &AppState, open: &mut bool) {
        egui::Window::new("Cheat Table Exporter")
            .open(open)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(format!("{} scalar rows captured", self.last_rows.len()));
                ui.label("Output path:");
                ui.text_edit_singleline(&mut self.path);
                if ui.button("Export .CT").clicked() && !self.path.is_empty() {
                    let xml = build_ct(&self.last_rows);
                    let n = self.last_rows.len();
                    // The hook has no generic file-write action, so write
                    // directly — but report the outcome in this window rather
                    // than dropping it. A failed export used to look exactly
                    // like a successful one.
                    self.status = Some(match std::fs::write(&self.path, xml) {
                        Ok(()) => Ok(format!("wrote {n} entries to {}", self.path)),
                        Err(e) => Err(format!("{}: {e}", self.path)),
                    });
                }
                match &self.status {
                    Some(Ok(msg)) => {
                        ui.colored_label(egui::Color32::from_rgb(0x4C, 0xAF, 0x50), msg);
                    }
                    Some(Err(msg)) => {
                        ui.colored_label(egui::Color32::from_rgb(0xE5, 0x39, 0x35), msg);
                    }
                    None => {}
                }
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for r in &self.last_rows {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "{}  {}  {}",
                                    r.address, r.ce_type, r.description
                                ));
                            });
                        }
                    });
            });
    }

    fn save_settings(&self) -> Option<String> {
        save_json(self)
    }
    fn load_settings(&mut self, data: &str) -> bool {
        load_json(self, data)
    }
}

fn build_ct(rows: &[CeRow]) -> String {
    let mut entries = String::new();
    for (i, r) in rows.iter().enumerate() {
        entries.push_str(&format!(
            "  <CheatEntry>\n\
                   <ID>{id}</ID>\n\
                   <Description>\"{desc}\"</Description>\n\
                   <VariableType>{vt}</VariableType>\n\
                   <Address>{addr}</Address>\n\
                 </CheatEntry>\n",
            id = i,
            desc = escape(&r.description),
            vt = escape(&r.ce_type),
            addr = escape(&r.address),
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <CheatTable CheatEngineTableVersion=\"45\">\n\
         {entries}\
         </CheatTable>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_well_formed() {
        let rows = vec![
            CeRow {
                description: "Player.hp".into(),
                address: "0x00007FF700001234".into(),
                ce_type: "4 Bytes".into(),
            },
            CeRow {
                description: "Player.x".into(),
                address: "0x00007FF700001238".into(),
                ce_type: "Float".into(),
            },
        ];
        let xml = build_ct(&rows);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("Player.hp"));
        assert!(xml.contains("Player.x"));
        assert!(xml.contains("4 Bytes"));
        assert!(xml.contains("Float"));
    }
}
