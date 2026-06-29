//! Phases 4–6 live, sans pixels: drive `AppState` with a real `VmemBackend`
//! against a spawned target — attach, resolve an address expression, render the
//! live table, edit a value, and follow a `ClassPtr` to a nested class.
//!
//! Self-skips if ptrace is denied or the `target` helper binary is not built.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use reclass::app_state::AppState;
use reclass_backend_vmem::VmemBackend;
use reclass_core::{IntWidth, Node, NodeKind, PathSeg};

fn target_bin() -> Option<PathBuf> {
    // .../target/debug/deps/<test>  ->  .../target/debug/target
    let exe = std::env::current_exe().ok()?;
    let deps = exe.parent()?; // deps
    let profile = deps.parent()?; // debug
    let p = profile.join("target");
    p.exists().then_some(p)
}

#[test]
fn live_app_attach_expr_edit_and_follow() {
    let Some(bin) = target_bin() else {
        eprintln!("SKIP: target helper not built (run `cargo build --workspace`)");
        return;
    };
    let mut child = Command::new(&bin)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn target");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).expect("read target line");
    let t: Vec<&str> = line.split_whitespace().collect();
    let hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    let arr_addr = hex(t[2]);
    let arr: [u32; 4] = [
        t[3].parse().unwrap(),
        t[4].parse().unwrap(),
        t[5].parse().unwrap(),
        t[6].parse().unwrap(),
    ];
    let parent_addr = hex(t[7]);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let backend = match VmemBackend::by_pid(child.id() as i32) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("SKIP: attach failed: {e}");
                return;
            }
        };
        let mut st = AppState::new();
        st.set_backend(Box::new(backend));

        // Inner: 4 x UInt32 matching the live array.
        let inner = st.add_class("Inner"); // view 0
        for i in 0..4 {
            st.push_node(
                inner,
                Node::new(format!("e{i}"), NodeKind::UInt(IntWidth::W32)),
            )
            .unwrap();
        }
        st.set_address_expr(inner, format!("0x{arr_addr:x}"))
            .unwrap();

        // Outer: a ClassPtr to Inner, living at the pointer cell.
        let outer = st.add_class("Outer"); // view 1
        st.push_node(
            outer,
            Node::new("toInner", NodeKind::ClassPtr { class_id: inner }),
        )
        .unwrap();
        st.set_address_expr(outer, format!("0x{parent_addr:x}"))
            .unwrap();
        // expand Outer.toInner (root index 1, node 0)
        st.toggle_expand(1, vec![PathSeg::Node(0)]);

        // --- Phase 4: live table over the resolved expression ---
        let rows = st.compute_rows();
        assert_eq!(
            st.view_status[0].base, arr_addr,
            "expr did not resolve to arr"
        );
        let inner_rows: Vec<_> = rows.iter().filter(|r| r.root == 0).collect();
        assert_eq!(inner_rows.len(), 4);
        for (i, r) in inner_rows.iter().enumerate() {
            assert_eq!(r.value, arr[i].to_string(), "live value e{i}");
            assert_eq!(r.address, arr_addr + (i as u64) * 4);
        }

        // --- Phase 6: follow the ClassPtr to the nested Inner, live ---
        let outer_rows: Vec<_> = rows.iter().filter(|r| r.root == 1).collect();
        // ptr row + 4 inner element rows
        assert!(outer_rows.iter().any(|r| r.expandable && r.expanded));
        let nested_e0 = outer_rows
            .iter()
            .find(|r| r.name == "e0")
            .expect("nested e0 row");
        assert_eq!(nested_e0.value, arr[0].to_string());
        assert_eq!(
            nested_e0.address, arr_addr,
            "nested e0 should alias the array"
        );

        // --- Phase 5: edit a value, write-back, and observe the change ---
        let e1_addr = arr_addr + 4;
        st.write_value(e1_addr, &NodeKind::UInt(IntWidth::W32), "123456789")
            .expect("write_value");
        let rows2 = st.compute_rows();
        let e1 = rows2
            .iter()
            .find(|r| r.root == 0 && r.name == "e1")
            .unwrap();
        assert_eq!(e1.value, "123456789", "edited value not observed live");
    }));

    let _ = child.kill();
    let _ = child.wait();
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
