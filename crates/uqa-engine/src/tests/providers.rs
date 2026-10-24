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
