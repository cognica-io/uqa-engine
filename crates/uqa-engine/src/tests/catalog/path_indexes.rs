//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::{memory::MemoryBudget, Edge, Vertex};
use uqa_graph::{GraphStore, PathIndexCatalog};

fn bounded(engine: &Engine) -> MemoryBudget {
    let memory = MemoryBudget::new(1 << 20);
    let mut indexes = PathIndexCatalog::new(&memory);
    for (name, index) in engine.durable.path_indexes.read().iter() {
        indexes.insert(name, index.clone()).unwrap();
    }
    *engine.durable.path_indexes.write() = indexes;
    memory
}

fn populate(engine: &Engine) {
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
    for number in 0..16 {
        engine
            .build_path_index(&format!("saved{number:02}"), "g", &[vec!["edge".into()]])
            .unwrap();
    }
}

fn original_index(engine: &Engine) {
    assert_eq!(engine.list_path_indexes().unwrap().len(), 16);
    let index = engine.get_path_index("saved00", "g").unwrap().unwrap();
    assert_eq!(
        index.lookup(&["edge".into()]).unwrap().unwrap(),
        [(1, 2), (2, 3)].into()
    );
    assert_eq!(
        engine
            .graph_with("g", |graph| graph.vertex_ids_in_graph("g"))
            .unwrap()
            .unwrap()
            .unwrap(),
        [1, 2, 3].into()
    );
}

#[derive(Debug, PartialEq, Eq)]
struct StoredPaths {
    definitions: Vec<(String, String)>,
    pairs: Vec<(u64, u64)>,
}

fn stored_paths(engine: &Engine) -> Option<StoredPaths> {
    engine.storage.catalog.as_ref().map(|catalog| StoredPaths {
        definitions: catalog.load_path_indexes().unwrap(),
        pairs: catalog
            .path_index_pairs("g::saved00", r#"["edge"]"#, None, 256)
            .unwrap(),
    })
}

fn replaced_index(engine: &Engine) {
    assert_eq!(
        engine
            .get_path_index("saved00", "g")
            .unwrap()
            .unwrap()
            .lookup(&["edge".into(), "edge".into()])
            .unwrap()
            .unwrap(),
        [(1, 3)].into()
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_path_registry_admission_preserves_physical_materialization_and_transaction_recovery(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native-path-indexes.db");
    let engine = provider.open(&path);
    populate(&engine);
    for operation in 0..5 {
        engine.sql("BEGIN", &[]).unwrap();
        let memory = bounded(&engine);
        let original = engine.durable.path_indexes.snapshot();
        let stored = stored_paths(&engine);
        let occupied = memory.reserve(memory.available()).unwrap();
        let result = match operation {
            0 => engine.build_path_index("added", "g", &[vec!["absent".into()]]),
            1 => engine.build_path_index("saved00", "g", &[vec!["absent".into()]]),
            2 => engine.drop_path_index("saved00", "g"),
            3 => engine
                .add_graph_vertex(Vertex::new(4, "node"), "g")
                .map(|()| true),
            _ => engine.drop_graph("g"),
        };
        let error = result.unwrap_err();
        assert_eq!(
            uqa_execution::storage_errors::storage_error("path index", &error).sqlstate(),
            Some("53200"),
            "{provider:?}, {operation}: {error}"
        );
        assert_eq!(stored_paths(&engine), stored, "{provider:?}, {operation}");
        drop(occupied);
        engine.sql("ROLLBACK", &[]).unwrap();
        assert!(engine
            .durable
            .path_indexes
            .read()
            .keys()
            .eq(original.keys()));
        assert_eq!(stored_paths(&engine), stored);
        drop(original);
        assert_eq!(memory.used(), 0);
        assert_eq!(engine.transaction_depth(), 0);
        original_index(&engine);
    }
    engine
        .build_path_index("saved00", "g", &[vec!["edge".into(), "edge".into()]])
        .unwrap();
    replaced_index(&engine);
    assert!(engine.drop_path_index("saved01", "g").unwrap());
    assert_eq!(engine.list_path_indexes().unwrap().len(), 15);
    drop(engine);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        assert_eq!(engine.list_path_indexes().unwrap().len(), 15);
        replaced_index(&engine);
        engine
            .add_graph_vertex(Vertex::new(4, "node"), "g")
            .unwrap();
        assert!(engine.list_path_indexes().unwrap().is_empty());
    }
}
