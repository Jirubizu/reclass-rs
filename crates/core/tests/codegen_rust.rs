//! Phase-9 acceptance: the generated Rust compiles, and its `size_of` /
//! `offset_of` match the model. We emit a struct, append a `main` that asserts
//! every field offset and the total size, compile it with `rustc`, and run it.

use std::process::Command;

use reclass_core::class::ClassRegistry;
use reclass_core::codegen::{generate, Language};
use reclass_core::node::{IntWidth, Node, NodeKind, TextEncoding};

fn build_registry() -> (ClassRegistry, u32) {
    let mut reg = ClassRegistry::new();
    let inner = reg.add_class("Inner");
    reg.push_node(inner, Node::new("a", NodeKind::Int(IntWidth::W32)))
        .unwrap(); // 0
    reg.push_node(inner, Node::new("b", NodeKind::UInt(IntWidth::W64)))
        .unwrap(); // 4
    reg.push_node(inner, Node::new("c", NodeKind::Bool))
        .unwrap(); // 12  -> size 13

    let player = reg.add_class("Player");
    reg.push_node(player, Node::new("hp", NodeKind::Int(IntWidth::W32)))
        .unwrap(); // 0
    reg.push_node(player, Node::new("pos", NodeKind::Vec3))
        .unwrap(); // 4 (12)
    reg.push_node(
        player,
        Node::new("inner", NodeKind::ClassInstance { class_id: inner }),
    )
    .unwrap(); // 16 (13)
    reg.push_node(
        player,
        Node::new(
            "scores",
            NodeKind::Array {
                element: Box::new(NodeKind::UInt(IntWidth::W16)),
                count: 5,
            },
        ),
    )
    .unwrap(); // 29 (10)
    reg.push_node(
        player,
        Node::new(
            "name",
            NodeKind::Text {
                encoding: TextEncoding::Utf8,
                len: 12,
            },
        ),
    )
    .unwrap(); // 39 (12)
    reg.push_node(player, Node::new("flags", NodeKind::Hex(IntWidth::W32)))
        .unwrap(); // 51 (4)
    reg.push_node(
        player,
        Node::new("next", NodeKind::ClassPtr { class_id: player }),
    )
    .unwrap(); // 55 (8) -> size 63
    (reg, player)
}

#[test]
fn generated_rust_compiles_and_offsets_match() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    // ensure rustc is available; otherwise fail loudly (it is a build dependency)
    assert!(
        Command::new(&rustc).arg("--version").output().is_ok(),
        "rustc not available to compile generated code"
    );

    let (reg, player) = build_registry();
    let mut code = generate(&reg, Language::Rust);

    // Append an assertion harness driven by the model's own offsets/size.
    let offsets = reg.offsets(player);
    let names = ["hp", "pos", "inner", "scores", "name", "flags", "next"];
    code.push_str("\nfn main() {\n    use core::mem::{size_of, offset_of};\n");
    for (name, off) in names.iter().zip(offsets.iter()) {
        code.push_str(&format!(
            "    assert_eq!(offset_of!(Player, {name}), {off}, \"offset of {name}\");\n"
        ));
    }
    code.push_str(&format!(
        "    assert_eq!(size_of::<Player>(), {}, \"size of Player\");\n",
        reg.size_of(player)
    ));
    code.push_str("    assert_eq!(size_of::<Inner>(), 13, \"size of Inner\");\n");
    code.push_str("    println!(\"OK\");\n}\n");

    let dir = std::env::temp_dir().join(format!("reclass_codegen_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("gen.rs");
    let bin = dir.join("gen_bin");
    std::fs::write(&src, &code).unwrap();

    let out = Command::new(&rustc)
        .args([
            "--edition",
            "2021",
            "-A",
            "warnings",
            src.to_str().unwrap(),
            "-o",
            bin.to_str().unwrap(),
        ])
        .output()
        .expect("invoke rustc");
    assert!(
        out.status.success(),
        "generated Rust failed to compile:\n{}\n--- code ---\n{}",
        String::from_utf8_lossy(&out.stderr),
        code
    );

    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        run.status.success(),
        "offset/size assertions failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");

    let _ = std::fs::remove_dir_all(&dir);
}
