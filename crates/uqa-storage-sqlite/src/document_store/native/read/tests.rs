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
fn maximum_native_id_uses_bounded_work_independent_of_live_document_count() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    let work = Arc::new(AtomicUsize::new(0));
    for count in [32, 4096] {
        connection.begin_transaction().unwrap();
        for id in 1..=count {
            documents.put(id, BTreeMap::new()).unwrap();
        }
        connection.commit_transaction().unwrap();
        let counter = Arc::clone(&work);
        connection
            .with_physical(|sqlite| {
                sqlite.progress_handler(
                    1,
                    Some(move || {
                        counter.fetch_add(1, Ordering::Relaxed);
                        false
                    }),
                )?;
                Ok(())
            })
            .unwrap();
        work.store(0, Ordering::Relaxed);
        assert_eq!(documents.max_doc_id().unwrap(), count);
        let instructions = work.load(Ordering::Relaxed);
        assert!(
            instructions < 2000,
            "maximum ID query used {instructions} SQLite VM instructions for {count} documents"
        );
        connection
            .with_physical(|sqlite| {
                sqlite.progress_handler(0, None::<fn() -> bool>)?;
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn maximum_native_id_follows_private_deletes_undo_and_retained_snapshots() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    assert_eq!(documents.max_doc_id().unwrap(), 0);
    for id in [1, 50, 100] {
        documents.put(id, BTreeMap::new()).unwrap();
    }
    let retained = documents.snapshot().unwrap();
    connection.begin_transaction().unwrap();
    documents.put(200, BTreeMap::new()).unwrap();
    connection.savepoint("larger").unwrap();
    documents.delete(200).unwrap();
    documents.delete(100).unwrap();
    assert_eq!(documents.max_doc_id().unwrap(), 50);
    assert_eq!(retained.max_doc_id().unwrap(), 100);
    let other = connection.new_session();
    SQLiteDocumentStore::new(other.clone(), "docs")
        .put(300, BTreeMap::new())
        .unwrap();
    assert_eq!(documents.max_doc_id().unwrap(), 50);
    connection.rollback_to_savepoint("larger").unwrap();
    assert_eq!(documents.max_doc_id().unwrap(), 200);
    connection.rollback_transaction().unwrap();
    assert_eq!(documents.max_doc_id().unwrap(), 300);
    assert_eq!(retained.max_doc_id().unwrap(), 100);
    for id in [300, 100, 50, 1] {
        documents.delete(id).unwrap();
    }
    assert_eq!(documents.max_doc_id().unwrap(), 0);
}

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
            history: captured.history,
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
                assert_eq!(values.len(), 0);
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
        history: captured.history,
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
    assert_eq!(read.ids(None, 0).unwrap().len(), 0);
    drop(full);
    assert_eq!(read.ids(None, 1).unwrap(), [1]);
    control.cancellation().cancel();
    assert!(matches!(read.ids(None, 0), Err(SQLiteError::Cancelled(_))));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn the_latest_document_count_reads_stored_rows_and_other_snapshots_count_records() {
    use rusqlite::hooks::{AuthAction, Authorization};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    assert_eq!(documents.len().unwrap(), 0);
    connection.begin_transaction().unwrap();
    for id in 1..=64 {
        documents.put(id, BTreeMap::new()).unwrap();
    }
    assert_eq!(documents.len().unwrap(), 64);
    connection.commit_transaction().unwrap();
    let retained = documents.snapshot().unwrap();
    documents.delete(7).unwrap();
    documents.put(100, BTreeMap::new()).unwrap();
    documents.put(101, BTreeMap::new()).unwrap();

    let stored_reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&stored_reads);
    connection
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Read {
                        table_name: "_documents",
                        ..
                    }
                ) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    stored_reads.store(0, Ordering::Relaxed);
    assert_eq!(documents.len().unwrap(), 65);
    assert!(
        stored_reads.swap(0, Ordering::Relaxed) > 0,
        "the latest count read no stored rows"
    );
    assert_eq!(retained.len().unwrap(), 64);
    assert_eq!(
        stored_reads.swap(0, Ordering::Relaxed),
        0,
        "a retained snapshot counted the latest stored rows"
    );
    connection.begin_transaction().unwrap();
    documents.delete(1).unwrap();
    documents.put(102, BTreeMap::new()).unwrap();
    documents.delete(103).unwrap();
    stored_reads.store(0, Ordering::Relaxed);
    // The stored rows are counted, and the transaction's own records add or remove what they insert or delete; deleting a document that was never stored removes nothing.
    assert_eq!(documents.len().unwrap(), 65);
    assert!(
        stored_reads.swap(0, Ordering::Relaxed) > 0,
        "the count of a transaction that changed the table read no stored rows"
    );
    connection.rollback_transaction().unwrap();
    assert_eq!(documents.len().unwrap(), 65);
    connection
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(
                crate::connection::PREPARED_STATEMENT_CACHE_CAPACITY,
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn latest_document_ids_read_the_stored_rows_with_private_records_merged() {
    use rusqlite::hooks::{AuthAction, Authorization};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    for id in [10, 20, 30, 40] {
        documents.put(id, BTreeMap::new()).unwrap();
    }
    let stored_reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&stored_reads);
    connection
        .with_physical(|sqlite| {
            sqlite.set_prepared_statement_cache_capacity(0);
            sqlite.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Read {
                        table_name: "_documents",
                        ..
                    }
                ) {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Authorization::Allow
            }))?;
            Ok(())
        })
        .unwrap();
    connection.begin_transaction().unwrap();
    // Identities before, between and after the stored rows, and a deletion of one of them.
    documents.delete(20).unwrap();
    for id in [5, 35, 50] {
        documents.put(id, BTreeMap::new()).unwrap();
    }
    stored_reads.store(0, Ordering::Relaxed);
    assert_eq!(documents.doc_ids().unwrap(), [5, 10, 30, 35, 40, 50]);
    assert!(
        stored_reads.swap(0, Ordering::Relaxed) > 0,
        "the identities were not read from the stored rows"
    );
    // A page resumes after the last identity it visited, stored or private.
    let page = |after, limit| {
        let mut ids = Vec::new();
        documents
            .for_each_next_fields_borrowed(after, limit, &[], &mut |id, _| {
                ids.push(id);
                true
            })
            .unwrap();
        ids
    };
    assert_eq!(page(Some(10), 2), [30, 35]);
    assert_eq!(page(Some(40), 5), [50]);
    assert_eq!(page(None, 1), [5]);
    connection.rollback_transaction().unwrap();
    assert_eq!(documents.doc_ids().unwrap(), [10, 20, 30, 40]);
    connection
        .with_physical(|sqlite| {
            sqlite.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>)?;
            sqlite.set_prepared_statement_cache_capacity(
                crate::connection::PREPARED_STATEMENT_CACHE_CAPACITY,
            );
            Ok(())
        })
        .unwrap();
}
