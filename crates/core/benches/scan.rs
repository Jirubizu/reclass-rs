//! Pointer-scan benchmarks.
//!
//! The reverse map dominates: it touches every pointer-aligned word in readable
//! memory once, while the backwards walk only probes a bounded neighbourhood
//! per node. These measure both against a pointer-dense synthetic target so a
//! regression in either shows up separately.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use reclass_core::backend::{MockBackend, Perms, Region};
use reclass_core::scan::{ScanConfig, scan_pointers};

const MODULE: u64 = 0x40_0000;
const HEAP: u64 = 0x10_0000_0000;

fn perms() -> Perms {
    Perms {
        read: true,
        write: true,
        execute: false,
        shared: false,
    }
}

/// A module region plus `heap_len` bytes of heap where every 8th word is a
/// pointer back into the heap — the dense case the reverse map has to survive.
///
/// The chain planted is module -> mid -> target, so the walk has something real
/// to find rather than exhausting the queue.
fn dense(heap_len: usize) -> (MockBackend, u64) {
    let m = MockBackend::new();
    let module_len = 0x1000usize;
    m.put_region(Region {
        start: MODULE,
        end: MODULE + module_len as u64,
        perms: perms(),
        path: Some("/usr/lib/game.so".into()),
    });
    m.put_region(Region {
        start: HEAP,
        end: HEAP + heap_len as u64,
        perms: perms(),
        path: None,
    });

    let mid = HEAP + 0x1000;
    let target = HEAP + (heap_len as u64 / 2);

    let mut heap = vec![0u8; heap_len];
    // Every 8th qword points somewhere else in the heap: enough density that
    // the value filter cannot trivially reject most words.
    for i in (0..heap_len / 8).step_by(8) {
        let value = HEAP + ((i as u64 * 64) % heap_len as u64);
        heap[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
    let off = (mid - HEAP) as usize;
    heap[off..off + 8].copy_from_slice(&target.to_le_bytes());
    m.put(HEAP, heap);

    let mut module = vec![0u8; module_len];
    module[0x40..0x48].copy_from_slice(&mid.to_le_bytes());
    m.put(MODULE, module);

    (m, target)
}

fn scan_dense_heap(c: &mut Criterion) {
    let mut group = c.benchmark_group("pointer_scan");
    for mb in [1usize, 4, 16] {
        let (backend, target) = dense(mb << 20);
        let cfg = ScanConfig::default();
        // sanity: the planted chain is actually findable, so the bench is not
        // timing an early-out
        assert!(
            !scan_pointers(&backend, target, &cfg).unwrap().is_empty(),
            "planted chain not found at {mb} MiB"
        );
        group.bench_function(format!("{mb}MiB"), |b| {
            b.iter(|| black_box(scan_pointers(&backend, target, &cfg).unwrap()));
        });
    }
    group.finish();
}

/// Depth costs queue breadth, not another full memory pass — this should stay
/// flat next to the map build above.
fn scan_by_depth(c: &mut Criterion) {
    let (backend, target) = dense(4 << 20);
    let mut group = c.benchmark_group("pointer_scan_depth");
    for depth in [1usize, 4, 8] {
        let cfg = ScanConfig {
            max_depth: depth,
            ..Default::default()
        };
        group.bench_function(format!("depth_{depth}"), |b| {
            b.iter(|| black_box(scan_pointers(&backend, target, &cfg).unwrap()));
        });
    }
    group.finish();
}

criterion_group!(benches, scan_dense_heap, scan_by_depth);
criterion_main!(benches);
