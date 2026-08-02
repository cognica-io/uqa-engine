//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{mpsc, Arc, Barrier};
use std::time::Duration;

use uqa_core::Value;

use super::Engine;

fn sqlite_data_version(engine: &Engine) -> u64 {
    engine
        .storage
        .sqlite_session
        .as_ref()
        .expect("persistent test engine")
        .data_version()
        .expect("read SQLite data version")
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
            "DROP TABLE _postings; \
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
            .seen_sqlite_data_version
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
