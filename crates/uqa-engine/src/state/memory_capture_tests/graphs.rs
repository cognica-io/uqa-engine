//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::sql;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
use super::{Engine, StorageReadControl};
use crate::tests::providers::Provider;
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, Vertex};
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
use uqa_graph::GraphImageSnapshot;
use uqa_graph::{GraphCatalog, GraphStore};

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
fn image(source: &GraphImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(1))
        .unwrap();
    bytes
}

#[test]
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
fn committed_graph_capture_keeps_original_membership_after_rename_and_retirement() {
    let engine = Engine::new();
    for id in 0..64 {
        engine
            .add_graph_vertex(Vertex::new(id + 1, "node"), &format!("g-{id:02}"))
            .unwrap();
    }
    let source = Arc::downgrade(&engine.durable.graphs.read()["g-00"]);
    let unread = Arc::downgrade(&engine.durable.graphs.read()["g-01"]);
    let root = engine.durable.graphs.snapshot().identity();
    let expected = image(
        &engine.durable.graphs.read()["g-00"]
            .capture_image_with_control(&StorageReadControl::with_limit(1 << 20))
            .unwrap(),
    );
    let control = StorageReadControl::with_limit(8 << 20);
    let cut = engine.capture_committed_memory(&control).unwrap();
    sql(&engine, "BEGIN; SAVEPOINT saved_graphs");
    engine.rename_graph("g-00", "renamed").unwrap();
    engine.drop_graph("g-01").unwrap();
    sql(&engine, "ROLLBACK TO saved_graphs; COMMIT");
    assert!(engine.has_graph("g-00").unwrap());
    assert!(engine.has_graph("g-01").unwrap());
    assert!(!engine.has_graph("renamed").unwrap());
    engine.rename_graph("g-00", "renamed").unwrap();
    engine.drop_graph("g-01").unwrap();
    drop(engine);
    assert!(!root.is_alive());
    let graphs = cut.resources.graphs.captured(&cut.cut).unwrap();
    assert_eq!(graphs.get().unwrap().len(), 64);
    assert_eq!(graphs.get().unwrap().names().next(), Some("g-00"));
    assert!(graphs.get().unwrap().image("renamed").unwrap().is_none());
    let child = graphs.get().unwrap().image("g-00").unwrap().unwrap();
    drop((graphs, cut));
    assert!(source.upgrade().is_some());
    assert!(unread.upgrade().is_none());
    assert_eq!(image(&child), expected);
    drop(child);
    assert!(source.upgrade().is_none());
    assert_eq!(control.memory().used(), 0);
}

#[test]
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
fn committed_graph_image_reader_owns_its_source_after_engine_and_cut_retirement() {
    let engine = Engine::new();
    engine
        .add_graph_vertex(Vertex::new(1, "selected"), "g")
        .unwrap();
    let original_source = Arc::downgrade(&engine.durable.graphs.read()["g"]);
    let original = StorageReadControl::with_limit(4 << 20);
    let invoking = StorageReadControl::with_limit(256 << 10);
    let cut = engine.capture_committed_memory(&original).unwrap();
    let graphs = cut.resources.graphs.captured(&cut.cut).unwrap();
    let selected = graphs.get().unwrap().image("g").unwrap().unwrap();
    let mut reader = selected.into_image_reader(&invoking).unwrap();
    engine
        .add_graph_vertex(Vertex::new(2, "later"), "g")
        .unwrap();
    drop((graphs, cut, engine));
    assert!(original_source.upgrade().is_some());
    let bytes = std::thread::spawn(move || {
        while !reader.prepare(1).unwrap().ready {}
        let mut bytes = Vec::new();
        let mut buffer = [0; 17];
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                return bytes;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
    })
    .join()
    .unwrap();
    assert!(original_source.upgrade().is_none());
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
    let destination = StorageReadControl::with_limit(1 << 20);
    let graph =
        uqa_graph::MemoryGraphStore::read_image(&mut bytes.as_slice(), &destination).unwrap();
    assert_eq!(graph.vertex_ids_in_graph("g").unwrap(), [1].into());
    assert_eq!(graph.get_vertex(1).unwrap().label, "selected");
    assert!(graph.get_vertex(2).is_none());
    drop(graph);
    assert_eq!(destination.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_graph_catalog_denial_rolls_back_entities_names_and_path_definitions(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("graphs.db"));
    for name in ["g", "h", "z"] {
        engine
            .add_graph_vertex(Vertex::new(1, "node"), name)
            .unwrap();
    }
    engine
        .build_path_index("saved", "g", &[vec!["edge".into()]])
        .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let mut admitted = GraphCatalog::new(&memory);
    for (name, graph) in engine.durable.graphs.read().iter() {
        admitted.insert(name, Arc::clone(graph)).unwrap();
    }
    *engine.durable.graphs.write() = admitted;
    let original_oids = engine.durable.graph_catalog_oids.snapshot();
    for operation in 0..5 {
        if operation == 4 {
            sql(&engine, "BEGIN");
        }
        let held = memory.reserve(memory.available()).unwrap();
        let result = match operation {
            0 => engine.create_graph("later").map(|_| ()),
            1 => engine.drop_graph("g").map(|_| ()),
            2 => engine.rename_graph("g", "later").map(|_| ()),
            _ => engine.add_graph_vertex(Vertex::new(2, "denied"), "g"),
        };
        let error = result.unwrap_err();
        assert_eq!(
            uqa_execution::storage_errors::storage_error("graph publication", &error).sqlstate(),
            Some("53200"),
            "{provider:?}, {operation}: {error}"
        );
        if operation == 4 {
            sql(&engine, "ROLLBACK");
        }
        assert_eq!(engine.list_graphs().unwrap(), ["g", "h", "z"]);
        assert_eq!(
            engine
                .graph_with("g", |graph| graph
                    .vertex_ids_in_graph("g")
                    .unwrap()
                    .into_iter()
                    .collect::<Vec<_>>())
                .unwrap()
                .unwrap(),
            [1]
        );
        assert!(engine.get_path_index("saved", "g").unwrap().is_some());
        assert!(engine
            .durable
            .graph_catalog_oids
            .read()
            .iter()
            .eq(original_oids.iter()));
        drop(held);
    }
    engine.rename_graph("g", "renamed").unwrap();
    engine
        .add_graph_vertex(Vertex::new(2, "accepted"), "renamed")
        .unwrap();
    engine.drop_graph("h").unwrap();
    assert_eq!(engine.list_graphs().unwrap(), ["renamed", "z"]);
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&directory.path().join("graphs.db"));
        assert_eq!(reopened.list_graphs().unwrap(), ["renamed", "z"]);
        assert_eq!(
            reopened
                .graph_with("renamed", |graph| graph
                    .vertex_ids_in_graph("renamed")
                    .unwrap()
                    .into_iter()
                    .collect::<Vec<_>>())
                .unwrap()
                .unwrap(),
            [1, 2]
        );
    }
}
