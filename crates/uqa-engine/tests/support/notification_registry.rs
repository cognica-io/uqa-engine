//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `SQLite` connection for inspecting or locking an existing notification registry.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use uqa_storage::{encryption_key::AuxiliaryFile, StorageEncryptionKey};

pub(crate) fn open(database: &Path, credential: Option<StorageEncryptionKey>) -> Connection {
    let file = AuxiliaryFile::Notifications;
    let path = file.path(database);
    let registry = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    if let Some(credential) = credential {
        let key = credential.for_file(file, &path).unwrap();
        registry
            .pragma_update(None, "key", key.expose_secret())
            .unwrap();
    }
    registry
}
