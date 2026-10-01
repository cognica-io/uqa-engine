//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Record cursors perform a constant number of key-only statements and admit each key before stepping.

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use super::*;

#[test]
fn ordered_point_metadata_uses_constant_statements_without_a_statement_cache() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let keys = (0_u64..1024)
        .map(|id| [b"ordered/".as_slice(), &id.to_be_bytes()].concat())
        .collect::<Vec<_>>();
    let writes = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(b"value"),
        })
        .collect::<Vec<_>>();
    let commit = PreparedRecordCommit::new(&writes, &control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    store.commit(id, &commit, &control).unwrap();
    let snapshot = store.snapshot(&control).unwrap();
    let statements = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&statements);
    store
        .with(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                if matches!(context.action, AuthAction::Select) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let mut visited = 0;
    snapshot
        .visit_keys(
            b"ordered/",
            None,
            usize::MAX,
            &control,
            &mut |key, record| {
                assert_eq!(key, keys[visited]);
                assert!(record.live);
                visited += 1;
                Ok(true)
            },
        )
        .unwrap();
    assert_eq!(visited, keys.len());
    assert!(
        statements.load(Ordering::Relaxed) <= 16,
        "ordered metadata must not prepare statements per key"
    );
    store
        .with(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn ordered_point_values_use_constant_statements_without_a_statement_cache() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let keys = (0_u64..1024)
        .map(|id| [b"ordered/".as_slice(), &id.to_be_bytes()].concat())
        .collect::<Vec<_>>();
    let writes = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(b"value"),
        })
        .collect::<Vec<_>>();
    let commit = PreparedRecordCommit::new(&writes, &control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    store.commit(id, &commit, &control).unwrap();
    let snapshot = store.snapshot(&control).unwrap();
    let retained = control.memory().used();
    let statements = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&statements);
    store
        .with(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: AuthContext<'_>| {
                if matches!(context.action, AuthAction::Select) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let mut visited = 0;
    snapshot
        .visit_prefix(
            b"ordered/",
            None,
            usize::MAX,
            &control,
            &mut |key, record| {
                assert_eq!(key, keys[visited]);
                assert_eq!(record.value, Some(b"value".as_slice()));
                visited += 1;
                Ok(true)
            },
        )
        .unwrap();
    store
        .with(|sqlite| {
            sqlite.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(visited, keys.len());
    assert!(
        statements.load(Ordering::Relaxed) <= 20,
        "ordered values must not prepare statements per key"
    );
    assert_eq!(control.memory().used(), retained);
}

#[test]
fn ordered_values_stop_before_later_oversize_payloads_and_release_the_last_row() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let storage = control();
    let large = vec![7; 1 << 20];
    let writes = [
        RecordWrite {
            key: b"ordered/a",
            expected: None,
            value: Some(b"small"),
        },
        RecordWrite {
            key: b"ordered/z",
            expected: None,
            value: Some(&large),
        },
    ];
    let commit = PreparedRecordCommit::new(&writes, &storage).unwrap();
    let id = store.allocate_transaction(&storage).unwrap();
    store.commit(id, &commit, &storage).unwrap();
    let snapshot = store.snapshot(&storage).unwrap();
    let read = StorageReadControl::with_limit(1024);
    let mut visited = 0;
    snapshot
        .visit_prefix(b"ordered/", None, usize::MAX, &read, &mut |key, record| {
            assert_eq!(key, b"ordered/a");
            assert_eq!(record.value, Some(b"small".as_slice()));
            visited += 1;
            Ok(false)
        })
        .unwrap();
    assert_eq!(visited, 1);
    assert_eq!(read.memory().used(), 0);
    assert!(read.memory().peak() < large.len());
    let error = snapshot
        .visit_prefix(b"ordered/", Some(b"ordered/a"), 1, &read, &mut |_, _| {
            panic!("rejected payload must not visit")
        })
        .unwrap_err()
        .into_storage_error();
    assert!(matches!(error, uqa_storage::StorageBackendError::Memory(_)));
    assert_eq!(read.memory().used(), 0);
    let error = snapshot
        .visit_prefix(b"ordered/", None, usize::MAX, &read, &mut |_, _| {
            read.cancellation().cancel();
            Ok(false)
        })
        .unwrap_err()
        .into_storage_error();
    assert!(matches!(
        error,
        uqa_storage::StorageBackendError::Cancelled(_)
    ));
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn ordered_cursor_stops_before_oversize_keys_and_never_hydrates_metadata_values() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let storage = control();
    let large = vec![7; 1 << 20];
    let large_key = [b"ordered/z".as_slice(), large.as_slice()].concat();
    let writes = [
        RecordWrite {
            key: b"ordered/a",
            expected: None,
            value: Some(&large),
        },
        RecordWrite {
            key: &large_key,
            expected: None,
            value: Some(b"small"),
        },
    ];
    let commit = PreparedRecordCommit::new(&writes, &storage).unwrap();
    let id = store.allocate_transaction(&storage).unwrap();
    store.commit(id, &commit, &storage).unwrap();
    let snapshot = store.snapshot(&storage).unwrap();
    let read = StorageReadControl::with_limit(1024);
    let mut visited = 0;
    snapshot
        .visit_keys(b"ordered/", None, usize::MAX, &read, &mut |key, record| {
            assert_eq!(key, b"ordered/a");
            assert!(record.live);
            visited += 1;
            Ok(false)
        })
        .unwrap();
    assert_eq!(visited, 1);
    assert!(read.memory().peak() < large.len());
    assert_eq!(read.memory().used(), 0);
    let error = snapshot
        .visit_keys(b"ordered/", Some(b"ordered/a"), 1, &read, &mut |_, _| {
            panic!("rejected key must not visit")
        })
        .unwrap_err()
        .into_storage_error();
    assert!(matches!(error, uqa_storage::StorageBackendError::Memory(_)));
    assert_eq!(read.memory().used(), 0);
    assert!(read.memory().peak() < large.len());
    let error = snapshot
        .visit_keys(b"ordered/", None, usize::MAX, &read, &mut |_, _| {
            read.cancellation().cancel();
            Ok(false)
        })
        .unwrap_err()
        .into_storage_error();
    assert!(matches!(
        error,
        uqa_storage::StorageBackendError::Cancelled(_)
    ));
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn stopping_at_a_point_does_not_admit_a_later_compacted_run_key() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let storage = control();
    let mut keys = vec![b"ordered/a".to_vec()];
    let run_prefix = [b"ordered/z".as_slice(), &[b'x'; 512]].concat();
    keys.extend((0_u64..130).map(|id| [run_prefix.as_slice(), &id.to_be_bytes()].concat()));
    let writes = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(b"value"),
        })
        .collect::<Vec<_>>();
    let commit = PreparedRecordCommit::new(&writes, &storage).unwrap();
    let id = store.allocate_transaction(&storage).unwrap();
    store.commit(id, &commit, &storage).unwrap();
    store.reclaim_versions(&storage).unwrap();
    store
        .with(|sqlite| {
            assert!(
                sqlite.query_row("SELECT count(*) FROM _uqa_mvcc_runs", [], |row| {
                    row.get::<_, i64>(0)
                })? > 0
            );
            assert!(sqlite.query_row(
                "SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_heads WHERE key = ?1)",
                [b"ordered/a".as_slice()],
                |row| row.get::<_, bool>(0),
            )?);
            Ok(())
        })
        .unwrap();
    let snapshot = store.snapshot(&storage).unwrap();
    for (limit, more) in [(1, true), (usize::MAX, false)] {
        let read = StorageReadControl::with_limit(256);
        let mut visited = 0;
        snapshot
            .visit_keys(b"ordered/", None, limit, &read, &mut |key, record| {
                assert_eq!(key, b"ordered/a");
                assert!(record.live);
                visited += 1;
                Ok(more)
            })
            .unwrap();
        assert_eq!(visited, 1);
        assert_eq!(read.memory().used(), 0);
        let error = snapshot
            .visit_keys(b"ordered/", Some(b"ordered/a"), 1, &read, &mut |_, _| {
                panic!("an oversized selected run key must be rejected before its visitor")
            })
            .unwrap_err()
            .into_storage_error();
        assert!(matches!(error, uqa_storage::StorageBackendError::Memory(_)));
        assert_eq!(read.memory().used(), 0);
    }
}
