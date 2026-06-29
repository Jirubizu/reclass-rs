//! Engine benchmarks. Proves the render loop batches reads (one scatter per
//! pointer-chain level) and stays allocation-light across ticks.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use reclass_core::backend::MockBackend;
use reclass_core::class::ClassRegistry;
use reclass_core::engine::{Engine, ExpandState, PathSeg, Root};
use reclass_core::node::{IntWidth, Node, NodeKind};

/// 256-byte class with 64 Hex32 nodes.
fn flat_256(c: &mut Criterion) {
    let mut reg = ClassRegistry::new();
    let cid = reg.add_class("Big");
    for i in 0..64 {
        reg.push_node(
            cid,
            Node::new(format!("f{i}"), NodeKind::Hex(IntWidth::W32)),
        )
        .unwrap();
    }
    assert_eq!(reg.size_of(cid), 256);

    let backend = MockBackend::new();
    backend.put(0x10_0000, vec![0xABu8; 256]);

    let roots = [Root {
        class_id: cid,
        base: 0x10_0000,
    }];
    let expand = ExpandState::new();
    let mut eng = Engine::new();

    c.bench_function("flat_256_64nodes", |b| {
        b.iter(|| {
            let rows = eng.snapshot(&backend, &reg, &roots, &expand, None);
            black_box(rows.len());
        });
    });
    // one scatter per tick
    assert_eq!(eng.last_read_levels(), 1);
}

/// A four-deep `ClassPtr` chain (A -> B -> C -> D), all expanded.
fn nested_ptr_chain(c: &mut Criterion) {
    let mut reg = ClassRegistry::new();
    // leaf D
    let d = reg.add_class("D");
    for i in 0..8 {
        reg.push_node(d, Node::new(format!("d{i}"), NodeKind::Int(IntWidth::W32)))
            .unwrap();
    }
    let mk = |reg: &mut ClassRegistry, name: &str, next: u32| -> u32 {
        let id = reg.add_class(name);
        for i in 0..4 {
            reg.push_node(
                id,
                Node::new(format!("{name}{i}"), NodeKind::Hex(IntWidth::W32)),
            )
            .unwrap();
        }
        reg.push_node(id, Node::new("next", NodeKind::ClassPtr { class_id: next }))
            .unwrap();
        id
    };
    let cc = mk(&mut reg, "C", d);
    let bb = mk(&mut reg, "B", cc);
    let aa = mk(&mut reg, "A", bb);

    let backend = MockBackend::new();
    // A @ 0x1000, B @ 0x2000, C @ 0x3000, D @ 0x4000
    // each A/B/C: 4*4 = 16 bytes header + 8 byte ptr at offset 16
    let layout = |base: u64, ptr: u64| {
        let mut bytes = vec![0u8; 24];
        bytes[16..24].copy_from_slice(&ptr.to_le_bytes());
        backend.put(base, bytes);
    };
    layout(0x1000, 0x2000);
    layout(0x2000, 0x3000);
    layout(0x3000, 0x4000);
    backend.put(0x4000, vec![7u8; 32]); // D leaf

    let mut expand = ExpandState::new();
    // expand A.next, B.next, C.next (next is node index 4 in each)
    expand.expand(0, vec![PathSeg::Node(4)]);
    expand.expand(0, vec![PathSeg::Node(4), PathSeg::Node(4)]);
    expand.expand(
        0,
        vec![PathSeg::Node(4), PathSeg::Node(4), PathSeg::Node(4)],
    );

    let roots = [Root {
        class_id: aa,
        base: 0x1000,
    }];
    let mut eng = Engine::new();

    c.bench_function("nested_ptr_chain_depth4", |b| {
        b.iter(|| {
            let rows = eng.snapshot(&backend, &reg, &roots, &expand, None);
            black_box(rows.len());
        });
    });
    // 4 levels (A, B, C, D) => 4 scatter calls per tick
    assert_eq!(eng.last_read_levels(), 4);
}

criterion_group!(benches, flat_256, nested_ptr_chain);
criterion_main!(benches);
