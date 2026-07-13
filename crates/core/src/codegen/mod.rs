//! Struct emission.
//!
//! Emits C, C++, and Rust definitions from the [`ClassRegistry`]. The model is
//! byte-packed (offset `i` = sum of sizes `0..i`), so Rust output is
//! `#[repr(C, packed)]` and C/C++ output is `#pragma pack(push, 1)`, making the
//! generated `size_of` / field offsets exactly match the model. Each field
//! carries its offset as a comment.
//!
//! Split by target: [`rust`] (plain `#[repr(C, packed)]` structs), [`c`] (C/C++
//! with forward declarations), and [`project`] (a standalone `vmem`-backed Cargo
//! project). This module owns the [`Language`] dispatch and the naming/ordering
//! helpers shared across all three.

use crate::class::{ClassId, ClassRegistry};
use crate::node::NodeKind;

mod c;
mod project;
mod rust;

pub use project::{GenFile, generate_project};

/// Target output language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    /// C99 (`struct`s + typedefs, `#pragma pack`).
    C,
    /// C++ (`struct`s with forward declarations).
    Cpp,
    /// Rust (`#[repr(C, packed)]`).
    Rust,
}

/// Generate definitions for every class in the registry.
pub fn generate(reg: &ClassRegistry, lang: Language) -> String {
    match lang {
        Language::Rust => rust::generate(reg),
        Language::C | Language::Cpp => c::generate(reg, lang),
    }
}

pub(super) fn sanitize(name: &str, fallback: impl Fn() -> String) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        let ok = ch.is_ascii_alphanumeric() || ch == '_';
        if ok && !(i == 0 && ch.is_ascii_digit()) {
            out.push(ch);
        } else if ok && i == 0 {
            out.push('_');
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() { fallback() } else { out }
}

/// Escape a (already char-sanitized) identifier that collides with a Rust
/// keyword: raw form `r#kw`, or a trailing `_` for the few keywords that cannot
/// be raw (`crate`, `self`, `super`, `Self`).
pub(super) fn rust_ident(name: &str) -> String {
    const NON_RAW: [&str; 4] = ["crate", "self", "super", "Self"];
    #[rustfmt::skip]
    const KEYWORDS: [&str; 49] = [
        "as", "break", "const", "continue", "crate", "else", "enum", "extern",
        "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "Self", "static",
        "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
        "while", "async", "await", "dyn", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "typeof", "yield", "gen",
    ];
    if NON_RAW.contains(&name) {
        format!("{name}_")
    } else if KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_string()
    }
}

pub(super) fn class_type_name(reg: &ClassRegistry, id: ClassId) -> String {
    match reg.name_of(id) {
        Some(n) => sanitize(n, || format!("Class{id}")),
        None => format!("Class{id}"),
    }
}

/// A class's Rust type name, keyword-escaped (`class_type_name` is shared with
/// C output, which does not need escaping).
pub(super) fn rust_type_name(reg: &ClassRegistry, id: ClassId) -> String {
    rust_ident(&class_type_name(reg, id))
}

/// Order classes so that any class embedded by value (`ClassInstance`, possibly
/// through arrays) appears before the class that embeds it. Falls back to id
/// order on a cycle (which `validate` would already reject).
pub(super) fn topo_order(reg: &ClassRegistry) -> Vec<ClassId> {
    fn inline_deps(kind: &NodeKind, acc: &mut Vec<ClassId>) {
        match kind {
            NodeKind::ClassInstance { class_id } => acc.push(*class_id),
            NodeKind::Array { element, .. } => inline_deps(element, acc),
            _ => {}
        }
    }

    fn visit(
        reg: &ClassRegistry,
        id: ClassId,
        visited: &mut std::collections::HashSet<ClassId>,
        on_stack: &mut std::collections::HashSet<ClassId>,
        order: &mut Vec<ClassId>,
    ) {
        if visited.contains(&id) || on_stack.contains(&id) {
            return;
        }
        on_stack.insert(id);
        if let Some(class) = reg.get(id) {
            for node in &class.nodes {
                let mut deps = Vec::new();
                inline_deps(&node.kind, &mut deps);
                for d in deps {
                    visit(reg, d, visited, on_stack, order);
                }
            }
        }
        on_stack.remove(&id);
        if visited.insert(id) {
            order.push(id);
        }
    }

    let mut visited = std::collections::HashSet::new();
    let mut on_stack = std::collections::HashSet::new();
    let mut order = Vec::new();

    for id in reg.ids() {
        visit(reg, id, &mut visited, &mut on_stack, &mut order);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassRegistry;
    use crate::node::{IntWidth, Node, NodeKind, TextEncoding};

    fn registry() -> (ClassRegistry, ClassId) {
        let mut reg = ClassRegistry::new();
        let inner = reg.add_class("Inner");
        reg.push_node(inner, Node::new("x", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.push_node(inner, Node::new("y", NodeKind::Float32))
            .unwrap();
        let outer = reg.add_class("Player");
        reg.push_node(outer, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.push_node(
            outer,
            Node::new("inner", NodeKind::ClassInstance { class_id: inner }),
        )
        .unwrap();
        reg.push_node(
            outer,
            Node::new(
                "scores",
                NodeKind::Array {
                    element: Box::new(NodeKind::UInt(IntWidth::W16)),
                    count: 4,
                },
            ),
        )
        .unwrap();
        reg.push_node(
            outer,
            Node::new("next", NodeKind::ClassPtr { class_id: outer }),
        )
        .unwrap();
        (reg, outer)
    }

    #[test]
    fn rust_output_has_repr_packed_and_offsets() {
        let (reg, _) = registry();
        let code = generate(&reg, Language::Rust);
        assert!(code.contains("#[repr(C, packed)]"));
        assert!(code.contains("pub struct Player"));
        assert!(code.contains("pub hp: i32, // 0x0"));
        assert!(code.contains("pub inner: Inner, // 0x4"));
        assert!(code.contains("pub scores: [u16; 4], // 0xC"));
        assert!(code.contains("pub next: *mut Player,"));
    }

    #[test]
    fn c_output_forward_declares_and_packs() {
        let (reg, _) = registry();
        let code = generate(&reg, Language::C);
        assert!(code.contains("#include <stdint.h>"));
        assert!(code.contains("struct Player;")); // forward decl
        assert!(code.contains("#pragma pack(push, 1)"));
        assert!(code.contains("int32_t hp;"));
        assert!(code.contains("struct Inner inner;"));
        assert!(code.contains("uint16_t scores[4];"));
        assert!(code.contains("struct Player* next;"));
        // Inner is defined before Player (topo order)
        let inner_pos = code.find("struct Inner {").unwrap();
        let player_pos = code.find("struct Player {").unwrap();
        assert!(inner_pos < player_pos);
    }

    #[test]
    fn cpp_uses_cstdint() {
        let (reg, _) = registry();
        let code = generate(&reg, Language::Cpp);
        assert!(code.contains("#include <cstdint>"));
    }

    #[test]
    fn project_gen_emits_cargo_structs_and_accessors() {
        let (reg, _) = registry();
        let files = generate_project(&reg, "My Game-Hack", Some("game"));
        let get = |p: &str| {
            files
                .iter()
                .find(|(name, _)| name == p)
                .map(|(_, c)| c.as_str())
                .unwrap_or_else(|| panic!("missing {p}"))
        };

        let cargo = get("Cargo.toml");
        assert!(cargo.contains("name = \"my_game_hack\"")); // sanitized + lowercased
        assert!(cargo.contains("vmem = { git = "));

        let code = get("src/generated.rs");
        // layout struct is reused verbatim from the Rust codegen
        assert!(code.contains("#[repr(C, packed)]"));
        assert!(code.contains("pub struct Player {"));
        // accessor + scalar get/set
        assert!(code.contains("pub struct PlayerView<'a> {"));
        assert!(code.contains("pub const SIZE: usize ="));
        assert!(code.contains("pub fn hp(&self) -> Result<i32, Error> {"));
        assert!(code.contains("pub fn set_hp(&self, value: i32) -> Result<(), Error> {"));
        assert!(code.contains("Ok(i32::from_le_bytes(b))"));
        assert!(code.contains("self.proc.read_bytes(self.base + 0x0, &mut b)?;"));
        // inline class instance -> nested accessor, no setter
        assert!(code.contains("pub fn inner(&self) -> InnerView<'a> {"));
        assert!(!code.contains("pub fn set_inner"));
        // array of scalars -> [T; N] get/set
        assert!(code.contains("pub fn scores(&self) -> Result<[u16; 4], Error> {"));
        // class pointer -> raw ptr + deref navigation
        assert!(code.contains("pub fn next_ptr(&self) -> Result<usize, Error> {"));
        assert!(code.contains("pub fn next(&self) -> Result<PlayerView<'a>, Error> {"));

        let main = get("src/main.rs");
        assert!(main.contains("mod generated;"));
        assert!(main.contains("Process::by_name(\"game\")?"));
        assert!(main.contains("InnerView::new(&proc, base)")); // root = first class (Inner)
    }

    #[test]
    fn project_gen_escapes_keywords_and_handles_large_arrays() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("entity");
        // Rust keyword field names must be raw-escaped in both struct + accessors.
        reg.push_node(c, Node::new("type", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.push_node(c, Node::new("move", NodeKind::Int(IntWidth::W8)))
            .unwrap();
        // A >32-element byte buffer would break bytemuck's Pod-based read::<[u8; N]>;
        // read_bytes has no such limit.
        reg.push_node(
            c,
            Node::new(
                "name",
                NodeKind::Text {
                    encoding: TextEncoding::Utf8,
                    len: 260,
                },
            ),
        )
        .unwrap();
        let code = &generate_project(&reg, "kw", None)[1].1;
        assert!(code.contains("pub r#type: i32,"));
        assert!(code.contains("pub r#move: i8,"));
        assert!(code.contains("pub fn r#type(&self) -> Result<i32, Error> {"));
        assert!(code.contains("pub fn set_type(&self, value: i32) -> Result<(), Error> {"));
        assert!(code.contains("pub fn r#move(&self) -> Result<i8, Error> {"));
        // large byte buffer via read_bytes, no Pod bound
        assert!(code.contains("pub fn name(&self) -> Result<[u8; 260], Error> {"));
        assert!(!code.contains("read::<"));
    }

    #[test]
    fn project_gen_emits_module_resolution_from_address_exprs() {
        let mut reg = ClassRegistry::new();
        let player = reg.add_class("Player");
        reg.push_node(player, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.set_address_expr(player, "<game> + 0x1000").unwrap();
        let entity = reg.add_class("Entity");
        reg.push_node(entity, Node::new("id", NodeKind::UInt(IntWidth::W32)))
            .unwrap();
        reg.set_address_expr(entity, "<engine.so> + 0x2000 - 0x10")
            .unwrap();

        let main = &generate_project(&reg, "addrexpr", Some("proc"))[2].1;
        // module resolution
        assert!(main.contains("mod_game_base: u64 = proc.module(\"game\")?.base as u64;"));
        assert!(
            main.contains("mod_engine_so_base: u64 = proc.module(\"engine.so\")?.base as u64;")
        );
        assert!(main.contains(
            "let player = PlayerView::new(&proc, mod_game_base.wrapping_add(0x1000_u64) as usize);"
        ));
        assert!(main.contains(
            "let entity = EntityView::new(&proc, mod_engine_so_base.wrapping_add(0x2000_u64).wrapping_sub(0x10_u64) as usize);"
        ));
        assert!(main.contains("Process::by_name(\"proc\")?"));
    }

    #[test]
    fn project_gen_emits_live_deref_for_pointer_expressions() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("Player");
        reg.push_node(c, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        // Deref: [0x5A3518]  → read pointer at the literal address
        reg.set_address_expr(c, "[0x5A3518]").unwrap();
        // Deref + module + offset (nested): [<game> + 0x100]
        let e = reg.add_class("Entity");
        reg.push_node(e, Node::new("id", NodeKind::UInt(IntWidth::W32)))
            .unwrap();
        reg.set_address_expr(e, "[<game> + 0x380]").unwrap();

        let main = &generate_project(&reg, "deref", Some("proc"))[2].1;
        assert!(main.contains("proc.read::<u64>(0x5A3518_u64 as usize)?"));
        assert!(main.contains("proc.read::<u64>(mod_game_base.wrapping_add(0x380_u64) as usize)?"));
        assert!(!main.contains("deref not static"));
    }

    #[test]
    fn project_gen_disambiguates_colliding_module_var_names() {
        // `a.b` and `a-b` both sanitize to `a_b`; each must still get its own var.
        let mut reg = ClassRegistry::new();
        let a = reg.add_class("A");
        reg.push_node(a, Node::new("x", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.set_address_expr(a, "<a.b>").unwrap();
        let b = reg.add_class("B");
        reg.push_node(b, Node::new("y", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.set_address_expr(b, "<a-b>").unwrap();

        let main = &generate_project(&reg, "collide", Some("proc"))[2].1;
        assert!(main.contains("proc.module(\"a.b\")?.base as u64;"));
        assert!(main.contains("proc.module(\"a-b\")?.base as u64;"));
        // second module gets a suffixed variable, and the first is bound exactly once
        assert!(main.contains("mod_a_b_base_2"));
        assert_eq!(main.matches("let mod_a_b_base:").count(), 1);
    }
}
