//! Acceptance: a project written by the app round-trips through `.rcnet` on disk.

use reclass::app_state::AppState;
use reclass_core::{IntWidth, Node, NodeKind};

#[test]
fn a_project_round_trips_through_a_real_rcnet_file() {
    let dir = std::env::temp_dir().join("reclass_rcnet_smoke.rcnet");
    let mut st = AppState::new();
    let weapon = st.add_class("Weapon");
    st.push_node(weapon, Node::new("ammo", NodeKind::Int(IntWidth::W32)))
        .unwrap();
    let player = st.add_class("Player");
    st.push_node(player, Node::new("hp", NodeKind::Int(IntWidth::W32)))
        .unwrap();
    st.push_node(
        player,
        Node::new("w", NodeKind::ClassPtr { class_id: weapon }),
    )
    .unwrap();
    st.set_address_expr(player, "<game.so> + 0x10".to_string())
        .unwrap();
    let before = st.registry().size_of(player);

    let path = dir.to_str().unwrap();
    let out = st.export_rcnet(path).unwrap();
    assert!(out.is_exact(), "{:?}", out.notes);
    assert!(std::fs::metadata(path).unwrap().len() > 0);

    let mut other = AppState::new();
    let r = other.import_rcnet(path).unwrap();
    assert_eq!(r.classes, 2);
    let p2 = other
        .registry()
        .iter()
        .find(|c| c.name == "Player")
        .unwrap()
        .id;
    assert_eq!(other.registry().size_of(p2), before);
    assert_eq!(
        other.registry().get(p2).unwrap().address_expr,
        "<game.so> + 0x10"
    );
    // the import opened the biggest class so the table is not blank
    assert!(!other.project.views.is_empty());
    // and it is undoable
    assert!(other.undo());
    let _ = std::fs::remove_file(&dir);
}
