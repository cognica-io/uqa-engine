//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original graph diagnostics survive transaction failure and completion boundaries.

use std::collections::BTreeMap;
use uqa_graph::{cypher::CypherError, GraphStore, GraphStoreError};
use uqa_storage::{mvcc::VersionError, StorageBackendError};

use super::{integer_column, mutation_failures::persistent_engine, SQLError};

fn cypher_transaction_error(error: SQLError) -> CypherError {
    CypherError::from(StorageBackendError::backend("graph transaction", error))
}

fn assert_state(error: &StorageBackendError, state: &str) {
    let error = uqa_execution::storage_errors::storage_error("graph", error);
    assert_eq!(error.sqlstate(), Some(state), "{error}");
}

fn graph_error(state: &str) -> GraphStoreError {
    match state {
        "57014" => GraphStoreError::from(StorageBackendError::from(uqa_core::QueryCancelled)),
        "53200" => GraphStoreError::from(StorageBackendError::from(
            uqa_core::memory::MemoryError::SizeOverflow,
        )),
        "40001" => GraphStoreError::from(
            VersionError::WriteConflict {
                mutation: 0,
                expected: None,
                actual: None,
            }
            .into_storage_error(),
        ),
        _ => unreachable!(),
    }
}

#[test]
fn failed_graph_mutations_keep_their_cause_and_restore_the_active_transaction() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent_engine(provider, &directory.path().join("graphs.db"));
        engine.create_graph("g").unwrap();
        for state in ["57014", "53200", "40001"] {
            engine.begin().unwrap();
            let error = engine
                .graph_with_mut("g", |store| {
                    store.add_vertex(
                        uqa_core::Vertex {
                            vertex_id: 1,
                            label: "node".into(),
                            properties: BTreeMap::new(),
                        },
                        "g",
                    )?;
                    Err::<(), _>(graph_error(state))
                })
                .unwrap_err();
            assert_state(&error, state);
            assert_eq!(
                engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
                Some("25P02")
            );
            let error = engine
                .run_cypher("g", "MATCH (n) RETURN n", BTreeMap::new())
                .unwrap_err();
            assert_state(&StorageBackendError::backend("cypher", error), "25P02");
            engine.commit().unwrap();
            assert!(engine
                .graph_with("g", |store| store.get_vertex(1))
                .unwrap()
                .unwrap()
                .unwrap()
                .is_none());
        }
        engine.sql("BEGIN READ ONLY", &[]).unwrap();
        let error = engine
            .run_cypher("g", "CREATE (n:node)", BTreeMap::new())
            .unwrap_err();
        assert_state(&StorageBackendError::backend("cypher", error), "25006");
        engine.rollback().unwrap();
    }
}

#[test]
fn mapped_graph_transaction_retains_a_conflict_reported_only_at_commit() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent_engine(provider, &directory.path().join("completion.db"));
        engine.sql("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO items VALUES (1, 10)", &[]).unwrap();
        let peer = engine.new_session().unwrap();
        let error = engine
            .with_implicit_mapped_transaction(
                |engine| {
                    engine
                        .sql("UPDATE items SET value = 20 WHERE id = 1", &[])
                        .map_err(cypher_transaction_error)?;
                    // The public provider API can publish while a logical SQL writer is private. Its conflicting source record must be detected at the original commit boundary.
                    peer.storage
                        .backend
                        .as_ref()
                        .unwrap()
                        .document_store("public.items")
                        .put(
                            1,
                            [
                                ("id".into(), crate::Value::Int(1)),
                                ("value".into(), crate::Value::Int(30)),
                            ]
                            .into_iter()
                            .collect(),
                        )
                        .map_err(CypherError::from)?;
                    Ok(())
                },
                cypher_transaction_error,
            )
            .unwrap_err();
        assert_state(&StorageBackendError::backend("cypher", error), "40001");
        assert_eq!(engine.transaction_depth(), 0);
        assert_eq!(
            integer_column(
                &engine
                    .sql("SELECT value FROM items WHERE id = 1", &[])
                    .unwrap(),
                "value"
            ),
            [30]
        );
    }
}
