//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_core::{Edge, Vertex};
use uqa_graph::{CapturedPath, CapturedPathIndex};
use uqa_storage::StorageBackendError;

fn graph(engine: &Engine) {
    for id in 1..=3 {
        engine
            .add_graph_vertex(Vertex::new(id, "node"), "g")
            .unwrap();
    }
    engine
        .add_graph_edge(Edge::new(1, 1, 2, "edge"), "g")
        .unwrap();
    engine
        .add_graph_edge(Edge::new(2, 2, 3, "edge"), "g")
        .unwrap();
}

fn expected() -> CapturedPathIndex {
    CapturedPathIndex {
        paths: vec![
            CapturedPath {
                sequence: vec!["edge".into()],
                pairs: vec![(1, 2), (2, 3)],
            },
            CapturedPath {
                sequence: vec!["edge".into(), "edge".into()],
                pairs: vec![(1, 3)],
            },
            CapturedPath {
                sequence: vec!["missing".into()],
                pairs: vec![],
            },
        ],
    }
}

#[test]
fn memory_path_capture_preserves_evaluated_pairs_through_rollback_replacement_and_closure() {
    for mutate in [false, true] {
        let engine = Engine::new();
        graph(&engine);
        let sequences: Vec<_> = expected()
            .paths
            .into_iter()
            .map(|path| path.sequence)
            .collect();
        assert!(engine.build_path_index("saved", "g", &sequences).unwrap());
        let key = engine.durable.path_indexes.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        assert!(key.retained(&cut).unwrap().is_none());
        if mutate {
            sql(&engine, "BEGIN");
            engine
                .add_graph_edge(Edge::new(3, 3, 1, "edge"), "g")
                .unwrap();
            assert!(engine.get_path_index("saved", "g").unwrap().is_none());
            sql(&engine, "SAVEPOINT removed");
            engine.build_path_index("saved", "g", &sequences).unwrap();
            sql(&engine, "ROLLBACK TO removed; ROLLBACK");
            let index = engine.get_path_index("saved", "g").unwrap().unwrap();
            assert_eq!(
                index.lookup(&sequences[1]).unwrap().unwrap(),
                [(1, 3)].into()
            );
            engine.drop_graph("g").unwrap();
            graph(&engine);
            engine
                .add_graph_edge(Edge::new(3, 3, 1, "edge"), "g")
                .unwrap();
            engine.build_path_index("saved", "g", &sequences).unwrap();
            assert_ne!(
                engine
                    .get_path_index("saved", "g")
                    .unwrap()
                    .unwrap()
                    .lookup(&sequences[1])
                    .unwrap()
                    .unwrap(),
                [(1, 3)].into()
            );
        }
        drop(engine);
        let saved = key.retained(&cut).unwrap().unwrap();
        assert_eq!(saved.get().unwrap().len(), 1);
        assert_eq!(**saved.get().unwrap().get("g::saved").unwrap(), expected());
        drop(cut);
        assert!(control.memory().used() > 0);
        control.cancellation().cancel();
        assert!(matches!(
            saved.get(),
            Err(StorageBackendError::Cancelled(_))
        ));
        drop(saved);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn denied_path_capture_preserves_ordinary_updates_and_independent_cuts() {
    let engine = Engine::new();
    graph(&engine);
    let label = "missing".repeat(8192);
    engine
        .build_path_index("saved", "g", &[vec![label.clone()]])
        .unwrap();
    let key = engine.durable.path_indexes.key().unwrap();
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let small = StorageReadControl::with_limit(8192);
    let good = StorageReadControl::with_limit(1 << 20);
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    assert!(engine.drop_path_index("saved", "g").unwrap());
    assert!(matches!(
        failed.check(),
        Err(StorageBackendError::Memory(_))
    ));
    engine
        .build_path_index("saved", "g", &[vec!["edge".into()]])
        .unwrap();
    assert_eq!(
        engine
            .get_path_index("saved", "g")
            .unwrap()
            .unwrap()
            .lookup(&["edge".into()])
            .unwrap()
            .unwrap()
            .len(),
        2
    );
    drop(engine);
    let saved = key.retained(&cut).unwrap().unwrap();
    let value = saved.get().unwrap().get("g::saved").unwrap();
    assert_eq!(value.paths.len(), 1);
    assert_eq!(value.paths[0].sequence, [label]);
    assert!(value.paths[0].pairs.is_empty());
    drop((saved, cut, failed));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
