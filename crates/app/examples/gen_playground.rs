// One-off: emit examples/playground/playground.ron — the Player/Weapon layout
// matching playground.c. Run: cargo run -p reclass --example gen_playground -- <out.ron> <addr>
use reclass::app_state::AppState;
use reclass_core::{IntWidth, Node, NodeKind, TextEncoding};

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "playground.ron".into());
    let addr = args.next().unwrap_or_else(|| "0x404080".into());

    let mut s = AppState::new();
    let player = s.add_class("Player"); // view 0 (shown on load)
    let weapon = s.add_class("Weapon"); // view 1

    let txt = |len| NodeKind::Text {
        encoding: TextEncoding::Utf8,
        len,
    };
    let push = |s: &mut AppState, c, name: &str, k: NodeKind| {
        s.push_node(c, Node::new(name, k)).unwrap();
    };

    // Player (size 0x48) — mirrors `struct Player` in playground.c
    push(&mut s, player, "health", NodeKind::Int(IntWidth::W32)); // +0x00
    push(&mut s, player, "max_health", NodeKind::Int(IntWidth::W32)); // +0x04
    push(&mut s, player, "position", NodeKind::Vec3); // +0x08
    push(&mut s, player, "flags", NodeKind::Hex(IntWidth::W32)); // +0x14
    push(&mut s, player, "alive", NodeKind::Bool); // +0x18
    push(&mut s, player, "_pad0", NodeKind::Padding(3));
    push(&mut s, player, "name", txt(24)); // +0x1C
    push(&mut s, player, "_pad1", NodeKind::Padding(4));
    push(
        &mut s,
        player,
        "weapon",
        NodeKind::ClassPtr { class_id: weapon },
    ); // +0x38
    push(&mut s, player, "score", NodeKind::UInt(IntWidth::W64)); // +0x40

    // Weapon (size 0x18)
    push(&mut s, weapon, "damage", NodeKind::Int(IntWidth::W32));
    push(&mut s, weapon, "ammo", NodeKind::Int(IntWidth::W32));
    push(&mut s, weapon, "name", txt(16));

    s.set_address_expr(player, addr).unwrap();
    s.save(&out).unwrap();
    eprintln!("wrote {out}");
}
