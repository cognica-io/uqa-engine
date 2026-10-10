//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine transaction frames retain journal history only while their snapshots can use it.

use std::{path::Path, sync::Arc};

use crate::Engine;
use rstest::rstest;
use uqa_core::Value;

fn open(path: &Path, backend: &str) -> Engine {
    let engine = match backend {
        "native" => Engine::open(path).unwrap(),
        "kv" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        "redb" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    // The tests schedule every snapshot owner explicitly. Stop and join the automatic worker before data creation; an independently running maintenance transaction legitimately retains journal history.
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    engine
}

#[rstest]
fn transaction_completion_reclaims_history_but_savepoints_keep_the_root_snapshot(
    #[values("native", "kv", "redb")] backend: &str,
    #[values("COMMIT", "ROLLBACK", "drop")] completion: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.db");
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".uqa-row-changes");
    let size = || std::fs::metadata(&sidecar).unwrap().len();
    let root = open(&path, backend);
    root.sql(
        "CREATE TABLE t (id integer PRIMARY KEY, v integer); INSERT INTO t VALUES (1, 0)",
        &[],
    )
    .unwrap();
    for _ in 0..8 {
        root.sql("UPDATE t SET v = v + 1", &[]).unwrap();
        assert_eq!(
            size(),
            16,
            "completed {backend} writer must release its baseline"
        );
    }
    let reader = root.new_session().unwrap();
    reader.release_automatic_statistics_client();
    reader
        .sql(
            "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM t; SAVEPOINT kept",
            &[],
        )
        .unwrap();
    for _ in 0..8 {
        root.sql("UPDATE t SET v = v + 1", &[]).unwrap();
    }
    let retained = size();
    assert!(
        retained >= 16 + 8 * 48,
        "the live snapshot retains each update"
    );
    reader.sql("ROLLBACK TO kept; RELEASE kept", &[]).unwrap();
    assert_eq!(
        size(),
        retained,
        "savepoint release must preserve the root baseline"
    );
    assert_eq!(
        reader.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
        Value::Int(8)
    );
    if completion != "drop" {
        reader.sql(completion, &[]).unwrap();
        assert_eq!(size(), 16, "{completion} must release the last baseline");
    }
    drop(reader);
    assert_eq!(size(), 16, "a dropped session must release its baseline");
    drop(root);
    let reopened = open(&path, backend);
    assert_eq!(
        reopened.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
        Value::Int(16)
    );
    reopened.sql("UPDATE t SET v = v + 1", &[]).unwrap();
    assert_eq!(size(), 16);
}

#[rstest]
fn maintenance_snapshot_retains_history_until_its_own_completion(
    #[values("native", "kv", "redb")] backend: &str,
    #[values("COMMIT", "ROLLBACK", "drop")] completion: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("maintenance.db");
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".uqa-row-changes");
    let size = || std::fs::metadata(&sidecar).unwrap().len();
    let root = open(&path, backend);
    root.sql(
        "CREATE TABLE t (id integer PRIMARY KEY, v integer); INSERT INTO t VALUES (1, 0)",
        &[],
    )
    .unwrap();
    for _ in 0..8 {
        root.sql("UPDATE t SET v = v + 1", &[]).unwrap();
        assert_eq!(size(), 16);
    }
    let reader = root.new_session().unwrap();
    let maintenance = root.new_session().unwrap();
    reader.release_automatic_statistics_client();
    maintenance.release_automatic_statistics_client();
    reader
        .sql(
            "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM t",
            &[],
        )
        .unwrap();
    maintenance
        .with_storage_maintenance_scope(|worker| {
            worker.sql("SELECT * FROM t", &[]).unwrap();
            for _ in 0..8 {
                root.sql("UPDATE t SET v = v + 1", &[]).unwrap();
            }
            assert_eq!(size(), 16 + 8 * 48);
            if completion != "drop" {
                reader.sql(completion, &[]).unwrap();
            }
            drop(reader);
            assert_eq!(
                size(),
                16 + 8 * 48,
                "the maintenance baseline is still live after {completion}"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(size(), 16, "completing the last owner reclaims history");
    assert_eq!(
        root.sql("SELECT v FROM t", &[]).unwrap().rows[0]["v"],
        Value::Int(16)
    );
}
