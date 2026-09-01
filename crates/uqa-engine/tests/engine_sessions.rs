//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent engine sessions must share one logical database while keeping
//! every piece of SQL session state and every `SQLite` transaction independent.

use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility, ScoringMode};
use uqa_sql::SQLError;
use uqa_storage::{Catalog, ManagedConnection, RelationIdentity, SequenceOptions, SequenceRow};

fn scalar_int(engine: &Engine, sql: &str, column: &str) -> i64 {
    match engine.sql(sql, &[]).unwrap().rows[0].get(column) {
        Some(Value::Int(value)) => *value,
        other => panic!("expected integer column `{column}`, got {other:?}"),
    }
}

fn scalar_float(engine: &Engine, sql: &str) -> f64 {
    let result = engine.sql(sql, &[]).unwrap();
    match result.rows[0].values().next().unwrap() {
        Value::Float(value) => *value,
        other => panic!("expected float, got {other:?}"),
    }
}

fn register_blocking_scalar(engine: &Engine, name: &str) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let function_name = name.to_string();
    engine
        .register_scalar_function_with_options(
            name,
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |args: &[Value]| {
                if !args.is_empty() {
                    return Err(SQLError::BadArity {
                        name: function_name.clone(),
                        expected: "no arguments".into(),
                        actual: args.len(),
                    });
                }
                entered_tx
                    .send(())
                    .map_err(|err| SQLError::Internal(format!("signal scalar entry: {err}")))?;
                release_rx
                    .lock()
                    .map_err(|_| SQLError::Internal("blocking scalar mutex poisoned".into()))?
                    .recv()
                    .map_err(|err| SQLError::Internal(format!("wait for scalar release: {err}")))?;
                Ok(Value::Int(1))
            },
        )
        .unwrap();
    (entered_rx, release_tx)
}

#[test]
fn one_engine_serializes_overlapping_sql_statements() {
    let engine = Arc::new(Engine::new());
    let (entered_rx, release_tx) = register_blocking_scalar(&engine, "block_same_session");

    let first_engine = engine.clone();
    let first = thread::spawn(move || first_engine.sql("SELECT block_same_session() AS n", &[]));
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first statement did not enter the blocking scalar");

    let second_engine = engine.clone();
    let second = thread::spawn(move || second_engine.sql("SELECT block_same_session() AS n", &[]));
    assert!(
        entered_rx.recv_timeout(Duration::from_millis(250)).is_err(),
        "a second statement entered the same Engine while its statement gate was held"
    );

    release_tx.send(()).unwrap();
    assert_eq!(first.join().unwrap().unwrap().rows[0]["n"], Value::Int(1));
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("second statement did not enter after the first released the gate");
    release_tx.send(()).unwrap();
    assert_eq!(second.join().unwrap().unwrap().rows[0]["n"], Value::Int(1));
}

#[test]
fn independent_sessions_run_read_statements_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent-read-sessions.db");
    let root = Engine::open(&path).unwrap();
    let (entered_rx, release_tx) = register_blocking_scalar(&root, "block_independent_session");
    let first = root.new_session().unwrap();
    let second = root.new_session().unwrap();

    let first = thread::spawn(move || first.sql("SELECT block_independent_session() AS n", &[]));
    let second = thread::spawn(move || second.sql("SELECT block_independent_session() AS n", &[]));

    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first session did not enter its read statement");
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("second session was serialized behind the first read statement");
    release_tx.send(()).unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(first.join().unwrap().unwrap().rows[0]["n"], Value::Int(1));
    assert_eq!(second.join().unwrap().unwrap().rows[0]["n"], Value::Int(1));
}

#[test]
fn persistent_sessions_isolate_all_session_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-state.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE SCHEMA alpha", &[]).unwrap();
    root.sql("CREATE SCHEMA beta", &[]).unwrap();
    root.sql("CREATE TABLE shared_t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();

    let alpha = root.new_session().unwrap();
    let beta = root.new_session().unwrap();

    alpha.sql("SET search_path TO alpha, public", &[]).unwrap();
    alpha.sql("SET work_mem TO '64MB'", &[]).unwrap();
    beta.sql("SET search_path TO beta, public", &[]).unwrap();
    beta.sql("SET work_mem TO '8MB'", &[]).unwrap();
    assert_eq!(alpha.search_path(), vec!["alpha", "public"]);
    assert_eq!(beta.search_path(), vec!["beta", "public"]);
    assert_eq!(root.search_path(), vec!["public"]);
    assert_eq!(alpha.show_variable("work_mem").unwrap(), "64MB");
    assert_eq!(beta.show_variable("work_mem").unwrap(), "8MB");
    assert_eq!(root.show_variable("work_mem").unwrap(), "64MB");

    alpha
        .sql("PREPARE only_alpha AS SELECT id FROM shared_t", &[])
        .unwrap();
    assert!(alpha.lookup_prepared("only_alpha").is_some());
    assert!(beta.lookup_prepared("only_alpha").is_none());
    assert!(root.lookup_prepared("only_alpha").is_none());

    alpha.cancel();
    assert!(alpha.is_cancelled());
    assert!(!beta.is_cancelled());
    assert!(!root.is_cancelled());
    beta.sql("SELECT id FROM shared_t", &[]).unwrap();
    alpha.reset_cancellation();

    alpha.begin().unwrap();
    assert_eq!(alpha.transaction_depth(), 1);
    assert_eq!(beta.transaction_depth(), 0);
    assert_eq!(root.transaction_depth(), 0);
    alpha.rollback().unwrap();
}

#[test]
fn committed_ddl_is_immediately_visible_to_existing_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ddl-visibility.db");
    let root = Engine::open(&path).unwrap();
    let creator = root.new_session().unwrap();
    let observer = root.new_session().unwrap();

    creator.sql("CREATE SCHEMA app", &[]).unwrap();
    creator
        .sql(
            "CREATE TABLE app.items (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
    assert!(observer.has_schema("app").unwrap());
    assert!(observer.has_table("app.items").unwrap());

    creator
        .sql("ALTER TABLE app.items ADD COLUMN version INTEGER", &[])
        .unwrap();
    assert!(observer.table_has_column("app.items", "version").unwrap());
    observer
        .sql(
            "INSERT INTO app.items (id, body, version) VALUES (1, 'visible', 7)",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar_int(
            &creator,
            "SELECT version FROM app.items WHERE id = 1",
            "version"
        ),
        7
    );

    creator.sql("SET search_path TO app, public", &[]).unwrap();
    creator
        .sql("ALTER TABLE app.items RENAME TO renamed", &[])
        .unwrap();
    assert!(!observer.has_table("app.items").unwrap());
    assert!(observer.has_table("app.renamed").unwrap());
    assert_eq!(
        scalar_int(&observer, "SELECT COUNT(*) AS n FROM app.renamed", "n"),
        1
    );
}

#[test]
fn committed_table_data_invalidates_sibling_value_count_and_statistics_caches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("table-data-cache-visibility.db");
    let root = Engine::open(&path).unwrap();
    root.sql(
        "CREATE TABLE cache_items (id INTEGER PRIMARY KEY, category TEXT)",
        &[],
    )
    .unwrap();
    root.sql(
        "CREATE INDEX cache_items_category_gin ON cache_items USING gin (category)",
        &[],
    )
    .unwrap();
    let writer = root.new_session().unwrap();
    let observer = root.new_session().unwrap();

    // Warm every session-local dependency while the durable table is empty:
    // the PK value index, document count, column statistics, and SQL plan.
    assert!(observer
        .sql("SELECT id FROM cache_items WHERE id = 1", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(observer.document_count("cache_items").unwrap(), 0);
    assert_eq!(
        observer.column_stats("cache_items").unwrap()["id"].row_count,
        0
    );

    writer.begin().unwrap();
    writer
        .sql(
            "INSERT INTO cache_items (id, category) VALUES (1, 'committed')",
            &[],
        )
        .unwrap();
    // The writer's dirty generation remains private until COMMIT.
    assert!(observer
        .sql("SELECT id FROM cache_items WHERE id = 1", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(observer.document_count("cache_items").unwrap(), 0);
    writer.commit().unwrap();

    let rows = observer
        .sql("SELECT id FROM cache_items WHERE id = 1", &[])
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], Value::Int(1));
    assert_eq!(observer.document_count("cache_items").unwrap(), 1);
    assert_eq!(
        observer.column_stats("cache_items").unwrap()["id"].row_count,
        1
    );

    writer.begin().unwrap();
    writer
        .sql(
            "INSERT INTO cache_items (id, category) VALUES (2, 'rolled back')",
            &[],
        )
        .unwrap();
    writer.rollback().unwrap();
    assert!(observer
        .sql("SELECT id FROM cache_items WHERE id = 2", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(observer.document_count("cache_items").unwrap(), 1);
}

#[test]
fn durable_registry_changes_are_private_until_commit_and_rollback_restores_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry-transaction-visibility.db");
    let root = Engine::open(&path).unwrap();
    let writer = root.new_session().unwrap();
    let observer = root.new_session().unwrap();

    writer.begin().unwrap();
    writer.create_graph("pending_graph").unwrap();
    writer
        .add_graph_vertex(
            uqa_core::Vertex {
                vertex_id: 1,
                label: "Person".into(),
                properties: BTreeMap::default(),
            },
            "pending_graph",
        )
        .unwrap();
    writer.sql("CREATE SCHEMA pending_schema", &[]).unwrap();
    writer
        .sql(
            "CREATE VIEW pending_schema.pending_view AS SELECT 1 AS n",
            &[],
        )
        .unwrap();

    assert!(writer.has_graph("pending_graph").unwrap());
    assert!(writer.has_schema("pending_schema").unwrap());
    assert!(writer
        .view("pending_schema.pending_view")
        .unwrap()
        .is_some());
    assert!(!observer.has_graph("pending_graph").unwrap());
    assert!(!observer.has_schema("pending_schema").unwrap());
    assert!(observer
        .view("pending_schema.pending_view")
        .unwrap()
        .is_none());

    writer.rollback().unwrap();
    assert!(!writer.has_graph("pending_graph").unwrap());
    assert!(!writer.has_schema("pending_schema").unwrap());
    assert!(writer
        .view("pending_schema.pending_view")
        .unwrap()
        .is_none());

    writer.begin().unwrap();
    writer.create_graph("committed_graph").unwrap();
    writer.sql("CREATE SCHEMA committed_schema", &[]).unwrap();
    writer
        .sql(
            "CREATE VIEW committed_schema.committed_view AS SELECT 2 AS n",
            &[],
        )
        .unwrap();
    writer.commit().unwrap();

    assert!(observer.has_graph("committed_graph").unwrap());
    assert!(observer.has_schema("committed_schema").unwrap());
    assert!(observer
        .view("committed_schema.committed_view")
        .unwrap()
        .is_some());
}

fn create_cross_store_table(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE INDEX docs_embedding_ivf ON docs USING ivf (embedding) WITH (lists = 1, probes = 1, train_threshold = 1)",
            &[],
        )
        .unwrap();
}

fn assert_cross_store_rollback_visible(observer: &Engine) {
    assert!(observer
        .load_scoring_params("transaction_marker")
        .unwrap()
        .is_none());
    assert!(observer.get_document("docs", 1).unwrap().is_none());
    assert!(observer
        .search("docs", "body", "pending", &ScoringMode::default(), 10)
        .unwrap()
        .is_empty());
    assert!(observer
        .knn_search("docs", "embedding", [1.0, 0.0], 10)
        .unwrap()
        .is_empty());
}

fn assert_cross_store_commit_visible(observer: &Engine) {
    assert_eq!(
        observer.load_scoring_params("transaction_marker").unwrap(),
        Some(r#"{"phase":"commit"}"#.to_string())
    );
    assert!(observer.get_document("docs", 2).unwrap().is_some());
    assert_eq!(
        observer
            .search("docs", "body", "committed", &ScoringMode::default(), 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        observer
            .knn_search("docs", "embedding", [0.0, 1.0], 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn one_session_transaction_is_atomic_across_catalog_documents_text_and_vectors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cross-store-transaction.db");
    let root = Engine::open(&path).unwrap();
    create_cross_store_table(&root);

    let writer = root.new_session().unwrap();
    let observer = root.new_session().unwrap();
    writer.begin().unwrap();
    writer
        .save_scoring_params("transaction_marker", r#"{"phase":"rollback"}"#)
        .unwrap();
    writer
        .sql(
            "INSERT INTO docs (id, body, embedding) VALUES (1, 'pending token', ARRAY[1.0, 0.0])",
            &[],
        )
        .unwrap();

    assert!(writer.get_document("docs", 1).unwrap().is_some());
    assert_eq!(
        writer
            .search("docs", "body", "pending", &ScoringMode::default(), 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        writer
            .knn_search("docs", "embedding", [1.0, 0.0], 10)
            .unwrap()
            .len(),
        1
    );

    // A WAL reader must complete while the writer holds its pinned
    // transaction, and it must not observe any of the uncommitted stores.
    let (observed_tx, observed_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let scoring = observer.load_scoring_params("transaction_marker");
            let document = observer.get_document("docs", 1);
            let text_hits = observer
                .search("docs", "body", "pending", &ScoringMode::default(), 10)
                .unwrap()
                .len();
            let vector_hits = observer
                .knn_search("docs", "embedding", [1.0, 0.0], 10)
                .unwrap()
                .len();
            observed_tx
                .send((scoring, document, text_hits, vector_hits))
                .unwrap();
        });
        let read_result = observed_rx.recv_timeout(Duration::from_secs(2));
        // Always release the writer before the scope joins, even when the
        // read timed out, so a serialization regression cannot hang the test.
        writer.rollback().unwrap();
        let (params, document, text_hits, vector_hits) =
            read_result.expect("independent WAL reader was serialized behind the writer");
        assert!(params.unwrap().is_none());
        assert!(document.unwrap().is_none());
        assert_eq!(text_hits, 0);
        assert_eq!(vector_hits, 0);
    });

    assert_cross_store_rollback_visible(&observer);

    writer.begin().unwrap();
    writer
        .save_scoring_params("transaction_marker", r#"{"phase":"commit"}"#)
        .unwrap();
    writer
        .sql(
            "INSERT INTO docs (id, body, embedding) VALUES (2, 'committed token', ARRAY[0.0, 1.0])",
            &[],
        )
        .unwrap();
    writer.commit().unwrap();

    assert_cross_store_commit_visible(&observer);
}

#[test]
fn sequence_values_are_unique_across_sessions_and_independent_opens() {
    const SESSION_COUNT: usize = 4;
    const VALUES_PER_SESSION: usize = 25;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("global-sequence.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE SEQUENCE global_ids START 1", &[]).unwrap();

    let mut sessions = (0..SESSION_COUNT - 1)
        .map(|_| root.new_session().unwrap())
        .collect::<Vec<_>>();
    // A separately opened Engine must use the durable atomic sequence too;
    // correctness cannot depend on sharing an in-process cache.
    sessions.push(Engine::open(&path).unwrap());

    let handles = sessions
        .into_iter()
        .map(|session| {
            thread::spawn(move || {
                (0..VALUES_PER_SESSION)
                    .map(|_| session.nextval("global_ids").unwrap())
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let mut values = handles
        .into_iter()
        .flat_map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    values.sort_unstable();

    let expected = (1..=(SESSION_COUNT * VALUES_PER_SESSION) as i64).collect::<Vec<_>>();
    assert_eq!(
        values, expected,
        "sequence returned duplicate or lost values"
    );

    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        reopened.nextval("global_ids").unwrap(),
        (SESSION_COUNT * VALUES_PER_SESSION + 1) as i64
    );
}

#[test]
fn sequence_cache_blocks_are_session_local_and_definition_invalidations_are_global() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session-sequence-cache.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE SEQUENCE cached_ids CACHE 5", &[]).unwrap();
    let sibling = root.new_session().unwrap();

    assert_eq!(root.nextval("cached_ids").unwrap(), 1);
    assert_eq!(
        root.sequence_state("cached_ids")
            .unwrap()
            .unwrap()
            .1
            .current,
        5
    );
    sibling.setval("cached_ids", 15).unwrap();
    assert_eq!(root.nextval("cached_ids").unwrap(), 2);
    assert_eq!(
        root.sequence_state("cached_ids")
            .unwrap()
            .unwrap()
            .1
            .current,
        15
    );

    assert_eq!(sibling.nextval("cached_ids").unwrap(), 16);
    assert_eq!(
        sibling
            .sequence_state("cached_ids")
            .unwrap()
            .unwrap()
            .1
            .current,
        20
    );
    root.sql("ALTER SEQUENCE cached_ids CACHE 2", &[]).unwrap();
    assert_eq!(root.nextval("cached_ids").unwrap(), 21);
    assert_eq!(
        root.sequence_state("cached_ids")
            .unwrap()
            .unwrap()
            .1
            .current,
        22
    );
    assert_eq!(sibling.nextval("cached_ids").unwrap(), 23);
    assert_eq!(
        sibling
            .sequence_state("cached_ids")
            .unwrap()
            .unwrap()
            .1
            .current,
        24
    );
    assert_eq!(root.nextval("cached_ids").unwrap(), 22);

    drop(sibling);
    drop(root);
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(reopened.nextval("cached_ids").unwrap(), 25);
    assert_eq!(
        reopened
            .sequence_state("cached_ids")
            .unwrap()
            .unwrap()
            .1
            .current,
        26
    );
}

#[test]
fn sequence_persistence_changes_invalidate_remote_caches_only_when_state_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session-sequence-persistence.db");
    let root = Engine::open(&path).unwrap();
    root.sql(
        "CREATE SEQUENCE changed_ids CACHE 3;
         CREATE SEQUENCE unchanged_ids CACHE 3",
        &[],
    )
    .unwrap();
    let sibling = root.new_session().unwrap();

    assert_eq!(root.nextval("changed_ids").unwrap(), 1);
    assert_eq!(root.nextval("unchanged_ids").unwrap(), 1);
    sibling
        .sql(
            "ALTER SEQUENCE changed_ids SET UNLOGGED;
             ALTER SEQUENCE unchanged_ids SET LOGGED",
            &[],
        )
        .unwrap();

    assert_eq!(root.nextval("changed_ids").unwrap(), 4);
    assert_eq!(root.nextval("unchanged_ids").unwrap(), 2);
    assert_eq!(root.currval("changed_ids").unwrap(), 4);
    assert_eq!(root.currval("unchanged_ids").unwrap(), 2);
}

#[test]
fn currval_is_owned_by_the_logical_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-currval.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE SEQUENCE session_ids START 10", &[])
        .unwrap();
    let first = root.new_session().unwrap();
    let second = root.new_session().unwrap();

    assert!(root.currval("session_ids").is_err());
    assert!(first.currval("session_ids").is_err());
    assert!(second.currval("session_ids").is_err());

    assert_eq!(first.nextval("session_ids").unwrap(), 10);
    assert_eq!(first.currval("session_ids").unwrap(), 10);
    assert!(root.currval("session_ids").is_err());
    assert!(second.currval("session_ids").is_err());

    assert_eq!(second.nextval("session_ids").unwrap(), 11);
    assert_eq!(second.currval("session_ids").unwrap(), 11);
    assert_eq!(first.currval("session_ids").unwrap(), 10);
    assert!(root.currval("session_ids").is_err());
}

#[test]
fn currval_does_not_follow_a_recreated_sequence_with_the_same_name() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sequence-incarnation.db");
    let stale_session = Engine::open(&path).unwrap();
    stale_session
        .sql("CREATE SEQUENCE public.session_ids START 10", &[])
        .unwrap();
    assert_eq!(stale_session.nextval("session_ids").unwrap(), 10);
    assert_eq!(stale_session.currval("session_ids").unwrap(), 10);
    assert_eq!(stale_session.lastval().unwrap(), 10);

    let ddl_session = Engine::open(&path).unwrap();
    assert!(ddl_session.drop_sequence("session_ids").unwrap());
    assert!(ddl_session
        .create_sequence("session_ids", 100, 1, false)
        .unwrap());

    assert!(stale_session.currval("session_ids").is_err());
    assert!(stale_session.lastval().is_err());
    assert_eq!(stale_session.nextval("session_ids").unwrap(), 100);
    assert_eq!(stale_session.currval("session_ids").unwrap(), 100);
    assert_eq!(stale_session.lastval().unwrap(), 100);
}

#[test]
fn opening_an_engine_assigns_legacy_sequence_object_identities() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-sequence-identity.db");
    let catalog = Catalog::open(ManagedConnection::open(&path).unwrap()).unwrap();
    catalog.save_schema("public").unwrap();
    assert!(catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_ids"),
            object_id: [0; 16],
            definition_generation: [0; 16],
            start: 5,
            increment: 1,
            current: 5,
            called: false,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap());
    drop(catalog);

    let engine = Engine::open(&path).unwrap();
    assert_eq!(engine.nextval("legacy_ids").unwrap(), 5);
    drop(engine);

    let catalog = Catalog::open(ManagedConnection::open(&path).unwrap()).unwrap();
    let sequence = catalog.load_sequence_rows().unwrap().remove(0);
    assert_ne!(sequence.object_id, [0; 16]);
}

#[test]
fn independently_opened_engine_refreshes_direct_reads_after_external_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("external-direct-read.db");
    let observer = Engine::open(&path).unwrap();
    let writer = Engine::open(&path).unwrap();

    writer.sql("CREATE SCHEMA external", &[]).unwrap();
    writer
        .sql(
            "CREATE TABLE external.docs (id INTEGER PRIMARY KEY, body TEXT); \
             INSERT INTO external.docs (id, body) VALUES (1, 'visible')",
            &[],
        )
        .unwrap();
    writer.create_graph("external_graph").unwrap();

    assert!(observer.has_schema("external").unwrap());
    assert!(observer.has_table("external.docs").unwrap());
    assert_eq!(
        observer
            .get_document("external.docs", 1)
            .unwrap()
            .unwrap()
            .get("body"),
        Some(&uqa_core::Value::Str("visible".into()))
    );
    assert!(observer.has_graph("external_graph").unwrap());
}

#[test]
fn legacy_sequence_snapshot_is_consumed_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-sequence.db");
    let catalog = Catalog::open(ManagedConnection::open(&path).unwrap()).unwrap();
    catalog
        .set_metadata(
            "sql_sequences_json",
            r#"{"legacy":{"start":1,"increment":1,"current":0}}"#,
        )
        .unwrap();
    drop(catalog);

    let engine = Engine::open(&path).unwrap();
    assert_eq!(engine.nextval("legacy").unwrap(), 1);
    assert!(engine.drop_sequence("legacy").unwrap());
    drop(engine);

    let reopened = Engine::open(&path).unwrap();
    assert!(
        reopened.nextval("legacy").is_err(),
        "a consumed legacy snapshot must not resurrect a dropped sequence"
    );
}

#[test]
fn random_seed_state_is_reproducible_and_session_local() {
    let dir = tempfile::tempdir().unwrap();
    let root = Engine::open(&dir.path().join("random-sessions.db")).unwrap();
    let alpha = root.new_session().unwrap();
    let beta = root.new_session().unwrap();

    alpha.sql("SELECT setseed(0.25)", &[]).unwrap();
    beta.sql("SELECT setseed(0.25)", &[]).unwrap();

    let alpha_first = scalar_float(&alpha, "SELECT random()");
    let alpha_second = scalar_float(&alpha, "SELECT random()");
    let beta_first = scalar_float(&beta, "SELECT random()");
    let beta_second = scalar_float(&beta, "SELECT random()");
    assert_eq!((alpha_first, alpha_second), (beta_first, beta_second));

    alpha.sql("SELECT setseed(0.25)", &[]).unwrap();
    assert_eq!(scalar_float(&alpha, "SELECT random()"), alpha_first);
}
