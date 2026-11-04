//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use uqa_graph::LabelKind;

#[test]
fn committed_graph_oid_roots_preserve_original_labels_with_bounded_capture_work() {
    for count in [1, 16, 64] {
        let engine = Engine::new();
        for index in 0..count {
            engine.create_graph(format!("saved_{index:03}")).unwrap();
        }
        engine
            .create_graph_label("saved_000", "Original", LabelKind::Vertex)
            .unwrap();
        let original = engine.durable.graph_catalog_oids.snapshot();
        let expected = original
            .iter()
            .map(|(name, graph)| (name.clone(), graph.clone()))
            .collect::<Vec<_>>();
        let weak = Arc::downgrade(&original);
        drop(original);
        sql(&engine, "BEGIN");
        engine
            .create_graph_label("saved_000", "Private", LabelKind::Edge)
            .unwrap();
        sql(&engine, "SAVEPOINT changed");
        let control = StorageReadControl::with_limit(16 << 20);
        let selected = capture(&engine, &control);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some(
                selected
                    .catalogs
                    .graph_catalog_oids
                    .captured(&selected.cut)
                    .unwrap(),
            );
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 128,
            "count={count}: {allocations:?}"
        );
        let graphs = captured.unwrap();
        assert!(graphs
            .get()
            .unwrap()
            .iter()
            .eq(expected.iter().map(|(k, v)| (k, v))));
        engine.drop_graph_label("saved_000", "Original").unwrap();
        sql(&engine, "ROLLBACK TO changed; COMMIT");
        engine.drop_graph("saved_000").unwrap();
        engine.create_graph("saved_000").unwrap();
        engine.rename_graph("saved_000", "moved").unwrap();
        drop(engine);
        assert!(weak.upgrade().is_none());
        assert!(graphs
            .get()
            .unwrap()
            .iter()
            .eq(expected.iter().map(|(k, v)| (k, v))));
        drop(selected);
        control.cancellation().cancel();
        assert!(graphs.get().is_err());
        drop(graphs);
        assert_eq!(control.memory().used(), 0);
    }
}
