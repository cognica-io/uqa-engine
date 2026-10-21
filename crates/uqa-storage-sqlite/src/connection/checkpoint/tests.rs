//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ManagedConnection;
use std::sync::atomic::Ordering;

#[test]
fn wal_commits_finish_while_maintenance_is_paused_and_shutdown_joins_it() {
    for encrypted in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("checkpoint.db");
        let connection = if encrypted {
            ManagedConnection::open_encrypted(&path, "checkpoint-test-key").unwrap()
        } else {
            ManagedConnection::open(&path).unwrap()
        };
        let signal = Arc::clone(&connection.pool.checkpoint.as_ref().unwrap().signal);
        let paused = signal.work_gate.lock();
        connection.with_physical(|sqlite| {
            sqlite.pragma_update(None, "synchronous", "FULL")?;
            sqlite.execute_batch("CREATE TABLE payload(value BLOB); INSERT INTO payload VALUES(zeroblob(8388608));")?;
            assert_eq!(sqlite.pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))?, 2);
            Ok(())
        }).unwrap();
        assert_eq!(signal.completed.load(Ordering::Acquire), 0);
        // The committed payload is visible even though no maintenance can run.
        assert_eq!(
            connection
                .new_session()
                .with_physical(|sqlite| {
                    Ok(
                        sqlite.query_row("SELECT length(value) FROM payload", [], |row| {
                            row.get::<_, i64>(0)
                        })?,
                    )
                })
                .unwrap(),
            8 << 20
        );
        drop(paused);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while signal.completed.load(Ordering::Acquire) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "maintenance did not finish"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(connection);
        assert!(signal.request.lock().stopping);
        let reopened = if encrypted {
            ManagedConnection::open_encrypted(&path, "checkpoint-test-key").unwrap()
        } else {
            ManagedConnection::open(&path).unwrap()
        };
        reopened
            .with_physical(|sqlite| {
                assert_eq!(
                    sqlite.query_row("SELECT length(value) FROM payload", [], |row| row
                        .get::<_, i64>(0))?,
                    8 << 20
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn passive_maintenance_retries_after_a_physical_reader_releases_its_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader.db");
    let connection = ManagedConnection::open(&path).unwrap();
    connection
        .with_physical(|sqlite| {
            sqlite.execute_batch(
                "CREATE TABLE payload(value BLOB); INSERT INTO payload VALUES(zeroblob(1));",
            )?;
            Ok(())
        })
        .unwrap();
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM payload;")
        .unwrap();
    connection
        .with_physical(|sqlite| {
            sqlite.execute_batch("UPDATE payload SET value=zeroblob(8388608)")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        reader
            .query_row("SELECT length(value) FROM payload", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    reader.execute_batch("ROLLBACK").unwrap();
    let signal = &connection.pool.checkpoint.as_ref().unwrap().signal;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while signal.completed.load(Ordering::Acquire) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "maintenance did not resume"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
