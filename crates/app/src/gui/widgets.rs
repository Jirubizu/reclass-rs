//! Small egui label helpers, node-kind palettes, and class seeding — the
//! leaf, `ReClassApp`-independent pieces of the front-end.

use eframe::egui;
use reclass_core::{ClassId, IntWidth, Node, NodeKind, TextEncoding};

use crate::app_state::AppState;

/// Seed `cid` with `rows` fields of `kind` so a fresh class shows memory at once.
pub(super) fn seed_class(state: &mut AppState, cid: ClassId, kind: &NodeKind, rows: usize) {
    let nodes = (0..rows).map(|i| Node::new(format!("field_{i}"), kind.clone()));
    let _ = state.project.registry.push_nodes(cid, nodes);
}

/// A fixed-width, monospace, colored label cell (non-editable columns).
pub(super) fn cell_label(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    text: String,
    color: egui::Color32,
) {
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
pub(super) fn flow_label(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    let display = if text.is_empty() { "—" } else { text };
    ui.add(
        egui::Label::new(egui::RichText::new(display).monospace().color(color))
            .wrap_mode(egui::TextWrapMode::Extend),
    );
}

/// Blend `flash` into `base` by `t` (t=1 → flash, t=0 → base). Used to fade the
/// value-changed highlight back to the normal column color.
pub(super) fn mix(flash: egui::Color32, base: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let c = |a: u8, b: u8| (a as f32 * t + b as f32 * (1.0 - t)).round() as u8;
    egui::Color32::from_rgb(
        c(flash.r(), base.r()),
        c(flash.g(), base.g()),
        c(flash.b(), base.b()),
    )
}

pub(super) fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

pub(super) fn scalar_kinds() -> Vec<(&'static str, NodeKind)> {
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
pub(super) fn asm_size_kinds() -> [(&'static str, NodeKind); 8] {
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
pub(super) fn array_elem_kinds() -> [(&'static str, NodeKind); 8] {
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
