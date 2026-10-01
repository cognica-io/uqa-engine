//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Snapshots carry the commit monitor's value, and a session keeps its snapshot while that value stands.

use uqa_storage::KeyValueStore;

use super::*;

#[test]
fn a_snapshot_carries_the_monitor_value_it_was_captured_at() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("monitor.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let first = store.snapshot(&control).unwrap();
    let captured = first.commit_monitor().unwrap();
    assert_eq!(store.commit_monitor_version().unwrap(), Some(captured));
    // A snapshot captured without a commit in between carries the same value.
    assert_eq!(
        store.snapshot(&control).unwrap().commit_monitor(),
        Some(captured)
    );
    let id = store.allocate_transaction(&control).unwrap();
    store
        .commit(id, &prepared(b"a", b"live", &control), &control)
        .unwrap();
    let committed = store.commit_monitor_version().unwrap().unwrap();
    assert_ne!(committed, captured);
    // The earlier snapshot keeps the value of its capture, which tells its holder that it is no longer the latest.
    assert_eq!(first.commit_monitor(), Some(captured));
    let second = store.snapshot(&control).unwrap();
    assert_eq!(second.commit_monitor(), Some(committed));
    assert!(second.sequence() > first.sequence());

    // An in-memory database has no connection that could watch another one commit.
    let memory = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    assert_eq!(memory.commit_monitor_version().unwrap(), None);
    assert_eq!(memory.snapshot(&control).unwrap().commit_monitor(), None);
}

#[test]
fn a_session_refresh_keeps_its_snapshot_until_another_connection_commits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("refresh.db");
    let first = crate::key_value::SQLiteKeyValueStore::open(&path).unwrap();
    let second = first.open_session().unwrap();
    let cancellation = uqa_core::CancellationToken::new();
    first.begin_transaction().unwrap();
    first.put(b"own", b"private").unwrap();
    let before = first.read_view_revision().unwrap().unwrap();
    first.refresh_transaction_snapshot(&cancellation).unwrap();
    assert!(first
        .read_view_revision()
        .unwrap()
        .unwrap()
        .same_committed_state(&before));
    second.put(b"peer", b"committed").unwrap();
    assert!(first.get(b"peer").unwrap().is_none());
    first.refresh_transaction_snapshot(&cancellation).unwrap();
    assert_eq!(first.get(b"peer").unwrap().unwrap(), b"committed");
    assert_eq!(first.get(b"own").unwrap().unwrap(), b"private");
    assert!(!first
        .read_view_revision()
        .unwrap()
        .unwrap()
        .same_committed_state(&before));
    first.commit_transaction().unwrap();
    assert_eq!(second.get(b"own").unwrap().unwrap(), b"private");
}
