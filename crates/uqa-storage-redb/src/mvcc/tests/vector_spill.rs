//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_storage::{
    key_value::conformance::verify_vector_transaction_spill, KeyValueStorageBackend,
};

#[test]
fn vector_journal_larger_than_its_session_allowance_rebases_and_commits() {
    for hnsw in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let storage = crate::RedbStorage::open(directory.path().join("vector-spill.redb")).unwrap();
        verify_vector_transaction_spill(
            &KeyValueStorageBackend::new(Arc::new(storage.store())),
            hnsw,
        )
        .unwrap();
    }
}
