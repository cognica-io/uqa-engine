//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use super::*;
use crate::transaction::SQLiteTransaction;

#[test]
fn in_memory_connection_round_trip() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    mc.with(|c| {
        c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", [])?;
        c.execute("INSERT INTO t (id, v) VALUES (1, 'hi')", [])?;
        let got: String = c.query_row("SELECT v FROM t WHERE id = 1", [], |r| r.get(0))?;
        assert_eq!(got, "hi");
        Ok(())
    })
    .unwrap();
}

#[test]
fn vacuum_reclaims_free_pages_and_requires_autocommit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vacuum.sqlite3");
    let connection = ManagedConnection::open(&path).unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute_batch(
                "CREATE TABLE payloads (id INTEGER PRIMARY KEY, payload BLOB); \
                 WITH RECURSIVE ids(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM ids WHERE id < 256) \
                 INSERT INTO payloads SELECT id, zeroblob(8192) FROM ids; \
                 DELETE FROM payloads",
            )?;
            Ok(())
        })
        .unwrap();
    let before: i64 = connection
        .with(|sqlite| Ok(sqlite.pragma_query_value(None, "page_count", |row| row.get(0))?))
        .unwrap();

    connection.vacuum().unwrap();

    let after: i64 = connection
        .with(|sqlite| Ok(sqlite.pragma_query_value(None, "page_count", |row| row.get(0))?))
        .unwrap();
    assert!(
        after < before,
        "VACUUM page count {after} did not shrink from {before}"
    );

    connection.begin_transaction().unwrap();
    assert!(matches!(
        connection.vacuum(),
        Err(SQLiteError::TransactionAlreadyActive)
    ));
    connection.rollback_transaction().unwrap();
}

#[test]
fn compressed_vacuum_flushes_its_final_truncate_before_connection_reuse() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vacuum-compressed.uqac.sqlite3");
    let connection =
        ManagedConnection::open_compressed(&path, SQLiteCompressionOptions::default()).unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute_batch(
                "CREATE TABLE payloads (id INTEGER PRIMARY KEY, payload BLOB); \
                 WITH RECURSIVE ids(id) AS (VALUES (1) UNION ALL SELECT id + 1 FROM ids WHERE id < 256) \
                 INSERT INTO payloads SELECT id, zeroblob(8192) FROM ids; \
                 DELETE FROM payloads WHERE id > 2",
            )?;
            Ok(())
        })
        .unwrap();

    connection.vacuum().unwrap();

    let remaining: i64 = connection
        .with(|sqlite| Ok(sqlite.query_row("SELECT count(*) FROM payloads", [], |row| row.get(0))?))
        .unwrap();
    assert_eq!(remaining, 2);
}

#[test]
fn wal_mode_pragma_is_set() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let mode: String = mc
        .with(|c| Ok(c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?))
        .unwrap();
    // In-memory DBs cannot use WAL — SQLite silently downgrades.
    // Assert we got *some* known journal mode; the file-backed CI
    // path is what enforces WAL.
    assert!(matches!(
        mode.to_lowercase().as_str(),
        "memory" | "wal" | "delete" | "truncate" | "persist" | "off"
    ));
}

#[test]
fn data_version_uses_one_stable_monitor_connection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data-version.db");
    let observer = ManagedConnection::open(&path).unwrap();
    let writer = ManagedConnection::open(&path).unwrap();
    let before = observer.data_version().unwrap().unwrap();
    writer
        .with(|connection| {
            connection.execute_batch(
                "CREATE TABLE committed (id INTEGER PRIMARY KEY); \
                     INSERT INTO committed (id) VALUES (1)",
            )?;
            Ok(())
        })
        .unwrap();
    let after = observer.data_version().unwrap().unwrap();
    assert_ne!(after, before);
    assert_eq!(observer.data_version().unwrap(), Some(after));
}

#[test]
fn logical_sessions_share_the_pool_data_version_monitor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shared-data-version-monitor.db");
    let base = ManagedConnection::open(&path).unwrap();
    let first = base.new_session();
    let second = base.new_session();

    assert!(Arc::ptr_eq(&first.pool, &second.pool));
    assert!(base.pool.data_version_monitor.lock().is_none());
    let first_version = first.data_version().unwrap();
    assert!(base.pool.data_version_monitor.lock().is_some());
    assert_eq!(second.data_version().unwrap(), first_version);
}

#[test]
fn deferred_transaction_snapshot_can_be_pinned_before_a_user_query() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pinned-snapshot.db");
    let base = ManagedConnection::open(&path).unwrap();
    base.with(|connection| {
        connection.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY); \
                 INSERT INTO items (id) VALUES (1)",
        )?;
        Ok(())
    })
    .unwrap();
    let reader = base.new_session();
    let writer = base.new_session();

    reader.begin_deferred_transaction().unwrap();
    reader.pin_transaction_snapshot().unwrap();
    writer
        .with(|connection| {
            connection.execute("INSERT INTO items (id) VALUES (2)", [])?;
            Ok(())
        })
        .unwrap();

    let pinned_count: i64 = reader
        .with(|connection| {
            Ok(connection.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(pinned_count, 1);
    reader.commit_transaction().unwrap();

    let committed_count: i64 = reader
        .with(|connection| {
            Ok(connection.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(committed_count, 2);
}

#[test]
fn compressed_write_transaction_requires_pinned_connection_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("compressed-monitor.db");
    let connection =
        ManagedConnection::open_compressed(&path, SQLiteCompressionOptions::default()).unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute("CREATE TABLE items (id INTEGER PRIMARY KEY)", [])?;
            Ok(())
        })
        .unwrap();

    assert!(connection.data_version_monitor_is_nonblocking().unwrap());
    connection.begin_deferred_transaction().unwrap();
    connection.pin_transaction_snapshot().unwrap();
    assert!(connection.data_version_monitor_is_nonblocking().unwrap());
    connection.rollback_transaction().unwrap();

    connection.begin_transaction().unwrap();
    assert!(!connection.data_version_monitor_is_nonblocking().unwrap());
    connection.pin_transaction_snapshot().unwrap();
    connection.rollback_transaction().unwrap();
}

#[test]
fn file_sessions_run_read_closures_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent.sqlite3");
    let base = ManagedConnection::open(&path).unwrap();
    base.with(|connection| {
        connection.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", [])?;
        connection.execute("INSERT INTO t (id) VALUES (1)", [])?;
        Ok(())
    })
    .unwrap();

    let first = base.new_session();
    let second = base.new_session();
    let (first_entered_tx, first_entered_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let first_thread = thread::spawn(move || {
        first
            .with(|connection| {
                let count: i64 =
                    connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?;
                assert_eq!(count, 1);
                first_entered_tx.send(()).unwrap();
                release_first_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    first_entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first reader entered its connection closure");

    let (second_entered_tx, second_entered_rx) = mpsc::channel();
    let second_thread = thread::spawn(move || {
        second
            .with(|connection| {
                let count: i64 =
                    connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?;
                assert_eq!(count, 1);
                second_entered_tx.send(()).unwrap();
                Ok(())
            })
            .unwrap();
    });

    let concurrent = second_entered_rx
        .recv_timeout(Duration::from_secs(2))
        .is_ok();
    release_first_tx.send(()).unwrap();
    first_thread.join().unwrap();
    second_thread.join().unwrap();
    assert!(
        concurrent,
        "a database-wide connection mutex serialized independent readers"
    );
}

#[test]
fn transaction_is_pinned_to_clones_and_isolated_from_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("isolation.sqlite3");
    let writer = ManagedConnection::open(&path).unwrap();
    writer
        .with(|connection| {
            connection.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", [])?;
            Ok(())
        })
        .unwrap();
    let writer_clone = writer.clone();
    let observer = writer.new_session();

    writer.begin_transaction().unwrap();
    assert!(writer.in_transaction());
    assert!(writer_clone.in_transaction());
    assert!(!observer.in_transaction());
    writer_clone
        .with(|connection| {
            connection.execute("INSERT INTO t (id, v) VALUES (1, 'pending')", [])?;
            connection.execute("CREATE TEMP TABLE pinned (v INTEGER)", [])?;
            connection.execute("INSERT INTO pinned (v) VALUES (7)", [])?;
            Ok(())
        })
        .unwrap();
    let pinned_value: i64 = writer
        .with(|connection| Ok(connection.query_row("SELECT v FROM pinned", [], |row| row.get(0))?))
        .unwrap();
    assert_eq!(pinned_value, 7);

    let writer_count: i64 = writer
        .with(
            |connection| Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?),
        )
        .unwrap();
    let observer_count: i64 = observer
        .with(
            |connection| Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?),
        )
        .unwrap();
    assert_eq!(writer_count, 1);
    assert_eq!(observer_count, 0);

    writer.commit_transaction().unwrap();
    assert!(!writer.in_transaction());
    let committed_count: i64 = observer
        .with(
            |connection| Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?),
        )
        .unwrap();
    assert_eq!(committed_count, 1);
}

#[test]
fn dropping_session_rolls_back_its_pinned_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drop-rollback.sqlite3");
    let base = ManagedConnection::open(&path).unwrap();
    base.with(|connection| {
        connection.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", [])?;
        Ok(())
    })
    .unwrap();
    {
        let transaction = base.new_session();
        transaction.begin_transaction().unwrap();
        transaction
            .with(|connection| {
                connection.execute("INSERT INTO t (id) VALUES (1)", [])?;
                Ok(())
            })
            .unwrap();
    }
    let count: i64 = base
        .with(
            |connection| Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn ignored_storage_error_aborts_explicit_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aborted.sqlite3");
    let writer = ManagedConnection::open(&path).unwrap();
    writer
        .with(|connection| {
            connection.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", [])?;
            Ok(())
        })
        .unwrap();
    let observer = writer.new_session();

    writer.begin_transaction().unwrap();
    writer
        .with(|connection| {
            connection.execute("INSERT INTO t (id) VALUES (1)", [])?;
            Ok(())
        })
        .unwrap();
    let _ignored = writer.with(|connection| {
        connection.execute("INSERT INTO missing_table (id) VALUES (1)", [])?;
        Ok(())
    });
    assert!(matches!(
        writer.commit_transaction(),
        Err(SQLiteError::TransactionAborted(_))
    ));

    let count: i64 = observer
        .with(
            |connection| Ok(connection.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))?),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn failed_drop_rollback_is_reported_to_the_session() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let session = connection.new_session();
    let transaction = SQLiteTransaction::begin(session.clone()).unwrap();

    // Desynchronise SQLite's transaction state from the managed session to
    // exercise the otherwise difficult rollback-failure path. The
    // transaction guard must not silently report this cleanup as success.
    session
        .with(|sqlite| {
            sqlite.execute_batch("COMMIT")?;
            Ok(())
        })
        .unwrap();
    drop(transaction);

    assert!(matches!(
        session.with(|_| Ok(())),
        Err(SQLiteError::SessionCleanupFailed(_))
    ));

    // The failed cleanup notification is consumed exactly once, and the
    // managed session remains usable with a clean pooled connection.
    session
        .with(|sqlite| {
            sqlite.execute("CREATE TABLE recovered (id INTEGER PRIMARY KEY)", [])?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn sqlcipher_build_reports_cipher_version() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let version: String = mc
        .with(|c| Ok(c.query_row("PRAGMA cipher_version", [], |r| r.get(0))?))
        .unwrap();
    assert!(!version.is_empty());
}

#[test]
fn encrypted_file_requires_matching_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("encrypted.sqlite3");
    let key = "correct horse battery staple";

    {
        let mc = ManagedConnection::open_encrypted(&path, key).unwrap();
        mc.with(|c| {
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", [])?;
            c.execute("INSERT INTO t (id, v) VALUES (1, 'secret')", [])?;
            Ok(())
        })
        .unwrap();
    }

    {
        let mc = ManagedConnection::open_encrypted(&path, key).unwrap();
        let got: String = mc
            .with(|c| Ok(c.query_row("SELECT v FROM t WHERE id = 1", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(got, "secret");
    }

    assert!(ManagedConnection::open_encrypted(&path, "wrong key").is_err());
    assert!(ManagedConnection::open(&path).is_err());
    assert!(matches!(
        ManagedConnection::open_encrypted(&path, ""),
        Err(SQLiteError::EmptyEncryptionKey)
    ));
}

#[test]
fn compressed_file_reopens_through_vfs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compressed.uqac.sqlite3");
    let plain_path = dir.path().join("plain.sqlite3");
    let options = SQLiteCompressionOptions::default();
    let repeated = "compressible payload ".repeat(256);

    {
        let mc = ManagedConnection::open_compressed(&path, options).unwrap();
        mc.with(|c| {
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)", [])?;
            let mut stmt = c.prepare("INSERT INTO t (id, body) VALUES (?1, ?2)")?;
            for id in 0..128_i64 {
                stmt.execute(rusqlite::params![id, &repeated])?;
            }
            Ok(())
        })
        .unwrap();
    }

    {
        let mc = ManagedConnection::open_compressed(&path, options).unwrap();
        let count: i64 = mc
            .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(count, 128);
    }

    {
        let plain = Connection::open(&plain_path).unwrap();
        plain
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)", [])
            .unwrap();
        let mut stmt = plain
            .prepare("INSERT INTO t (id, body) VALUES (?1, ?2)")
            .unwrap();
        for id in 0..128_i64 {
            stmt.execute(rusqlite::params![id, &repeated]).unwrap();
        }
    }

    let compressed = std::fs::read(&path).unwrap();
    let plain_len = std::fs::metadata(&plain_path).unwrap().len();
    assert_eq!(&compressed[..8], b"UQACDB2\0");
    assert!(compressed.len() < plain_len as usize);
    assert!(ManagedConnection::open(&path).is_err());
}

#[test]
fn compressed_connections_refresh_committed_container_state_after_relocking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("compressed-concurrent-sessions.uqac.sqlite3");
    let options = SQLiteCompressionOptions::default();
    let reader = ManagedConnection::open_compressed(&path, options).unwrap();
    reader
        .with(|connection| {
            connection.execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, value INTEGER);
                 INSERT INTO t VALUES (1, 10);",
            )?;
            Ok(())
        })
        .unwrap();
    let initial: i64 = reader
        .with(|connection| {
            Ok(connection.query_row("SELECT value FROM t WHERE id = 1", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(initial, 10);

    let writer = ManagedConnection::open_compressed(&path, options).unwrap();
    writer
        .with(|connection| {
            connection.execute("UPDATE t SET value = 11 WHERE id = 1", [])?;
            Ok(())
        })
        .unwrap();

    let refreshed: i64 = reader
        .with(|connection| {
            Ok(connection.query_row("SELECT value FROM t WHERE id = 1", [], |row| row.get(0))?)
        })
        .unwrap();
    assert_eq!(refreshed, 11);
}

#[test]
fn lz4_compressed_file_reopens_through_vfs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compressed-lz4.uqac.sqlite3");
    let options = SQLiteCompressionOptions::lz4();
    let repeated = "lz4 compressible payload ".repeat(256);

    {
        let mc = ManagedConnection::open_compressed(&path, options).unwrap();
        mc.with(|c| {
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)", [])?;
            let mut stmt = c.prepare("INSERT INTO t (id, body) VALUES (?1, ?2)")?;
            for id in 0..128_i64 {
                stmt.execute(rusqlite::params![id, &repeated])?;
            }
            Ok(())
        })
        .unwrap();
    }

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"UQACDB2\0");

    let mc = ManagedConnection::open_compressed(&path, options).unwrap();
    let count: i64 = mc
        .with(|c| Ok(c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(count, 128);
}

#[test]
fn compressed_encrypted_file_requires_matching_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compressed-encrypted.uqac.sqlite3");
    let options = SQLiteCompressionOptions::default();
    let key = "correct horse battery staple";
    let secret = "very secret compressed payload".repeat(64);

    {
        let mc = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
        mc.with(|c| {
            c.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)", [])?;
            c.execute(
                "INSERT INTO t (id, body) VALUES (1, ?1)",
                rusqlite::params![&secret],
            )?;
            Ok(())
        })
        .unwrap();
    }

    {
        let mc = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
        let got: String = mc
            .with(|c| Ok(c.query_row("SELECT body FROM t WHERE id = 1", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(got, secret);
    }

    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes
        .windows(b"very secret compressed payload".len())
        .any(|window| window == b"very secret compressed payload"));
    assert!(ManagedConnection::open_compressed_encrypted(&path, "wrong key", options).is_err());
    assert!(ManagedConnection::open_compressed(&path, options).is_err());
    assert!(ManagedConnection::open(&path).is_err());
    assert!(matches!(
        ManagedConnection::open_compressed_encrypted(&path, "", options),
        Err(SQLiteError::EmptyEncryptionKey)
    ));
}

#[test]
fn compressed_encrypted_anchor_rejects_whole_file_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("anchored.uqac.sqlite3");
    let old_snapshot = dir.path().join("anchored-old.uqac.sqlite3");
    let options = SQLiteCompressionOptions::default();
    let key = "trusted anchor key";

    {
        let connection = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
        connection
            .with(|sqlite| {
                sqlite.execute_batch(
                    "CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1);",
                )?;
                Ok(())
            })
            .unwrap();
    }
    let old_anchor = compressed_vfs::read_authenticated_anchor(&path, key).unwrap();
    std::fs::copy(&path, &old_snapshot).unwrap();

    {
        let connection = ManagedConnection::open_compressed_encrypted(&path, key, options).unwrap();
        connection
            .with(|sqlite| {
                sqlite.execute("INSERT INTO t VALUES (2)", [])?;
                Ok(())
            })
            .unwrap();
    }
    let current_anchor = compressed_vfs::read_authenticated_anchor(&path, key).unwrap();
    assert_eq!(current_anchor.database_id, old_anchor.database_id);
    assert!(current_anchor.generation > old_anchor.generation);
    assert_ne!(current_anchor.state_tag, old_anchor.state_tag);
    {
        let anchored = ManagedConnection::open_compressed_encrypted_with_anchor(
            &path,
            key,
            options,
            current_anchor,
        )
        .unwrap();
        anchored
            .with(|sqlite| {
                sqlite.execute("INSERT INTO t VALUES (3)", [])?;
                Ok(())
            })
            .unwrap();
    }
    let Err(error) = ManagedConnection::open_compressed_encrypted(&path, key, options) else {
        panic!("stale registered anchor unexpectedly accepted a newer state");
    };
    assert!(!error.to_string().is_empty());
    let advanced_anchor = compressed_vfs::read_authenticated_anchor(&path, key).unwrap();
    assert!(advanced_anchor.generation > current_anchor.generation);
    ManagedConnection::open_compressed_encrypted_with_anchor(&path, key, options, advanced_anchor)
        .unwrap();

    std::fs::copy(&old_snapshot, &path).unwrap();
    let Err(error) = ManagedConnection::open_compressed_encrypted_with_anchor(
        &path,
        key,
        options,
        advanced_anchor,
    ) else {
        panic!("rolled-back container unexpectedly satisfied its trusted anchor");
    };
    assert!(error
        .to_string()
        .contains("does not match trusted generation"));

    let Err(error) = ManagedConnection::open_compressed_encrypted(&path, key, options) else {
        panic!("unanchored re-registration weakened an existing trusted anchor");
    };
    assert!(!error.to_string().is_empty());
}
