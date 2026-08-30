//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{mpsc, Arc, Barrier};
use std::time::Duration;

use uqa_core::Value;
use uqa_storage::sqlite::{Catalog, ManagedConnection};
use uqa_storage::{ColumnStatsInput, SQLiteStorageBackend};

use super::Engine;

fn sqlite_data_version(engine: &Engine) -> u64 {
    engine
        .storage
        .backend
        .as_ref()
        .expect("persistent test engine")
        .change_version()
        .expect("read storage change version")
        .expect("file-backed database has a data version")
}

#[test]
fn contended_transaction_stack_does_not_hide_autocommit_data_generation() {
    let engine = Arc::new(Engine::new());
    let initial_epoch = engine
        .epochs
        .table_data
        .published
        .load(std::sync::atomic::Ordering::Acquire);
    let locked = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));

    let holder_engine = Arc::clone(&engine);
    let holder_locked = Arc::clone(&locked);
    let holder_release = Arc::clone(&release);
    let holder = std::thread::spawn(move || {
        let _guard = holder_engine.session.transactions.lock();
        holder_locked.wait();
        holder_release.wait();
    });
    locked.wait();

    let (done_tx, done_rx) = mpsc::channel();
    let notifier_engine = Arc::clone(&engine);
    let notifier = std::thread::spawn(move || {
        notifier_engine.note_table_data_changed();
        done_tx.send(()).unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
    release.wait();
    done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    holder.join().unwrap();
    notifier.join().unwrap();

    assert_eq!(
        engine
            .epochs
            .table_data
            .published
            .load(std::sync::atomic::Ordering::Acquire),
        initial_epoch + 1
    );
    assert!(!engine
        .epochs
        .table_data
        .dirty
        .load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn initial_restore_eagerly_loads_column_statistics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("eager-column-statistics.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER); \
                 INSERT INTO t (id, val) VALUES (1, 10)",
                &[],
            )
            .unwrap();
    }

    let connection = ManagedConnection::open(&path).unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog
        .save_column_stats(ColumnStatsInput::basic(
            "public.t", "val", 1, 0, None, None, 999,
        ))
        .unwrap();
    let engine = Engine::from_persistent_backends(
        Arc::new(catalog),
        Arc::new(SQLiteStorageBackend::new(connection)),
    )
    .unwrap();

    let table = engine
        .storage
        .tables
        .read()
        .values()
        .next()
        .cloned()
        .expect("restored table");
    assert_eq!(table.column_stats.read()["val"].row_count, 999);
}

#[test]
fn independently_opened_backend_pairs_share_row_locks_for_the_same_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared-backend-row-locks.db");
    let seed = Engine::open(&path).unwrap();
    seed.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    seed.sql("INSERT INTO t VALUES (1)", &[]).unwrap();
    drop(seed);

    let open_engine = || {
        let connection = ManagedConnection::open(&path).unwrap();
        let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
        let backend = Arc::new(SQLiteStorageBackend::new(connection));
        Engine::from_persistent_backends(catalog, backend).unwrap()
    };
    let holder = open_engine();
    let contender = open_engine();
    holder.sql("BEGIN", &[]).unwrap();
    holder
        .sql("SELECT id FROM t WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let error = contender
        .sql("SELECT id FROM t WHERE id = 1 FOR UPDATE NOWAIT", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
    holder.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn backend_pair_wait_rechecks_through_an_independent_committed_session() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backend-pair-committed-recheck.db");
    let seed = Engine::open(&path).unwrap();
    seed.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)", &[])
        .unwrap();
    seed.sql("INSERT INTO t VALUES (1, 0)", &[]).unwrap();
    drop(seed);

    let open_engine = || {
        let connection = ManagedConnection::open(&path).unwrap();
        let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
        let backend = Arc::new(SQLiteStorageBackend::new(connection));
        Engine::from_persistent_backends(catalog, backend).unwrap()
    };
    let holder = open_engine();
    let waiter = open_engine();
    holder.sql("BEGIN", &[]).unwrap();
    holder.sql("UPDATE t SET v = 99 WHERE id = 1", &[]).unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql("SELECT v FROM t WHERE id = 1 FOR UPDATE", &[]))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    holder.sql("COMMIT", &[]).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("v"), Some(&Value::Int(99)));
}

#[test]
fn pinned_and_rollback_reload_do_not_consume_late_legacy_sequences() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("late-legacy-sequence.db")).unwrap();
    let catalog = engine.storage.catalog.as_ref().expect("persistent catalog");
    let legacy = r#"{"late":{"start":7,"increment":2,"current":5}}"#;
    catalog
        .set_metadata(crate::SEQUENCES_METADATA_KEY, legacy)
        .unwrap();
    let before = sqlite_data_version(&engine);

    engine.begin_implicit_statement_transaction(true).unwrap();
    assert_eq!(sqlite_data_version(&engine), before);
    engine.rollback().unwrap();

    assert_eq!(sqlite_data_version(&engine), before);
    assert_eq!(
        catalog
            .get_metadata(crate::SEQUENCES_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(legacy)
    );
    assert!(catalog.load_sequence_rows().unwrap().is_empty());
}

#[test]
fn new_session_does_not_repeat_open_time_catalog_migrations() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("new-session-migration.db")).unwrap();
    let catalog = engine.storage.catalog.as_ref().expect("persistent catalog");
    let legacy = r#"{"late":{"start":7,"increment":2,"current":5}}"#;
    catalog
        .set_metadata(crate::SEQUENCES_METADATA_KEY, legacy)
        .unwrap();
    let before = sqlite_data_version(&engine);

    let session = engine.new_session().unwrap();
    session.sql("SELECT 1", &[]).unwrap();

    assert_eq!(sqlite_data_version(&engine), before);
    assert_eq!(
        catalog
            .get_metadata(crate::SEQUENCES_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(legacy)
    );
    assert!(catalog.load_sequence_rows().unwrap().is_empty());
}

#[test]
fn pinned_reload_reports_a_missing_public_schema_without_repairing_it() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("missing-public.db")).unwrap();
    let catalog = engine.storage.catalog.as_ref().expect("persistent catalog");
    catalog.drop_schema("public").unwrap();
    let before = sqlite_data_version(&engine);

    let error = engine
        .begin_implicit_statement_transaction(true)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("missing required schema `public`"),
        "unexpected error: {error}"
    );
    assert_eq!(sqlite_data_version(&engine), before);
    assert!(!catalog
        .load_schemas()
        .unwrap()
        .iter()
        .any(|s| s == "public"));
}

#[test]
fn legacy_fts_repair_is_one_time_and_reload_remains_read_only() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("one-time-fts-repair.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT); \
                 INSERT INTO docs (id, body) VALUES (1, 'one-time repair'); \
                 CREATE INDEX docs_body_gin ON docs USING gin (body)",
                &[],
            )
            .unwrap();
    }
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE _posting_clusters; \
             DROP TABLE _posting_documents; \
             DROP TABLE _doc_lengths; \
             DROP TABLE _field_stats;",
        )
        .unwrap();

    let engine = Engine::open(&path).unwrap();
    let hits = engine
        .sql("SELECT id FROM docs WHERE text_match(body, 'repair')", &[])
        .unwrap();
    assert_eq!(hits.rows[0].get("id"), Some(&Value::Int(1)));

    let after_initial_repair = sqlite_data_version(&engine);
    assert_eq!(
        engine
            .epochs
            .seen_storage_change_version
            .load(std::sync::atomic::Ordering::Acquire),
        after_initial_repair,
        "initial repair was committed after the monitor baseline"
    );

    engine.begin_implicit_statement_transaction(true).unwrap();
    engine.commit().unwrap();
    assert_eq!(
        sqlite_data_version(&engine),
        after_initial_repair,
        "pinned catalog reload repeated the FTS repair"
    );

    let external = rusqlite::Connection::open(&path).unwrap();
    external
        .execute(
            "INSERT OR REPLACE INTO _metadata (key, value) VALUES ('reload_probe', '1')",
            [],
        )
        .unwrap();
    let external_commit = sqlite_data_version(&engine);
    engine.synchronize_catalog_registries().unwrap();
    assert_eq!(
        sqlite_data_version(&engine),
        external_commit,
        "external-commit refresh repeated the FTS repair"
    );
}
