//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{path::Path, sync::Arc};

use uqa_storage::key_value::conformance::{verify_diskann_generations, verify_diskann_reopen};
use uqa_storage::KeyValueStore;

use crate::connection::ManagedConnection;
use crate::key_value::SQLiteKeyValueStore;
use crate::SQLiteCompressionOptions;

fn connection(path: &Path, mode: u8) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "diskann-test-key"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "diskann-test-key",
            SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

#[test]
fn diskann_generations_reopen_through_sqlite_plain_encrypted_and_compressed_owners() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diskann.db");
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        let generation = verify_diskann_generations(&store).unwrap();
        drop(store);
        let reopened: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        verify_diskann_reopen(&reopened, generation).unwrap();
    }
}
