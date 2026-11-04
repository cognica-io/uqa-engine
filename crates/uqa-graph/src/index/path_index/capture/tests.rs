//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{GraphStore, GraphStoreHandle, MemoryGraphStore};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{Edge, Vertex};

#[test]
fn path_capture_keeps_exact_sequences_and_evaluated_pairs_after_source_loss() {
    let mut graph = MemoryGraphStore::new();
    graph.create_graph("g").unwrap();
    for id in 1..=3 {
        graph.add_vertex(Vertex::new(id, "node"), "g").unwrap();
    }
    for edge in [
        Edge::new(1, 1, 2, "knows"),
        Edge::new(2, 2, 3, "knows"),
        Edge::new(3, 3, 1, "a/b"),
    ] {
        graph.add_edge(edge, "g").unwrap();
    }
    let sequences = vec![
        vec!["knows".into()],
        vec!["knows".into(), "knows".into()],
        vec![],
        vec!["missing".into()],
        vec!["a/b".into()],
        vec!["a".into(), "b".into()],
    ];
    let source = PathIndex::build(&graph, "g", &sequences).unwrap();
    let PathStorage::Memory(paths) = &source.storage else {
        unreachable!()
    };
    let original = Arc::downgrade(paths);
    let expected: BTreeMap<_, _> = sequences
        .iter()
        .map(|sequence| (sequence.clone(), source.lookup(sequence).unwrap().unwrap()))
        .collect();
    let control = StorageReadControl::with_limit(16384);
    let captured = source.capture_memory_with_control(&control).unwrap();
    assert_eq!(Arc::strong_count(paths), 1);
    assert_eq!(captured.paths.len(), expected.len());
    drop((source, graph));
    assert!(original.upgrade().is_none());
    for (entry, (sequence, pairs)) in captured.paths.iter().zip(&expected) {
        assert_eq!(&entry.sequence, sequence);
        assert!(entry.pairs.iter().eq(pairs.iter()));
    }
    assert_eq!(expected[&vec!["knows".into()]], [(1, 2), (2, 3)].into());
    assert_eq!(
        expected[&vec!["knows".into(), "knows".into()]],
        [(1, 3)].into()
    );
    assert_ne!(
        expected[&vec!["a/b".into()]],
        expected[&vec!["a".into(), "b".into()]]
    );
    assert!(control.memory().used() > 0);
    drop(captured);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn path_capture_admits_large_labels_and_pairs_before_copy_and_retires_failures() {
    for (sequence, pairs) in [
        (vec!["long".repeat(8192)], [(u64::MAX, 0)].into()),
        (
            vec!["edge".into()],
            (0..20000).map(|id| (id, id + 1)).collect(),
        ),
    ] {
        let source = PathIndex {
            storage: PathStorage::Memory(Arc::new(
                super::super::MemoryPaths::from_entries(BTreeMap::from([(sequence, pairs)]))
                    .unwrap(),
            )),
        };
        let small = StorageReadControl::with_limit(4096);
        assert!(matches!(
            source.capture_memory_with_control(&small),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
        let control = StorageReadControl::with_limit(1 << 20);
        let saved = source.capture_memory_with_control(&control).unwrap();
        assert!(control.memory().used() >= 32768);
        let independent = StorageReadControl::with_limit(1 << 20);
        let other = source.capture_memory_with_control(&independent).unwrap();
        assert_eq!(control.memory().used(), independent.memory().used());
        assert_eq!(*saved, *other);
        control.cancellation().cancel();
        assert!(matches!(
            source.capture_memory_with_control(&control),
            Err(StorageBackendError::Cancelled(_))
        ));
        drop((saved, source));
        assert_eq!(control.memory().used(), 0);
        assert!(independent.memory().used() >= 32768);
        drop(other);
        assert_eq!(independent.memory().used(), 0);
    }
}

#[test]
fn path_capture_rejects_query_views_without_evaluating_the_graph() {
    let source = PathIndex {
        storage: PathStorage::ReadView {
            store: Arc::new(GraphStoreHandle::default()),
            graph: "not-created".into(),
            sequences: vec![vec!["edge".into()]],
        },
    };
    let control = StorageReadControl::with_limit(4096);
    let error = source.capture_memory_with_control(&control).unwrap_err();
    assert_eq!(
        error.to_string(),
        "memory path-index capture requires materialized memory storage"
    );
    assert_eq!(control.memory().used(), 0);
}
