//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public mutation families must roll back together at native MVCC materialization.

use super::{create_cross_store_table, scalar_int, Arc, Engine, Value};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::Vertex;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::mvcc::native::NativeRecordFamily;
use uqa_storage_sqlite::{ManagedConnection, SQLiteCompressionOptions, SQLiteStorageProvider};

fn connection(path: &Path, mode: usize) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "native publication fixture"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        3 => ManagedConnection::open_compressed_encrypted(
            path,
            "native publication fixture",
            SQLiteCompressionOptions::default(),
        ),
        _ => unreachable!(),
    }
    .unwrap()
}

fn engine(connection: &ManagedConnection) -> Engine {
    let engine =
        Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(connection.clone())))
            .unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    engine
}

fn prepare(engine: &Engine) {
    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("CREATE SCHEMA atomic_added; CREATE TABLE atomic_added.items(id INTEGER); CREATE VIEW atomic_added.answer AS SELECT 42 AS value; CREATE SEQUENCE atomic_added.seq; CREATE FUNCTION atomic_added.answer_fn() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 42'; CREATE INDEX atomic_body_btree ON docs(body); GRANT SELECT ON docs TO atomic_reader", &[]).unwrap();
    engine
        .register_named_analyzer("atomic_added_analyzer", r#"{"tokenizer":"keyword"}"#)
        .unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "atomic_added_analyzer", "both")
        .unwrap();
    engine
        .save_model(
            "atomic_added_model",
            &DeepModel {
                layers: vec![DeepLayerSpec::Embed {
                    embedding: vec![1.0],
                }],
                alpha: 0.0,
                gating: GatingSpec::None,
            },
        )
        .unwrap();
    engine
        .add_graph_vertex(Vertex::new(1, "Person"), "atomic_graph")
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs VALUES (mutation_probe(), 'published token', ARRAY[0.0, 1.0])",
            &[],
        )
        .unwrap();
}

fn assert_original(engine: &Engine) {
    assert_eq!(scalar_int(engine, "SELECT count(*) AS n FROM docs", "n"), 1);
    assert_eq!(
        engine.sql("SELECT body FROM docs", &[]).unwrap().rows[0]["body"],
        Value::Str("original".into())
    );
    assert!(engine
        .search(
            "docs",
            "body",
            "published",
            &super::ScoringMode::default(),
            10
        )
        .unwrap()
        .is_empty());
    let neighbors = engine
        .knn_search("docs", "embedding", [1.0, 0.0], 10)
        .unwrap();
    assert_eq!(neighbors.len(), 1);
    assert!((neighbors[0].score - 1.0).abs() < 1e-6);
    assert!(!engine.has_schema("atomic_added").unwrap());
    assert!(!engine.has_catalog_index("atomic_body_btree").unwrap());
    assert!(!engine
        .list_named_analyzers()
        .unwrap()
        .iter()
        .any(|name| name == "atomic_added_analyzer"));
    assert!(engine.load_model("atomic_added_model").unwrap().is_none());
    assert_eq!(
        scalar_int(
            engine,
            "SELECT count(*) AS n FROM pg_catalog.pg_proc WHERE proname = 'answer_fn'",
            "n"
        ),
        0
    );
    assert!(engine
        .table_field_analyzer("docs", "body")
        .unwrap()
        .is_none());
    assert_eq!(
        engine
            .sql(
                "SELECT has_table_privilege('atomic_reader', 'docs', 'SELECT') AS allowed",
                &[]
            )
            .unwrap()
            .rows[0]["allowed"],
        Value::Bool(false)
    );
    assert_eq!(scalar_int(engine, "SELECT count(*) AS n FROM cypher('atomic_graph', $$ MATCH (n) RETURN id(n) $$) AS result(id agtype)", "n"), 0);
}

#[test]
fn native_mutation_family_failures_preserve_sibling_and_reopened_state_without_replay() {
    use NativeRecordFamily as Family;
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        for family in [
            Family::Documents,
            Family::BtreeIndexEntries,
            Family::OccurrenceDocuments,
            Family::Vectors,
            Family::IVFAssignments,
            Family::Schemas,
            Family::Tables,
            Family::Analyzers,
            Family::Models,
            Family::Metadata,
            Family::TableFieldAnalyzers,
            Family::GraphVertices,
            Family::GraphMembership,
            Family::Views,
            Family::Sequences,
            Family::CatalogIndexes,
        ] {
            let path = directory.path().join(format!("{mode}-{}.db", family.id()));
            let connection = connection(&path, mode);
            let root = engine(&connection);
            create_cross_store_table(&root);
            root.sql("INSERT INTO docs VALUES (1, 'original', ARRAY[1.0, 0.0]); CREATE ROLE atomic_reader", &[]).unwrap();
            root.create_graph("atomic_graph").unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let callback = Arc::clone(&calls);
            root.register_scalar_function_with_options(
                "mutation_probe",
                super::SQLFunctionOptions::read_only(super::SQLFunctionVolatility::Volatile),
                move |_: &[Value]| {
                    callback.fetch_add(1, Ordering::AcqRel);
                    Ok(Value::Int(2))
                },
            )
            .unwrap();
            let observer = root.new_session().unwrap();
            prepare(&root);
            assert_eq!(calls.load(Ordering::Acquire), 1);
            assert_original(&observer);
            connection.with_physical(|physical| {
                physical.execute_batch(&format!("CREATE TRIGGER reject_native_publication BEFORE INSERT ON {} BEGIN SELECT RAISE(ABORT, 'injected native publication failure'); END", family.layout().table))?;
                Ok(())
            }).unwrap();
            let error = root.commit().unwrap_err();
            assert_ne!(error.sqlstate(), Some("08007"));
            assert!(
                error
                    .to_string()
                    .contains("injected native publication failure"),
                "mode {mode}, {family:?}: {error}"
            );
            assert_eq!(calls.load(Ordering::Acquire), 1);
            assert_original(&observer);
            connection
                .with_physical(|physical| {
                    physical.execute_batch("DROP TRIGGER reject_native_publication")?;
                    Ok(())
                })
                .unwrap();
            if root.transaction_depth() > 0 {
                root.rollback().unwrap();
            }
            assert_original(&root);
            drop((observer, root, connection));
            let reopened_connection = self::connection(&path, mode);
            let reopened = engine(&reopened_connection);
            assert_original(&reopened);
        }
    }
}
