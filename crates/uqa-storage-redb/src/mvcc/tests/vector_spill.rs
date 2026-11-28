//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_storage::{
    key_value::conformance::verify_vector_transaction_spill, KeyValueStorageBackend,
};

fn verify(hnsw: bool) {
    let directory = tempfile::tempdir().unwrap();
    let storage = crate::RedbStorage::open(directory.path().join("vector-spill.redb")).unwrap();
    verify_vector_transaction_spill(
        &KeyValueStorageBackend::new(Arc::new(storage.store())),
        hnsw,
    )
    .unwrap();
}

#[test]
fn spilled_ivf_journal_rebases_and_commits() {
    verify(false);
}

#[test]
fn spilled_hnsw_journal_rebases_and_commits() {
    verify(true);
}
