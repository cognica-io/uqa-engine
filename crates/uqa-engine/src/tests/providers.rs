//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit provider identities for native API and lifecycle qualification.

use crate::Engine;
use std::{path::Path, sync::Arc};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Provider {
    Memory,
    SQLiteNative,
    SQLiteKeyValue,
    Redb,
}

impl Provider {
    pub(crate) fn open_with_retention(self, path: &Path, retained_bytes: usize) -> Engine {
        let options = uqa_storage::mvcc::VersionedSessionOptions { retained_bytes };
        match self {
            Self::Memory => Engine::in_memory_with_retention(uqa_core::memory::MemoryBudget::new(
                retained_bytes,
            )),
            Self::SQLiteNative => {
                let connection = uqa_storage_sqlite::ManagedConnection::open(path).unwrap();
                connection.bind_native_records(options).unwrap();
                Engine::from_persistent_provider(Arc::new(
                    uqa_storage_sqlite::SQLiteStorageProvider::new(connection),
                ))
                .unwrap()
            }
            Self::SQLiteKeyValue => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open_with_options(path, options)
                    .unwrap(),
            ))
            .unwrap(),
            Self::Redb => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open_with_options(path, options).unwrap(),
            ))
            .unwrap(),
        }
    }

    pub(crate) fn open(self, path: &Path) -> Engine {
        match self {
            Self::Memory => Engine::new(),
            Self::SQLiteNative => Engine::open(path).unwrap(),
            Self::SQLiteKeyValue => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
            ))
            .unwrap(),
            Self::Redb => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(path).unwrap(),
            ))
            .unwrap(),
        }
    }
}
