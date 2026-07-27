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

use std::collections::HashSet;

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

/// Escape a (already char-sanitized) identifier that collides with a C or C++
/// keyword. C has no raw-identifier syntax, so the only escape is a trailing
/// `_`. The C and C++ sets are merged: escaping a C++-only word in C output is
/// harmless, and it keeps one struct definition valid under both languages.
pub(super) fn c_ident(name: &str) -> String {
    #[rustfmt::skip]
    const KEYWORDS: [&str; 109] = [
        // C (including C99/C11/C23 additions)
        "alignas", "alignof", "auto", "bool", "break", "case", "char",
        "const", "constexpr", "continue", "default", "do", "double", "else",
        "enum", "extern", "false", "float", "for", "goto", "if", "inline",
        "int", "long", "nullptr", "register", "restrict", "return", "short",
        "signed", "sizeof", "static", "static_assert", "struct", "switch",
        "thread_local", "true", "typedef", "typeof", "typeof_unqual", "union",
        "unsigned", "void", "volatile", "while",
        "_Alignas", "_Alignof", "_Atomic", "_BitInt", "_Bool", "_Complex",
        "_Decimal128", "_Decimal32", "_Decimal64", "_Generic", "_Imaginary",
        "_Noreturn", "_Static_assert", "_Thread_local",
        // C++
        "and", "and_eq", "asm", "bitand", "bitor", "catch", "char16_t",
        "char32_t", "char8_t", "class", "compl", "concept", "const_cast",
        "consteval", "constinit", "co_await", "co_return", "co_yield",
        "decltype", "delete", "dynamic_cast", "explicit", "export", "friend",
        "mutable", "namespace", "new", "noexcept", "not", "not_eq",
        "operator", "or", "or_eq", "private", "protected", "public",
        "reinterpret_cast", "requires", "static_cast", "template", "this",
        "throw", "try", "typeid", "typename", "using", "virtual", "wchar_t",
        "xor", "xor_eq",
    ];
    if KEYWORDS.contains(&name) {
        format!("{name}_")
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

/// The `name = value` table for an [`NodeKind::Enum`] node, for emission as a
/// comment beside the field.
///
/// Codegen deliberately does not emit a real `enum`: the value comes from a
/// foreign process and may be any bit pattern, and materializing an out-of-range
/// discriminant is undefined behaviour in both Rust and C++. The field stays an
/// integer; this note preserves the names.
pub(super) fn enum_note(kind: &NodeKind) -> Option<String> {
    let NodeKind::Enum { variants, .. } = kind else {
        return None;
    };
    if variants.is_empty() {
        return None;
    }
    Some(
        variants
            .iter()
            .map(|v| format!("{} = {}", v.name, v.value))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// Claim `base`, or the first free `base_2`, `base_3`, … if it is taken.
///
/// [`sanitize`] is lossy — `"a b"` and `"a-b"` both become `a_b` — so two
/// distinct user names can land on one identifier and emit a struct with
/// duplicate members, which does not compile in either language.
fn unique(base: &str, used: &mut HashSet<String>) -> String {
    let mut candidate = base.to_string();
    let mut n = 1u32;
    while !used.insert(candidate.clone()) {
        n += 1;
        candidate = format!("{base}_{n}");
    }
    candidate
}

/// The deduplicated type name for `id` under `escape`.
///
/// Runs the same assignment every call: classes claim names in ascending id
/// order, so a name depends only on the classes *before* it and every call
/// site — definition, forward declaration, field type, pointee — agrees
/// without threading a map through the emitters. Escaping happens before the
/// uniqueness check, so an escaped keyword (`int` -> `int_`) cannot collide
/// with a class literally named `int_`.
fn type_name_with(reg: &ClassRegistry, id: ClassId, escape: fn(&str) -> String) -> String {
    let mut used = HashSet::new();
    for class in reg.iter() {
        let base = escape(&class_type_name(reg, class.id));
        let name = unique(&base, &mut used);
        if class.id == id {
            return name;
        }
    }
    // Dangling id: not a class in the registry, so nothing claimed a name.
    escape(&class_type_name(reg, id))
}

/// A class's Rust type name, keyword-escaped and deduplicated.
pub(super) fn rust_type_name(reg: &ClassRegistry, id: ClassId) -> String {
    type_name_with(reg, id, rust_ident)
}

/// A class's C/C++ type name, keyword-escaped and deduplicated. `struct` has
/// its own namespace in C but keywords are still reserved there, so
/// `struct int` is as invalid as a field named `int`.
pub(super) fn c_type_name(reg: &ClassRegistry, id: ClassId) -> String {
    type_name_with(reg, id, c_ident)
}

/// A class's generated `…View` accessor type name.
///
/// Deduplicated but *not* keyword-escaped: the `View` suffix already makes the
/// identifier a non-keyword, and escaping first would produce `r#moveView`,
/// which is not valid Rust.
pub(super) fn rust_view_name(reg: &ClassRegistry, id: ClassId) -> String {
    format!("{}View", type_name_with(reg, id, str::to_string))
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
    fn c_keyword_field_and_class_names_are_escaped() {
        // C has no raw identifiers, so keywords get a trailing `_`. Without
        // this the emitted file was `int32_t int;` — valid layout, invalid C.
        let mut reg = ClassRegistry::new();
        let cls = reg.add_class("class"); // C++ keyword as a type name
        reg.push_node(cls, Node::new("int", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.push_node(cls, Node::new("delete", NodeKind::Float32))
            .unwrap();
        reg.push_node(cls, Node::new("next", NodeKind::ClassPtr { class_id: cls }))
            .unwrap();

        let code = generate(&reg, Language::C);
        assert!(code.contains("int32_t int_;"), "{code}");
        assert!(code.contains("float delete_;"), "{code}");
        // forward decl, definition and the self-pointer must agree
        assert!(code.contains("struct class_;"), "{code}");
        assert!(code.contains("struct class_ {"), "{code}");
        assert!(code.contains("struct class_* next;"), "{code}");
    }

    #[test]
    fn colliding_sanitized_names_are_deduplicated() {
        // `sanitize` maps every illegal char to `_`, so distinct user names
        // collapse onto one identifier and emitted a struct with two members
        // of the same name — invalid in both languages.
        let mut reg = ClassRegistry::new();
        let a = reg.add_class("Player Data");
        let _b = reg.add_class("Player-Data");
        reg.push_node(a, Node::new("hit points", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        reg.push_node(a, Node::new("hit/points", NodeKind::Int(IntWidth::W32)))
            .unwrap();

        let rs = generate(&reg, Language::Rust);
        assert!(rs.contains("pub struct Player_Data {"), "{rs}");
        assert!(rs.contains("pub struct Player_Data_2 {"), "{rs}");
        assert!(rs.contains("pub hit_points: i32,"), "{rs}");
        assert!(rs.contains("pub hit_points_2: i32,"), "{rs}");

        let c = generate(&reg, Language::C);
        assert!(c.contains("struct Player_Data {"), "{c}");
        assert!(c.contains("struct Player_Data_2 {"), "{c}");
        assert!(c.contains("int32_t hit_points;"), "{c}");
        assert!(c.contains("int32_t hit_points_2;"), "{c}");
    }

    #[test]
    fn escaped_keyword_cannot_collide_with_a_literal_underscore_name() {
        // `int` escapes to `int_`; a class actually named `int_` must not end
        // up with the same identifier. Dedup runs *after* escaping.
        let mut reg = ClassRegistry::new();
        let _int = reg.add_class("int");
        let _int_ = reg.add_class("int_");
        let c = generate(&reg, Language::C);
        assert!(c.contains("struct int_ {"), "{c}");
        assert!(c.contains("struct int__2 {"), "{c}");
    }

    #[test]
    fn nested_c_array_dimensions_are_outside_in() {
        // 5 rows of 3 u32 must emit `[5][3]`, matching Rust's `[[u32; 3]; 5]`.
        // The two backends used to disagree: C emitted `[3][5]` — same total
        // size, transposed indexing.
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("Grid");
        reg.push_node(
            c,
            Node::new(
                "grid",
                NodeKind::Array {
                    element: Box::new(NodeKind::Array {
                        element: Box::new(NodeKind::UInt(IntWidth::W32)),
                        count: 3,
                    }),
                    count: 5,
                },
            ),
        )
        .unwrap();
        assert!(generate(&reg, Language::C).contains("uint32_t grid[5][3];"));
        assert!(generate(&reg, Language::Rust).contains("pub grid: [[u32; 3]; 5],"));
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
    fn project_view_names_are_valid_and_unique() {
        // The `View` accessor name was built from `class_type_name` in the
        // definition but from the keyword-escaped `rust_type_name` at one call
        // site, so a class named `move` declared `moveView` and referenced
        // `r#moveView`. And because view names skipped dedup entirely, two
        // classes that sanitize alike defined the same view struct twice.
        let mut reg = ClassRegistry::new();
        let kw = reg.add_class("move");
        reg.push_node(kw, Node::new("a", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        let a = reg.add_class("Player Data");
        reg.push_node(a, Node::new("b", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        let b = reg.add_class("Player-Data");
        reg.push_node(b, Node::new("c", NodeKind::ClassPtr { class_id: a }))
            .unwrap();

        let code = &generate_project(&reg, "views", None)[1].1;
        assert!(!code.contains("r#moveView"), "{code}");
        assert!(code.contains("pub struct moveView<'a>"), "{code}");
        assert!(code.contains("pub struct Player_DataView<'a>"), "{code}");
        assert!(code.contains("pub struct Player_Data_2View<'a>"), "{code}");
        // the ClassPtr accessor in the second class must name the first's view
        assert!(
            code.contains("pub fn c(&self) -> Result<Player_DataView<'a>, Error> {"),
            "{code}"
        );
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

    /// A class exercising `Enum`, `Bitfield`, and `PtrText` in both encodings.
    fn exotic() -> ClassRegistry {
        use crate::node::EnumVariant;
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("Ent");
        reg.push_node(
            c,
            Node::new(
                "state",
                NodeKind::Enum {
                    width: IntWidth::W32,
                    variants: vec![
                        EnumVariant {
                            value: 0,
                            name: "Idle".into(),
                        },
                        EnumVariant {
                            value: 1,
                            name: "Run".into(),
                        },
                    ],
                },
            ),
        )
        .unwrap();
        reg.push_node(c, Node::new("flags", NodeKind::Bitfield(IntWidth::W16)))
            .unwrap();
        reg.push_node(
            c,
            Node::new(
                "name",
                NodeKind::PtrText {
                    encoding: TextEncoding::Utf8,
                    max: 64,
                },
            ),
        )
        .unwrap();
        reg.push_node(
            c,
            Node::new(
                "wname",
                NodeKind::PtrText {
                    encoding: TextEncoding::Utf16,
                    max: 64,
                },
            ),
        )
        .unwrap();
        reg
    }

    #[test]
    fn enum_emits_an_integer_field_plus_a_variant_comment() {
        let reg = exotic();
        for (lang, field) in [
            (Language::Rust, "pub state: u32, // 0x0"),
            (Language::C, "uint32_t state; // 0x0"),
        ] {
            let code = generate(&reg, lang);
            // never a real `enum`: a foreign process can hold any bit pattern
            assert!(!code.contains("enum Ent"), "{lang:?}\n{code}");
            assert!(code.contains(field), "{lang:?}\n{code}");
            assert!(
                code.contains("// enum: Idle = 0, Run = 1"),
                "{lang:?}\n{code}"
            );
        }
    }

    #[test]
    fn enum_with_no_variants_emits_no_empty_comment() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("E");
        reg.push_node(
            c,
            Node::new(
                "v",
                NodeKind::Enum {
                    width: IntWidth::W8,
                    variants: Vec::new(),
                },
            ),
        )
        .unwrap();
        let code = generate(&reg, Language::Rust);
        assert!(code.contains("pub v: u8"));
        assert!(!code.contains("// enum:"), "{code}");
    }

    #[test]
    fn bitfield_and_ptr_text_types_match_their_widths() {
        let reg = exotic();
        let rust = generate(&reg, Language::Rust);
        assert!(rust.contains("pub flags: u16, // 0x4"), "{rust}");
        assert!(rust.contains("pub name: *mut u8, // 0x6"), "{rust}");
        assert!(rust.contains("pub wname: *mut u16, // 0xE"), "{rust}");

        let c = generate(&reg, Language::C);
        assert!(c.contains("uint16_t flags; // 0x4"), "{c}");
        assert!(c.contains("char* name; // 0x6"), "{c}");
        assert!(c.contains("uint16_t* wname; // 0xE"), "{c}");
    }

    #[test]
    fn exotic_kinds_get_project_accessors() {
        let reg = exotic();
        let files = generate_project(&reg, "demo", None);
        let src = files
            .iter()
            .find(|f| f.0.ends_with("generated.rs"))
            .map(|f| f.1.as_str())
            .expect("generated project has a bindings module");
        // enum/bitfield read as their storage integer, PtrText as an address
        assert!(src.contains("-> Result<u32, Error>"), "{src}");
        assert!(src.contains("-> Result<u16, Error>"), "{src}");
        assert!(src.contains("-> Result<usize, Error>"), "{src}");
        assert!(
            !src.contains("no accessor: `state`"),
            "enum lost its accessor:\n{src}"
        );
        assert!(
            !src.contains("no accessor: `name`"),
            "PtrText lost its accessor:\n{src}"
        );
    }

    #[test]
    fn a_32_bit_target_emits_integer_pointers_not_host_width_ones() {
        use crate::class::PtrWidth;
        let mut reg = ClassRegistry::new();
        reg.set_ptr_width(PtrWidth::P32);
        let t = reg.add_class("T");
        let c = reg.add_class("S");
        reg.push_node(c, Node::new("p", NodeKind::Pointer)).unwrap();
        reg.push_node(c, Node::new("cp", NodeKind::ClassPtr { class_id: t }))
            .unwrap();
        reg.push_node(c, Node::new("after", NodeKind::UInt(IntWidth::W32)))
            .unwrap();

        // A host-compiled `*mut T` / `void*` is 8 bytes here, which would put
        // `after` at 0x10 in the generated struct but 0x8 in the live target.
        let rust = generate(&reg, Language::Rust);
        assert!(rust.contains("pub p: u32, // 0x0"), "{rust}");
        assert!(rust.contains("pub cp: u32, // 0x4"), "{rust}");
        assert!(rust.contains("pub after: u32, // 0x8"), "{rust}");
        assert!(!rust.contains("*mut"), "{rust}");

        let c_src = generate(&reg, Language::C);
        assert!(c_src.contains("uint32_t p; // 0x0"), "{c_src}");
        assert!(c_src.contains("uint32_t cp; // 0x4"), "{c_src}");
        assert!(c_src.contains("uint32_t after; // 0x8"), "{c_src}");
        assert!(!c_src.contains("void*"), "{c_src}");
    }

    #[test]
    fn a_64_bit_target_still_emits_real_pointers() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("S");
        reg.push_node(c, Node::new("p", NodeKind::Pointer)).unwrap();
        assert!(generate(&reg, Language::Rust).contains("pub p: *mut u8"));
        assert!(generate(&reg, Language::C).contains("void* p"));
    }

    #[test]
    fn project_accessors_read_the_target_pointer_width() {
        use crate::class::PtrWidth;
        let mut reg = ClassRegistry::new();
        reg.set_ptr_width(PtrWidth::P32);
        let c = reg.add_class("S");
        reg.push_node(c, Node::new("p", NodeKind::Pointer)).unwrap();
        let files = generate_project(&reg, "demo", None);
        let src = &files
            .iter()
            .find(|f| f.0.ends_with("generated.rs"))
            .expect("bindings module")
            .1;
        // a 4-byte read, not 8 — an 8-byte read would swallow the next field
        assert!(src.contains("[0u8; 4]"), "{src}");
        assert!(src.contains("-> Result<usize, Error>"), "{src}");
    }
}
