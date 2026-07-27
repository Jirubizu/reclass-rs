//! Undo-snapshot benchmarks.
//!
//! The app's undo history clones the whole [`Project`] before every structural
//! edit. That is only defensible if the clone is cheap next to the work the
//! edit already does, so this measures the clone against `size_of` /
//! `offsets` — the recomputation an edit's cache invalidation forces anyway.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use reclass_core::class::ClassRegistry;
use reclass_core::node::{IntWidth, Node, NodeKind};
use reclass_core::project::Project;

/// `classes` classes of `fields` fields each, every class pointing at the next
/// so the registry holds real cross-references rather than isolated leaves.
fn build(classes: u32, fields: usize) -> Project {
    let mut reg = ClassRegistry::new();
    let ids: Vec<_> = (0..classes)
        .map(|i| reg.add_class(format!("C{i}")))
        .collect();
    for (n, &id) in ids.iter().enumerate() {
        for f in 0..fields {
            reg.push_node(
                id,
                Node::new(format!("field_{f}"), NodeKind::Hex(IntWidth::W32)),
            )
            .expect("class was just created");
        }
        if let Some(&next) = ids.get(n + 1) {
            reg.push_node(id, Node::new("next", NodeKind::ClassPtr { class_id: next }))
                .expect("class was just created");
        }
    }
    Project {
        registry: reg,
        ..Default::default()
    }
}

fn snapshot_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("undo_snapshot");
    for (classes, fields) in [(8u32, 32usize), (64, 64), (256, 64)] {
        let project = build(classes, fields);
        group.bench_function(format!("clone_{classes}x{fields}"), |b| {
            b.iter(|| black_box(project.clone()));
        });
    }
    group.finish();
}

/// The baseline an edit pays regardless: every structural edit clears the
/// size/offset caches, so the next frame recomputes them.
fn offsets_after_invalidation(c: &mut Criterion) {
    let mut group = c.benchmark_group("undo_snapshot");
    for (classes, fields) in [(8u32, 32usize), (64, 64), (256, 64)] {
        let mut project = build(classes, fields);
        let ids = project.registry.ids();
        group.bench_function(format!("recompute_offsets_{classes}x{fields}"), |b| {
            b.iter(|| {
                project.registry.touch();
                for &id in &ids {
                    black_box(project.registry.offsets(id));
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, snapshot_clone, offsets_after_invalidation);
criterion_main!(benches);
