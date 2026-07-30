//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistence/atomicity regressions for graph mutation and dependent path
//! indexes. A graph snapshot replacement deletes its path-index catalog rows
//! in the same transaction; failed replacement must roll both changes back.

use std::{collections::BTreeMap, sync::Arc};

use tempfile::TempDir;
use uqa_core::{Edge, Vertex};
use uqa_engine::Engine;
use uqa_graph::{cypher::CypherError, GraphStore as _};
use uqa_storage::{
    Catalog, CatalogFacade, ManagedConnection, PersistentStorageBackend, SQLiteStorageBackend,
};

fn persistent_engine() -> (TempDir, ManagedConnection, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("graph-path-index-atomicity.db");
    let connection = ManagedConnection::open(&database).unwrap();
    let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(connection.clone()).unwrap());
    let backend: Arc<dyn PersistentStorageBackend> =
        Arc::new(SQLiteStorageBackend::new(connection.clone()));
    let engine = Engine::from_persistent_backends(catalog, backend).unwrap();
    (directory, connection, engine)
}

fn install_membership_insert_failure(connection: &ManagedConnection) {
    connection
        .with(|sqlite| {
            sqlite.execute_batch(
                "CREATE TRIGGER injected_graph_membership_failure
                 BEFORE INSERT ON _graph_membership
                 BEGIN SELECT RAISE(FAIL, 'injected graph membership failure'); END;",
            )?;
            Ok(())
        })
        .unwrap();
}

fn clear_membership_insert_failure(connection: &ManagedConnection) {
    connection
        .with(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER injected_graph_membership_failure")?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn failed_graph_replacement_preserves_graph_and_path_index_after_reopen() {
    let (directory, connection, engine) = persistent_engine();
    engine.create_graph("g").unwrap();
    engine.add_graph_vertex(Vertex::new(1, "P"), "g").unwrap();
    engine.add_graph_vertex(Vertex::new(2, "P"), "g").unwrap();
    engine
        .add_graph_edge(Edge::new(10, 1, 2, "knows"), "g")
        .unwrap();
    engine
        .build_path_index("k", "g", &[vec!["knows".to_string()]])
        .unwrap();

    install_membership_insert_failure(&connection);
    assert!(engine.add_graph_vertex(Vertex::new(3, "P"), "g").is_err());
    assert!(engine.get_path_index("k", "g").unwrap().is_some());
    let live_vertices = engine
        .graph_with("g", |store| store.vertex_ids_in_graph("g").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(live_vertices.into_iter().collect::<Vec<_>>(), vec![1, 2]);
    clear_membership_insert_failure(&connection);

    let database = directory.path().join("graph-path-index-atomicity.db");
    drop(engine);
    drop(connection);

    let reopened = Engine::open(&database).unwrap();
    let reopened_vertices = reopened
        .graph_with("g", |store| store.vertex_ids_in_graph("g").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        reopened_vertices.into_iter().collect::<Vec<_>>(),
        vec![1, 2]
    );
    let index = reopened
        .get_path_index("k", "g")
        .unwrap()
        .expect("the rolled-back path index must survive reopen");
    let pairs = index
        .lookup(&["knows".to_string()])
        .expect("persisted path sequence must be restored");
    assert_eq!(pairs.iter().copied().collect::<Vec<_>>(), vec![(1, 2)]);
}

#[test]
fn run_cypher_rolls_back_catalog_failure_without_erasing_error_types() {
    let (_directory, connection, engine) = persistent_engine();
    engine.create_graph("g").unwrap();
    install_membership_insert_failure(&connection);

    let storage_error = engine
        .run_cypher("g", "CREATE (:P {name: 'lost'})", BTreeMap::default())
        .expect_err("the injected catalog failure must abort Cypher mutation");
    assert!(
        matches!(storage_error, CypherError::Storage(ref message) if message.contains("injected graph membership failure")),
        "unexpected Cypher storage error: {storage_error}"
    );
    clear_membership_insert_failure(&connection);

    let vertices = engine
        .graph_with("g", |store| store.vertex_ids_in_graph("g").unwrap())
        .unwrap()
        .unwrap();
    assert!(vertices.is_empty());

    let parse_error = engine
        .run_cypher("g", "MATCH (", BTreeMap::default())
        .expect_err("parse errors must retain their public Cypher variant");
    assert!(matches!(parse_error, CypherError::Parse(_)));
}
