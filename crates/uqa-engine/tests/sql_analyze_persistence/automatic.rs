//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic maintenance must work without any explicit ANALYZE/statistics API.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{exec, tempdir, Catalog, Engine, ManagedConnection, Value};

fn stored_rows(path: &Path, table: &str) -> Option<i64> {
    let catalog = Catalog::open(ManagedConnection::open(path).unwrap()).unwrap();
    catalog
        .load_column_stats(table)
        .unwrap()
        .first()
        .map(|stats| stats.row_count)
}

fn wait_for_rows(engine: &Engine, path: &Path, table: &str, expected: i64) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if stored_rows(path, table) == Some(expected)
            && !engine.automatic_statistics_status().running
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "automatic statistics did not reach {expected}: {:?}",
            engine.automatic_statistics_status()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn committed_writes_automatically_create_and_refresh_persistent_statistics() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("automatic.sqlite3");
    let engine = Engine::open(&path).unwrap();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, category TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO t SELECT n, 'first' FROM generate_series(1, 100) AS g(n)",
    );
    wait_for_rows(&engine, &path, "public.t", 100);
    let before = engine.automatic_statistics_status().completed;
    exec(&engine, "INSERT INTO t VALUES (101, 'second')");
    assert_eq!(
        stored_rows(&path, "public.t"),
        Some(100),
        "writes must retain the last estimate"
    );
    exec(
        &engine,
        "INSERT INTO t SELECT n, 'second' FROM generate_series(102, 180) AS g(n)",
    );
    wait_for_rows(&engine, &path, "public.t", 180);
    assert!(engine.automatic_statistics_status().completed > before);
    assert_eq!(
        engine.sql("SELECT count(*) AS n FROM t", &[]).unwrap().rows[0]["n"],
        Value::Int(180)
    );
}

#[test]
fn automatic_statistics_never_hydrate_opaque_payloads() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("automatic-payload.sqlite3");
    let engine = Engine::open(&path).unwrap();
    exec(
        &engine,
        "CREATE TABLE assets (id INTEGER PRIMARY KEY, kind TEXT, bytes BYTEA)",
    );
    engine
        .sql(
            "INSERT INTO assets VALUES (1, 'image', $1)",
            &[uqa_engine::SQLParam::scalar(Value::Bytes(vec![
                9;
                8 * 1024
                    * 1024
            ]))],
        )
        .unwrap();
    rusqlite::Connection::open(&path).unwrap().execute(
        "DELETE FROM _document_blobs WHERE table_name = 'public.assets' AND field_name = 'bytes'", []
    ).unwrap();
    wait_for_rows(&engine, &path, "public.assets", 1);
    assert!(engine.automatic_statistics_status().last_error.is_none());
    let catalog = Catalog::open(ManagedConnection::open(&path).unwrap()).unwrap();
    let stats = catalog.load_column_stats("public.assets").unwrap();
    assert!(stats.iter().any(|stats| stats.column_name == "kind"));
    assert!(!stats.iter().any(|stats| stats.column_name == "bytes"));
    assert!(engine.sql("SELECT bytes FROM assets", &[]).is_err());
}

#[test]
fn automatic_maintenance_counts_follow_commit_savepoint_and_rollback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("automatic-rollback.sqlite3");
    let engine = Engine::open(&path).unwrap();
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY)");
    exec(&engine, "INSERT INTO t VALUES (1)");
    wait_for_rows(&engine, &path, "public.t", 1);
    exec(&engine, "BEGIN; INSERT INTO t VALUES (2); SAVEPOINT keep_one; INSERT INTO t VALUES (3); ROLLBACK TO keep_one; COMMIT");
    let catalog = Catalog::open(ManagedConnection::open(&path).unwrap()).unwrap();
    let json = catalog
        .get_metadata("uqa.statistics.maintenance.v1:public.t")
        .unwrap()
        .unwrap();
    let state: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(state["changes"], 1);
    exec(&engine, "BEGIN; INSERT INTO t VALUES (4); ROLLBACK");
    assert_eq!(
        catalog
            .get_metadata("uqa.statistics.maintenance.v1:public.t")
            .unwrap()
            .unwrap(),
        json
    );
    assert_eq!(stored_rows(&path, "public.t"), Some(1));
}

#[test]
fn failed_automatic_refresh_retains_pending_work_and_recovers_after_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("automatic-retry.sqlite3");
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY)");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TRIGGER fail_statistics BEFORE INSERT ON _column_stats BEGIN SELECT RAISE(ABORT, 'injected maintenance failure'); END;").unwrap();
        exec(&engine, "INSERT INTO t VALUES (1), (2), (3)");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if engine.automatic_statistics_status().last_error.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "injected failure was not reported"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(stored_rows(&path, "public.t"), None);
        assert_eq!(
            engine.sql("SELECT count(*) AS n FROM t", &[]).unwrap().rows[0]["n"],
            Value::Int(3)
        );
    }
    // Drop must release the maintenance session before immediate reopen.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TRIGGER fail_statistics")
        .unwrap();
    let reopened = Engine::open(&path).unwrap();
    wait_for_rows(&reopened, &path, "public.t", 3);
    assert!(reopened.automatic_statistics_status().last_error.is_none());
}

#[test]
fn sampled_automatic_statistics_keep_the_full_table_row_count() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("automatic-sampling.sqlite3");
    let engine = Engine::open(&path).unwrap();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, category TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO t SELECT n, 'same' FROM generate_series(1, 5000) AS g(n)",
    );
    wait_for_rows(&engine, &path, "public.t", 5000);
    let catalog = Catalog::open(ManagedConnection::open(&path).unwrap()).unwrap();
    let stats = catalog.load_column_stats("public.t").unwrap();
    assert_eq!(
        stats
            .iter()
            .find(|stats| stats.column_name == "id")
            .unwrap()
            .distinct_count,
        5000
    );
    assert_eq!(
        stats
            .iter()
            .find(|stats| stats.column_name == "category")
            .unwrap()
            .distinct_count,
        1
    );
}

#[test]
fn old_small_pending_changes_refresh_automatically_after_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("automatic-dirty-age.sqlite3");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1)",
        );
        wait_for_rows(&engine, &path, "public.t", 1);
        exec(&engine, "INSERT INTO t VALUES (2)");
        assert_eq!(stored_rows(&path, "public.t"), Some(1));
    }
    // Model a restart more than 60 seconds after the committed small write.
    // This exercises durable timer recovery without a minute-long test sleep.
    let catalog = Catalog::open(ManagedConnection::open(&path).unwrap()).unwrap();
    let key = "uqa.statistics.maintenance.v1:public.t";
    let mut pending: serde_json::Value =
        serde_json::from_str(&catalog.get_metadata(key).unwrap().unwrap()).unwrap();
    assert_eq!(pending["changes"], 1);
    pending["dirty_since_ms"] = serde_json::json!(0);
    catalog.set_metadata(key, &pending.to_string()).unwrap();
    let reopened = Engine::open(&path).unwrap();
    wait_for_rows(&reopened, &path, "public.t", 2);
}

#[test]
fn redb_automatic_statistics_persist_and_release_the_file_on_drop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("automatic.redb");
    {
        let provider: std::sync::Arc<dyn uqa_storage::PersistentStorageProvider> =
            std::sync::Arc::new(uqa_storage_redb::RedbStorage::open(&path).unwrap());
        let engine = Engine::from_persistent_provider(std::sync::Arc::clone(&provider)).unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1), (2)",
        );
        let reader = provider.open_session().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if reader
                .catalog
                .load_column_stats("public.t")
                .unwrap()
                .first()
                .is_some_and(|stats| stats.row_count == 2)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "redb automatic refresh failed: {:?}",
                engine.automatic_statistics_status()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let reopened = uqa_storage_redb::RedbStorage::open(&path).unwrap();
    let provider: std::sync::Arc<dyn uqa_storage::PersistentStorageProvider> =
        std::sync::Arc::new(reopened);
    assert_eq!(
        provider
            .open_session()
            .unwrap()
            .catalog
            .load_column_stats("public.t")
            .unwrap()[0]
            .row_count,
        2
    );
}

#[test]
fn compressed_automatic_statistics_use_separate_read_and_write_transactions() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("automatic-compressed.sqlite3");
    let engine = Engine::open_compressed(
        &path,
        uqa_storage_sqlite::SQLiteCompressionOptions::default(),
    )
    .unwrap();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1), (2)",
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let status = engine.automatic_statistics_status();
        if status.completed != 0 && !status.running {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "compressed automatic refresh failed: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(engine);
    let reopened = Engine::open_compressed(
        &path,
        uqa_storage_sqlite::SQLiteCompressionOptions::default(),
    )
    .unwrap();
    assert_eq!(reopened.column_stats("t").unwrap()["id"].row_count, 2);
}
