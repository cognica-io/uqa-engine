//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The connection that writes a large commit keeps the pages the commit dirties, and returns to its ordinary cache afterwards.

use super::*;

/// The dirty pages `connection` wrote in the middle of a transaction because its cache was full.
#[allow(unsafe_code)] // rusqlite exposes database status only through its raw SQLite handle.
fn spilled(connection: &Connection) -> i32 {
    let (mut current, mut highest) = (0, 0);
    // SAFETY: the handle belongs to the live connection borrowed for the call, and both outputs are valid integers.
    let code = unsafe {
        rusqlite::ffi::sqlite3_db_status(
            connection.handle(),
            rusqlite::ffi::SQLITE_DBSTATUS_CACHE_SPILL,
            &raw mut current,
            &raw mut highest,
            0,
        )
    };
    assert_eq!(code, rusqlite::ffi::SQLITE_OK);
    current
}

fn cache_size(connection: &Connection) -> i64 {
    connection
        .query_row("PRAGMA cache_size", [], |row| row.get(0))
        .unwrap()
}

/// 4,096 records of 1 KiB each, which dirty several times the 2 MiB a connection keeps.
fn large(prefix: &str, control: &StorageReadControl) -> PreparedRecordCommit {
    let value = vec![0x5a_u8; 1024];
    let keys = (0..4096)
        .map(|record| format!("{prefix}/{record:08}").into_bytes())
        .collect::<Vec<_>>();
    let writes = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(&value),
        })
        .collect::<Vec<_>>();
    PreparedRecordCommit::new(&writes, control).unwrap()
}

#[test]
fn a_transaction_of_the_same_size_spills_an_ordinary_cache() {
    let directory = tempfile::tempdir().unwrap();
    let raw = Connection::open(directory.path().join("raw.db")).unwrap();
    raw.pragma_update(None, "journal_mode", "WAL").unwrap();
    raw.execute_batch(
        "CREATE TABLE records (key BLOB PRIMARY KEY, value BLOB) WITHOUT ROWID; BEGIN",
    )
    .unwrap();
    let value = vec![0x5a_u8; 1024];
    for record in 0..4096 {
        raw.execute(
            "INSERT INTO records VALUES (?1, ?2)",
            params![format!("large/{record:08}").into_bytes(), value],
        )
        .unwrap();
    }
    assert!(spilled(&raw) > 0);
    raw.execute_batch("COMMIT").unwrap();
}

#[test]
fn a_large_commit_keeps_its_pages_and_returns_to_the_ordinary_cache() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("cache.db")).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let status = || {
        connection
            .with(|connection| Ok((spilled(connection), cache_size(connection))))
            .unwrap()
    };
    let (before, ordinary) = status();
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(id, &large("first", &control), &control)
        .unwrap();
    // The commit wrote every page once, at its end, and the connection is back at its ordinary limit.
    assert_eq!(status(), (before, ordinary));
    let snapshot = store.snapshot(&control).unwrap();
    assert_eq!(
        snapshot
            .get(b"first/00004095", &control)
            .unwrap()
            .and_then(|record| record.value().map(|value| value.len())),
        Some(1024)
    );
    // A commit that is rejected returns to the ordinary limit as well.
    let id = store.allocate_transaction(&control).unwrap();
    assert!(matches!(
        store.commit(id, &large("first", &control), &control),
        Err(CommitFailure::Rejected(VersionError::WriteConflict { .. }))
    ));
    assert_eq!(status(), (before, ordinary));
    // A small commit raises nothing.
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(id, &prepared(b"small", b"live", &control), &control)
        .unwrap();
    assert_eq!(status(), (before, ordinary));
}
