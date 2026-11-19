//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Production graph publication preserves captured data under bounded work and failed admission.

use super::*;
use crate::GraphStoreHandle;
use std::{error::Error, sync::Arc};
use uqa_core::{memory::MemoryError, Value};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

fn image(store: &MemoryGraphStore) -> Vec<u8> {
    let mut bytes = Vec::new();
    store
        .write_image(&mut bytes, &StorageReadControl::with_limit(1 << 24))
        .unwrap();
    bytes
}

fn fixture(memory: &MemoryBudget) -> MemoryGraphStore {
    let mut store = MemoryGraphStore::with_memory(memory);
    store.create_graph("g").unwrap();
    store.add_vertex(Vertex::new(1, "v"), "g").unwrap();
    store.add_vertex(Vertex::new(2, "v"), "g").unwrap();
    store.add_edge(Edge::new(10, 1, 2, "e"), "g").unwrap();
    store.copy_graph("g", "h").unwrap();
    store
        .create_label("g", "unused", LabelKind::Vertex)
        .unwrap();
    store.insert_raw_vertex(Vertex::new(99, "raw")).unwrap();
    store.insert_raw_edge(Edge::new(100, 1, 2, "raw")).unwrap();
    store
}

#[test]
fn native_graph_roots_capture_and_clone_without_visiting_primary_entities() {
    for count in [1, 64, 4096] {
        let source_memory = MemoryBudget::new(128 << 20);
        let mut source = MemoryGraphStore::with_memory(&source_memory);
        source.create_graph("g").unwrap();
        for id in 1..=count {
            let mut vertex = Vertex::new(id, "v");
            vertex
                .properties
                .insert("payload".into(), Value::Str("original".repeat(32)));
            source.add_vertex(vertex, "g").unwrap();
            source
                .allocate_vertex_id(&format!("label_{id:04}"), "g")
                .unwrap();
            if id > 1 {
                source
                    .add_edge(Edge::new(id, id - 1, id, "e"), "g")
                    .unwrap();
            }
        }
        let expected = image(&source);
        let mut saved = None;
        let allocation = allocation_counter::measure(|| saved = Some(source.clone()));
        assert_eq!(allocation.count_total, 0, "clone count {count}");
        let saved = saved.unwrap();
        assert!(std::ptr::eq(
            source.get_vertex(1).unwrap(),
            saved.get_vertex(1).unwrap()
        ));
        let control = StorageReadControl::with_limit(128 << 20);
        let handle = Arc::new(GraphStoreHandle::Memory(saved));
        let mut captured = None;
        let allocation = allocation_counter::measure(|| {
            captured = Some(handle.capture_image_with_control(&control).unwrap());
        });
        assert_eq!(allocation.count_total, 1, "capture count {count}");
        assert!(
            allocation.bytes_total < 512,
            "capture allocation {allocation:?}"
        );
        let required = control.memory().used();
        let exact = StorageReadControl::with_limit(required);
        let independent = handle.capture_image_with_control(&exact).unwrap();
        let short = StorageReadControl::with_limit(required - 1);
        assert!(matches!(
            handle.capture_image_with_control(&short),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(short.memory().used(), 0);
        let allocation = allocation_counter::measure(|| {
            source.add_vertex(Vertex::new(1, "changed"), "g").unwrap();
            source.allocate_vertex_id("label_0001", "g").unwrap();
        });
        assert!(
            allocation.bytes_total < 128 << 10,
            "selected mutation at {count}: {allocation:?}"
        );
        assert_ne!(image(&source), expected);
        drop(source);
        drop(handle);
        for retained in [captured.as_ref().unwrap(), &independent] {
            let mut bytes = Vec::new();
            retained
                .write_image(&mut bytes, &StorageReadControl::with_limit(1))
                .unwrap();
            assert_eq!(bytes, expected);
        }
        drop(captured);
        assert_eq!(control.memory().used(), 0);
        assert!(source_memory.used() > 0);
        drop(independent);
        assert_eq!(exact.memory().used(), 0);
        assert_eq!(source_memory.used(), 0);
    }
}

#[test]
fn native_graph_publication_denial_preserves_all_roots_and_identity_counters() {
    type Operation = fn(&mut MemoryGraphStore) -> GraphStoreResult<()>;
    let operations: &[(&str, Operation)] = &[
        ("create", |s| s.create_graph("new")),
        ("drop", |s| s.drop_graph("g")),
        ("rename", |s| s.rename_graph("g", "renamed")),
        ("insert vertex", |s| {
            s.add_vertex(Vertex::new(3, "fresh"), "g")
        }),
        ("replace vertex", |s| {
            s.add_vertex(Vertex::new(1, "changed"), "g")
        }),
        ("insert edge", |s| {
            s.add_edge(Edge::new(11, 2, 1, "fresh"), "g")
        }),
        ("replace edge", |s| {
            s.add_edge(Edge::new(10, 2, 1, "changed"), "g")
        }),
        ("remove vertex", |s| s.remove_vertex(1, "g")),
        ("remove edge", |s| s.remove_edge(10, "g")),
        ("attach vertex", |s| s.attach_vertex(99, "g")),
        ("attach edge", |s| s.attach_edge(100, "g")),
        ("raw vertex", |s| {
            s.insert_raw_vertex(Vertex::new(101, "new"))
        }),
        ("raw edge", |s| {
            s.insert_raw_edge(Edge::new(101, 1, 2, "new"))
        }),
        ("vertex identity", |s| {
            s.allocate_vertex_id("new", "g").map(|_| ())
        }),
        ("edge identity", |s| {
            s.allocate_edge_id("new", "g").map(|_| ())
        }),
        ("create label", |s| {
            s.create_label("g", "new", LabelKind::Vertex).map(|_| ())
        }),
        ("drop label", |s| s.drop_label("g", "unused").map(|_| ())),
        ("copy", |s| s.copy_graph("g", "copied")),
        ("union", |s| s.union_graphs("g", "h", "joined")),
        ("import labels", |s| {
            let mut record = GraphLabelRegistry::default();
            record.register_label("imported", LabelKind::Edge)?;
            s.import_label_registry("g", &record)
        }),
    ];
    for (name, operation) in operations {
        let memory = MemoryBudget::new(8 << 20);
        let original = fixture(&memory);
        let expected = image(&original);
        let mut failures = 0;
        for allowance in [0, 64, 256, 1024, 4096, 16384, 65536] {
            let mut candidate = original.clone();
            let held = memory.reserve(memory.available() - allowance).unwrap();
            let result = operation(&mut candidate);
            drop(held);
            if let Err(error) = result {
                let GraphStoreError::Backend(source) = error else {
                    panic!("{name}: {error}");
                };
                assert!(matches!(
                    source
                        .source()
                        .unwrap()
                        .downcast_ref::<StorageBackendError>(),
                    Some(StorageBackendError::Memory(MemoryError::Limit { .. }))
                ));
                assert_eq!(image(&candidate), expected, "{name} with {allowance} bytes");
                failures += 1;
                operation(&mut candidate).unwrap();
            }
        }
        assert!(failures > 0, "{name} did not exercise rejection");
        assert_eq!(
            image(&original),
            expected,
            "{name} changed the retained original"
        );
        drop(original);
        assert_eq!(memory.used(), 0, "{name} leaked admission");
    }
}

#[test]
fn native_graph_nested_transactions_restore_complete_roots_after_errors_and_panics() {
    let memory = MemoryBudget::new(8 << 20);
    let mut store = fixture(&memory);
    let original = image(&store);
    let failure = store.transaction(|outer| {
        outer.add_vertex(Vertex::new(3, "outer"), "g")?;
        let intermediate = image(outer);
        assert!(outer
            .transaction::<()>(|inner| {
                inner.rename_graph("g", "private")?;
                inner.allocate_vertex_id("private_label", "private")?;
                Err(GraphStoreError::InvalidMutation("rollback inner".into()))
            })
            .is_err());
        assert_eq!(image(outer), intermediate);
        Err::<(), _>(GraphStoreError::InvalidMutation("rollback outer".into()))
    });
    assert!(failure.is_err());
    assert_eq!(image(&store), original);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = store.transaction::<()>(|private| {
            private.drop_graph("g")?;
            panic!("rollback graph panic");
        });
    }));
    assert!(panic.is_err());
    assert_eq!(image(&store), original);
    store
        .transaction(|private| private.rename_graph("g", "committed"))
        .unwrap();
    assert!(store.has_graph("committed"));
    drop(store);
    assert_eq!(memory.used(), 0);
}
