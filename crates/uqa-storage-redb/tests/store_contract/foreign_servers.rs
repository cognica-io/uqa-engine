//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_storage::{
    key_value::conformance::{verify_foreign_server_reopen, verify_foreign_server_rows},
    KeyValueStore,
};
use uqa_storage_redb::RedbStorage;

#[test]
fn foreign_server_metadata_preserves_snapshots_undo_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign-servers.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let b: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        verify_foreign_server_rows(&a, &b).unwrap();
    }
    let storage = RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(storage.store());
    verify_foreign_server_reopen(&store).unwrap();
}
