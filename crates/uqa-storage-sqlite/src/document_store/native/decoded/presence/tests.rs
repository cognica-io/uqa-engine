//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{Catalog, ManagedConnection, SQLiteDocumentStore};
use uqa_storage::{
    document_store::Document, mvcc::VersionedSessionOptions, DocumentStore, StorageBackendError,
};

fn fixture(count: u64) -> SQLiteDocumentStore {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut store = SQLiteDocumentStore::new(connection, "docs");
    store.conn.begin_transaction().unwrap();
    for id in 1..=count {
        store.put(id, Document::new()).unwrap();
    }
    store.conn.commit_transaction().unwrap();
    store
}

#[test]
fn dense_document_presence_uses_bounded_selects_and_releases_reservations() {
    use rusqlite::hooks::{AuthAction, Authorization};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let store = fixture(1024);
    let snapshot = store.snapshot().unwrap();
    let control = store.conn.retention_control().unwrap();
    let baseline = control.memory().used();
    let selected = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&selected);
    store
        .conn
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(context.action, AuthAction::Select) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    let ids = (0..=1025).collect::<Vec<_>>();
    let mut seen = Vec::new();
    snapshot
        .for_each_fields_multi_ref_with_presence(&ids, &[], &mut |id, present, fields| {
            assert_eq!(present, (1..=1024).contains(&id));
            assert!(fields.is_empty());
            seen.push(id);
            true
        })
        .unwrap();
    assert_eq!(seen, ids);
    assert!(
        selected.load(Ordering::Relaxed) < 128,
        "{} SELECTs",
        selected.load(Ordering::Relaxed)
    );
    assert_eq!(control.memory().used(), baseline);
    store
        .conn
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(16);
            Ok(())
        })
        .unwrap();
}

#[test]
fn dense_document_presence_keeps_private_and_committed_snapshots_through_reentry() {
    let mut store = fixture(300);
    let old = store.snapshot().unwrap();
    store.conn.begin_transaction().unwrap();
    for id in (2..=300).step_by(2) {
        store.delete(id).unwrap();
    }
    let private = store.snapshot().unwrap();
    let ids = (0..=301).collect::<Vec<_>>();
    let mut seen = Vec::new();
    private
        .for_each_fields_multi_ref_with_presence(&ids, &[], &mut |id, present, _| {
            if id == 0 {
                store.delete(299).unwrap();
                store.put(301, Document::new()).unwrap();
            }
            seen.push((id, present));
            true
        })
        .unwrap();
    assert_eq!(
        seen,
        ids.iter()
            .map(|id| (*id, *id > 0 && *id <= 300 && id % 2 == 1))
            .collect::<Vec<_>>()
    );
    store.conn.rollback_transaction().unwrap();
    for snapshot in [old, store.snapshot().unwrap()] {
        snapshot
            .for_each_fields_multi_ref_with_presence(&ids, &[], &mut |id, present, _| {
                assert_eq!(present, (1..=300).contains(&id));
                true
            })
            .unwrap();
    }
}

#[test]
fn sparse_and_duplicate_presence_keeps_order_and_stops_before_later_invalid_ids() {
    let store = fixture(16);
    let snapshot = store.snapshot().unwrap();
    for ids in [
        vec![16, 1, 16, 17, 1, 16, 0, 1],
        vec![0, 1, 100, 200, 300, 400, 500, 600],
    ] {
        let mut seen = Vec::new();
        snapshot
            .for_each_fields_multi_ref_with_presence(&ids, &[], &mut |id, present, _| {
                seen.push((id, present));
                true
            })
            .unwrap();
        assert_eq!(
            seen,
            ids.iter()
                .map(|id| (*id, (1..=16).contains(id)))
                .collect::<Vec<_>>()
        );
    }
    let mut calls = 0;
    snapshot
        .for_each_fields_multi_ref_with_presence(&[1, u64::MAX], &[], &mut |id, present, _| {
            assert_eq!(id, 1);
            assert!(present);
            calls += 1;
            false
        })
        .unwrap();
    assert_eq!(calls, 1);
}

#[test]
fn dense_presence_cancellation_and_quota_leave_no_temporary_buffers() {
    let store = fixture(16);
    let snapshot = store.snapshot().unwrap();
    let ids = (1..=16).collect::<Vec<_>>();
    let control = store.conn.retention_control().unwrap();
    let baseline = control.memory().used();
    let hold = control
        .memory()
        .reserve(control.memory().limit() - baseline)
        .unwrap();
    assert!(matches!(
        snapshot.for_each_fields_multi_ref_with_presence(&ids, &[], &mut |_, _, _| panic!(
            "unadmitted presence"
        )),
        Err(StorageBackendError::Memory(_))
    ));
    drop(hold);
    assert_eq!(control.memory().used(), baseline);
    let mut calls = 0;
    assert!(matches!(
        snapshot.for_each_fields_multi_ref_with_presence(&ids, &[], &mut |_, _, _| {
            calls += 1;
            control.cancellation().cancel();
            false
        }),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), baseline);
    control.cancellation().reset();
}
