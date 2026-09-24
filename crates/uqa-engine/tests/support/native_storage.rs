//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit native storage sessions for integration fixtures and durable inspection.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;
use uqa_storage::{
    mvcc::{VersionedKeyValueStore, VersionedSessionOptions},
    read_control::StorageReadControl,
    KeyValueStore,
};
use uqa_storage_sqlite::{
    mvcc::native::{NativeRecordFamily, NativeRecordIdentity, NativeRecordOwner},
    ManagedConnection, SQLiteRecordStore,
};

#[path = "native_catalog.rs"]
mod native_catalog;
pub(crate) use native_catalog::{catalog, legacy_engine};

/// Leave a document's BLOB reference without its payload through the actual record protocol. Projection tests must fail if they fetch that unrequested field.
pub(crate) fn remove_blob(path: &Path, table: &str, doc_id: i64, field: &str) {
    let schema = catalog(ManagedConnection::open(path).unwrap())
        .unwrap()
        .load_tables()
        .unwrap()
        .into_iter()
        .find(|schema| schema.relation.qualified_name() == table)
        .unwrap();
    let control = StorageReadControl::with_limit(VersionedSessionOptions::default().retained_bytes);
    let key = NativeRecordIdentity::new(
        NativeRecordFamily::DocumentBlobs,
        NativeRecordOwner::Object {
            identity: schema.object_id,
            generation: schema.storage_generation,
        },
    )
    .unwrap()
    .encode_key(
        &[
            rusqlite::types::ValueRef::Integer(doc_id),
            rusqlite::types::ValueRef::Text(field.as_bytes()),
        ],
        &control,
    )
    .unwrap();
    let records =
        SQLiteRecordStore::for_native(&ManagedConnection::open(path).unwrap(), &control).unwrap();
    let store =
        VersionedKeyValueStore::new(Arc::new(records), None, VersionedSessionOptions::default());
    assert!(store.get(&key).unwrap().is_some());
    store.delete(&key).unwrap();
}
