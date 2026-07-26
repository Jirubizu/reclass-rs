//! Raw hex dump viewer for an arbitrary address. Reads through the backend
//! (synchronous, on the GUI thread during hook calls), renders a 16-byte-per-row
//! hex grid with an ASCII column.

use reclass::plugin::*;

/// Bytes per row in the dump.
const BYTES_PER_ROW: usize = 16;
/// Maximum bytes to fetch.
const MAX_BYTES: usize = 4096;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HexDump {
    /// Address as a hex string (e.g. "7ff80000").
    addr_input: String,
    /// Number of rows to show.
    rows_input: String,
    /// Fetched bytes from the last request.
    #[serde(skip)]
    bytes: Vec<u8>,
    /// First address of the fetched range.
    #[serde(skip)]
    base: u64,
    /// Error from the last read attempt, if any.
    #[serde(skip)]
    error: Option<String>,
}

impl HostPlugin for HexDump {
    fn name(&self) -> &str {
        "Hex Dump"
    }
    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn on_snapshot(&mut self, _rows: &[Row], _state: &AppState) -> Vec<PluginAction> {
        // Read on demand — triggered by a UI button, not every tick.
        // We use a simple flag to avoid re-reading every frame.
        Vec::new()
    }

    fn has_window(&self) -> bool {
        true
    }
    fn show_window(&mut self, ctx: &egui::Context, state: &AppState, open: &mut bool) {
        egui::Window::new("Hex Dump")
            .open(open)
            .resizable(true)
            .show(ctx, |ui| {
                egui::Grid::new("hexdump_inputs")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Address (hex):");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.addr_input).desired_width(200.0),
                        );
                        ui.end_row();
                        ui.label("Rows:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.rows_input).desired_width(200.0),
                        );
                        ui.end_row();
                    });
                if ui.button("Read").clicked() {
                    self.error = None;
                    let addr: u64 =
                        u64::from_str_radix(self.addr_input.trim_start_matches("0x"), 16)
                            .unwrap_or(0);
                    let rows: usize = self.rows_input.parse().unwrap_or(16);
                    let len = (rows * BYTES_PER_ROW).min(MAX_BYTES) as u64;
                    if let Some(backend) = state.backend() {
                        let mut buf = vec![0u8; len as usize];
                        match backend.read(addr, &mut buf) {
                            Ok(()) => {
                                self.bytes = buf;
                                self.base = addr;
                            }
                            Err(e) => {
                                self.bytes.clear();
                                self.error = Some(e.to_string());
                            }
                        }
                    } else {
                        self.error = Some("not attached".into());
                    }
                }
                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::RED, err);
                } else if !self.bytes.is_empty() {
                    ui.label(format!("0x{:X} — {} bytes:", self.base, self.bytes.len()));
                    ui.separator();
                    egui::ScrollArea::both()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            let mut header = format!("{:<8}  ", "Offset");
                            for i in 0..BYTES_PER_ROW {
                                header.push_str(&format!("{i:02X} "));
                            }
                            header.push_str(" ASCII");
                            ui.monospace(header);

                            for (row, chunk) in self.bytes.chunks(BYTES_PER_ROW).enumerate() {
                                let offset = self.base + row as u64 * BYTES_PER_ROW as u64;
                                let mut hex = String::new();
                                let mut ascii = String::new();
                                for &b in chunk {
                                    hex.push_str(&format!("{b:02X} "));
                                    ascii.push(if b.is_ascii_graphic() || b == b' ' {
                                        b as char
                                    } else {
                                        '.'
                                    });
                                }
                                // Pad short rows
                                for _ in chunk.len()..BYTES_PER_ROW {
                                    hex.push_str("   ");
                                }
                                ui.monospace(format!("{offset:08X}  {hex} {ascii}"));
                            }
                        });
                }
            });
    }

    fn save_settings(&self) -> Option<String> {
        save_json(self)
    }
    fn load_settings(&mut self, data: &str) -> bool {
        load_json(self, data)
    }
}
