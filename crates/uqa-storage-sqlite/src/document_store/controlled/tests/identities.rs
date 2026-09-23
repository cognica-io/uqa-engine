//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-owned identity pages preserve provider views and avoid large native or legacy bodies.

use crate::{
    Catalog, ManagedConnection, SQLiteCompressionOptions, SQLiteDocumentStore, SQLiteKeyValueStore,
};
use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_storage::{
    document_store::read_document_ids, key_value::KeyValueDocumentStore,
    mvcc::VersionedSessionOptions, read_control::StorageReadControl, DocumentStore,
    StorageBackendError,
};

#[derive(Clone, Copy)]
enum Provider {
    Legacy,
    Native,
    KeyValue,
}

const PROVIDERS: [Provider; 3] = [Provider::Legacy, Provider::Native, Provider::KeyValue];

fn verify(connection: ManagedConnection, provider: Provider) {
    let mut source: Box<dyn DocumentStore> = match provider {
        Provider::KeyValue => Box::new(KeyValueDocumentStore::new(
            std::sync::Arc::new(SQLiteKeyValueStore::new(connection).unwrap()),
            "docs",
        )),
        Provider::Legacy | Provider::Native => {
            Catalog::open(connection.clone()).unwrap();
            if matches!(provider, Provider::Native) {
                connection
                    .bind_native_records(VersionedSessionOptions::default())
                    .unwrap();
            }
            Box::new(SQLiteDocumentStore::new(connection, "docs"))
        }
    };
    for id in [1, 3, 5] {
        source
            .put(
                id,
                [("opaque".into(), Value::Str("x".repeat(128 << 10)))].into(),
            )
            .unwrap();
    }
    let snapshot = (!matches!(provider, Provider::Legacy)).then(|| source.snapshot().unwrap());
    source.delete(3).unwrap();
    source.put(4, BTreeMap::new()).unwrap();
    let control = StorageReadControl::with_limit(8 << 10);
    let views = std::iter::once((source.as_ref(), [4, 5]))
        .chain(snapshot.as_ref().map(|view| (view.as_ref(), [3, 5])));
    for (view, expected) in views {
        let ids = read_document_ids(view, Some(1), 2, &control).unwrap();
        assert_eq!(&*ids, &expected);
        assert_eq!(control.memory().used(), ids.capacity() * size_of::<u64>());
        let occupied = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        assert!(matches!(
            read_document_ids(view, None, 1, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert!(read_document_ids(view, Some(u64::MAX), 0, &control)
            .unwrap()
            .is_empty());
        drop(occupied);
        drop(ids);
        assert_eq!(control.memory().used(), 0);
        let ids = read_document_ids(view, Some(4), usize::MAX, &control).unwrap();
        assert_eq!(&*ids, &[5]);
        drop(ids);
        control.cancellation().cancel();
        assert!(matches!(
            read_document_ids(view, None, 0, &control),
            Err(StorageBackendError::Cancelled(_))
        ));
        control.cancellation().reset();
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn sqlite_controlled_identity_pages_hold_the_invoking_allowance_in_all_mappings() {
    for provider in PROVIDERS {
        verify(ManagedConnection::open_in_memory().unwrap(), provider);
    }
}

#[test]
fn controlled_identity_pages_keep_file_modes_and_native_retained_views() {
    for provider in PROVIDERS {
        for mode in 0..4 {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("identities.sqlite");
            let connection = match mode {
                0 => ManagedConnection::open(&path),
                1 => ManagedConnection::open_encrypted(&path, "controlled-identities"),
                2 => ManagedConnection::open_compressed(&path, SQLiteCompressionOptions::default()),
                _ => ManagedConnection::open_compressed_encrypted(
                    &path,
                    "controlled-identities",
                    SQLiteCompressionOptions::default(),
                ),
            }
            .unwrap();
            verify(connection, provider);
        }
    }
}

#[test]
fn native_identity_lookup_charges_owner_addressing_under_the_invoking_allowance() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let documents = SQLiteDocumentStore::new(connection, "x".repeat(16 << 10));
    let control = StorageReadControl::with_limit(64);
    assert!(matches!(
        read_document_ids(&documents, None, 1, &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
    assert!(read_document_ids(&documents, None, 0, &control)
        .unwrap()
        .is_empty());
}
