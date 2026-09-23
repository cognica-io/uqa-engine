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
