//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common sequence consumers preserve their chosen redb session through conflicts and reopen.

use std::sync::Arc;
use uqa_storage::key_value::conformance::{verify_sequence_concurrency, verify_sequence_reopen};
use uqa_storage::KeyValueStore;
use uqa_storage_redb::RedbStorage;

#[test]
fn sequence_consumers_share_conflict_undo_and_reopen_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sequences.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let b: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        verify_sequence_concurrency(&a, &b).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(reopened.store());
    verify_sequence_reopen(&store).unwrap();
}
