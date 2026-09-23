//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::NativeSnapshot;
use crate::{Catalog, ManagedConnection, SQLiteDocumentStore};
use uqa_storage::{mvcc::VersionedSessionOptions, read_control::StorageReadControl, DocumentStore};

#[test]
fn borrowed_native_identity_pages_keep_their_lease_during_reentrant_writes() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    for id in [1, 3, 5] {
        documents.put(id, BTreeMap::new()).unwrap();
    }
    let captured = connection.native_snapshot().unwrap().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let snapshot = SQLiteDocumentStore {
        conn: connection,
        table: "docs".into(),
        retained: Some(std::sync::Arc::new(NativeSnapshot {
            view: captured.view.try_clone().unwrap(),
            control: control.clone(),
            database: captured.database,
        })),
    };
    let retained = control.memory().used();
    assert_eq!(
        snapshot
            .for_each_next_fields(None, 3, &["value"], &mut |_, _| {
                panic!("an unsupported cursor must not invoke its consumer")
            })
            .unwrap(),
        None
    );
    let mut visited = Vec::new();
    assert_eq!(
        snapshot
            .for_each_next_fields(None, 3, &[], &mut |id, values| {
                assert!(values.is_empty());
                assert!(
                    control.memory().used() > retained,
                    "native ID buffers must remain charged through callbacks"
                );
                visited.push(id);
                if id == 1 {
                    documents.delete(3).unwrap();
                    documents.put(4, BTreeMap::new()).unwrap();
                }
                true
            })
            .unwrap(),
        Some(3)
    );
    assert_eq!(visited, [1, 3, 5]);
    assert_eq!(documents.next_doc_ids(None, 3).unwrap(), [1, 4, 5]);
    assert_eq!(control.memory().used(), retained);
    let mut stopped = Vec::new();
    assert_eq!(
        snapshot
            .for_each_next_fields(Some(1), 3, &[], &mut |id, _| {
                stopped.push(id);
                false
            })
            .unwrap(),
        Some(1)
    );
    assert_eq!(stopped, [3]);
    assert_eq!(control.memory().used(), retained);
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    assert!(matches!(
        snapshot.for_each_next_fields(None, 1, &[], &mut |_, _| {
            panic!("a rejected page must not invoke its consumer")
        }),
        Err(uqa_storage::StorageBackendError::Memory(_))
    ));
    drop(full);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(
        snapshot
            .for_each_next_fields(None, 0, &[], &mut |_, _| {
                panic!("an empty page must not invoke its consumer")
            })
            .unwrap(),
        Some(0)
    );
    control.cancellation().cancel();
    assert!(matches!(
        snapshot.for_each_next_fields(None, 0, &[], &mut |_, _| true),
        Err(uqa_storage::StorageBackendError::Cancelled(_))
    ));
}

#[test]
fn native_identity_page_buffers_share_the_read_allowance() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    connection.begin_transaction().unwrap();
    for id in 1..=4096 {
        documents.put(id, BTreeMap::new()).unwrap();
    }
    connection.commit_transaction().unwrap();
    let captured = connection.native_snapshot().unwrap().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let snapshot = NativeSnapshot {
        view: captured.view.try_clone().unwrap(),
        control: control.clone(),
        database: captured.database,
    };
    let read = NativeDocumentRead::new(&snapshot, "docs").unwrap();
    let ids = read.ids(None, 4096).unwrap();
    assert_eq!(ids.len(), 4096);
    assert_eq!(ids.first(), Some(&1));
    assert_eq!(ids.last(), Some(&4096));
    assert!(
        control.memory().peak() >= ids.capacity() * size_of::<DocId>(),
        "building the caller-owned identity buffer must reserve its full capacity"
    );
    assert_eq!(control.memory().used(), 0);
    assert_eq!(read.ids(Some(4095), 1).unwrap(), [4096]);
    let full = control.memory().reserve(control.memory().limit()).unwrap();
    assert!(matches!(read.ids(None, 1), Err(SQLiteError::Memory(_))));
    assert!(read.ids(None, 0).unwrap().is_empty());
    drop(full);
    assert_eq!(read.ids(None, 1).unwrap(), [1]);
    control.cancellation().cancel();
    assert!(matches!(read.ids(None, 0), Err(SQLiteError::Cancelled(_))));
    assert_eq!(control.memory().used(), 0);
}
