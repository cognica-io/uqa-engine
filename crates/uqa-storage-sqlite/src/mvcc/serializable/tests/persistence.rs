//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unchanged admissions release coordination without updating the checkpoint BLOB.

use super::*;

#[test]
fn empty_initialization_is_retained_but_repeated_observations_do_not_write() {
    for mode in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("checkpoint.db");
        let connection = if mode == 4 {
            ManagedConnection::open_in_memory().unwrap()
        } else {
            open(&path, mode)
        };
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let control = control();
        let held = store.serializable_admission(&control).unwrap();
        let coordinator = held.graph().coordinator();
        held.persist(&control).unwrap();
        let mut held = store.serializable_admission(&control).unwrap();
        assert_eq!(held.graph().coordinator(), coordinator);
        assert!(!held.graph().checkpoint_changed());
        let actor = held.graph_mut().admit(true, &control).unwrap();
        held.graph_mut()
            .observe_read(actor, point(b"absent"), &control)
            .unwrap();
        held.persist(&control).unwrap();
        for _ in 0..3 {
            let mut held = store.serializable_admission(&control).unwrap();
            let before = held.connection.total_changes();
            held.graph_mut()
                .observe_read(actor, point(b"absent"), &control)
                .unwrap();
            held.graph_mut().reclaim();
            assert_eq!(
                held.graph().safe_snapshot(actor, &control).unwrap(),
                uqa_storage::mvcc::SafeSnapshot::Safe
            );
            held.persist_in(&control).unwrap();
            assert!(held.connection.is_autocommit());
            assert_eq!(held.connection.total_changes(), before);
        }
        let mut held = store.serializable_admission(&control).unwrap();
        let before = held.connection.total_changes();
        held.graph_mut()
            .observe_read(actor, point(b"new"), &control)
            .unwrap();
        held.persist_in(&control).unwrap();
        assert_eq!(held.connection.total_changes(), before + 2);
        drop(held);
        let mut held = store.serializable_admission(&control).unwrap();
        let writer = held.graph_mut().admit(false, &control).unwrap();
        held.graph_mut()
            .observe_write(writer, point(b"new"), &control)
            .unwrap();
        held.persist(&control).unwrap();
    }
}

#[test]
fn cancelled_unchanged_persistence_still_releases_admission() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let id = admit_read(&store, b"retained");
    let control = control();
    let held = store.serializable_admission(&control).unwrap();
    assert!(!held.graph().checkpoint_changed());
    control.cancellation().cancel();
    assert!(matches!(
        held.persist(&control),
        Err(VersionError::Cancelled(_))
    ));
    control.cancellation().reset();
    store
        .serializable_admission(&control)
        .unwrap()
        .graph()
        .check_active(id)
        .unwrap();
}
