//! Context-menu entries to copy a field's declaration in C, Rust, Python, or
//! JSON format. The declaration includes the field name, type, offset, and
//! (for JSON) the full node metadata. Output goes to the system clipboard.

use reclass::plugin::*;

/// Map a `NodeKind` to a C type string.
fn c_type(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Hex(IntWidth::W8) | NodeKind::UInt(IntWidth::W8) => "uint8_t",
        NodeKind::Hex(IntWidth::W16) | NodeKind::UInt(IntWidth::W16) => "uint16_t",
        NodeKind::Hex(IntWidth::W32) | NodeKind::UInt(IntWidth::W32) => "uint32_t",
        NodeKind::Hex(IntWidth::W64) | NodeKind::UInt(IntWidth::W64) => "uint64_t",
        NodeKind::Int(IntWidth::W8) => "int8_t",
        NodeKind::Int(IntWidth::W16) => "int16_t",
        NodeKind::Int(IntWidth::W32) => "int32_t",
        NodeKind::Int(IntWidth::W64) => "int64_t",
        NodeKind::Float32 => "float",
        NodeKind::Float64 => "double",
        NodeKind::Bool => "bool",
        NodeKind::Pointer | NodeKind::ClassPtr { .. } => "void*",
        NodeKind::Vec2 => "float[2]",
        NodeKind::Vec3 => "float[3]",
        NodeKind::Vec4 => "float[4]",
        NodeKind::Text { .. } => "char*",
        NodeKind::Array { element, .. } => c_type(element),
        _ => "void*",
    }
}

/// Map a `NodeKind` to a Rust type string.
fn rust_type(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Hex(IntWidth::W8) | NodeKind::UInt(IntWidth::W8) => "u8",
        NodeKind::Hex(IntWidth::W16) | NodeKind::UInt(IntWidth::W16) => "u16",
        NodeKind::Hex(IntWidth::W32) | NodeKind::UInt(IntWidth::W32) => "u32",
        NodeKind::Hex(IntWidth::W64) | NodeKind::UInt(IntWidth::W64) => "u64",
        NodeKind::Int(IntWidth::W8) => "i8",
        NodeKind::Int(IntWidth::W16) => "i16",
        NodeKind::Int(IntWidth::W32) => "i32",
        NodeKind::Int(IntWidth::W64) => "i64",
        NodeKind::Float32 => "f32",
        NodeKind::Float64 => "f64",
        NodeKind::Bool => "bool",
        NodeKind::Pointer | NodeKind::ClassPtr { .. } => "*const ()",
        NodeKind::Vec2 => "[f32; 2]",
        NodeKind::Vec3 => "[f32; 3]",
        NodeKind::Vec4 => "[f32; 4]",
        NodeKind::Text { .. } => "&str",
        NodeKind::Array { element, .. } => rust_type(element),
        _ => "()",
    }
}

/// Map a `NodeKind` to a Python type string.
fn py_type(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Int(_) | NodeKind::UInt(_) | NodeKind::Hex(_) => "int",
        NodeKind::Float32 | NodeKind::Float64 => "float",
        NodeKind::Bool => "bool",
        NodeKind::Text { .. } => "str",
        _ => "int",
    }
}

#[derive(Default)]
pub struct CopyAs {
    // stateless — pure formatting from registry
}

impl HostPlugin for CopyAs {
    fn name(&self) -> &str {
        "Copy As"
    }
    fn version(&self) -> (u32, u32) {
        (0, 1)
    }

    fn context_menu_entries(&self) -> &[(&str, &str)] {
        &[
            ("copy_c", "Copy as C"),
            ("copy_rust", "Copy as Rust"),
            ("copy_py", "Copy as Python"),
            ("copy_json", "Copy as JSON"),
        ]
    }

    fn on_context_menu(
        &mut self,
        id: &str,
        class: ClassId,
        idx: usize,
        state: &AppState,
    ) -> Vec<PluginAction> {
        let registry = state.registry();
        let Some(cls) = registry.get(class) else {
            return Vec::new();
        };
        let Some(node) = cls.nodes.get(idx) else {
            return Vec::new();
        };
        let offset = registry.offset_of(class, idx).unwrap_or(0);
        let text = match id {
            "copy_c" => {
                format!(
                    "/* offset 0x{offset:02X} */ {ty} {name};",
                    ty = c_type(&node.kind),
                    name = node.name,
                )
            }
            "copy_rust" => {
                format!(
                    "// offset 0x{offset:02X}\n{name}: {ty},",
                    name = node.name,
                    ty = rust_type(&node.kind),
                )
            }
            "copy_py" => {
                format!(
                    "# offset 0x{offset:02X} — read as {ty}\n{name}",
                    name = (&node.name).replace(' ', "_"),
                    ty = py_type(&node.kind),
                )
            }
            "copy_json" => serde_json::json!({
                "name": node.name,
                "kind": format!("{:?}", node.kind),
                "comment": node.comment,
                "class": cls.name,
                "offset_hex": format!("0x{offset:02X}"),
            })
            .to_string(),
            _ => return Vec::new(),
        };
        vec![PluginAction::SetClipboard(text)]
    }
}
