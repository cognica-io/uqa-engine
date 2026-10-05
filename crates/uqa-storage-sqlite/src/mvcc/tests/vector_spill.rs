//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_storage::{
    key_value::conformance::verify_vector_transaction_spill, KeyValueStorageBackend,
    PersistentStorageProvider,
};

#[rstest::rstest]
fn vector_journal_larger_than_its_session_allowance_rebases_and_commits(
    #[values(false, true)] native: bool,
    #[values(false, true)] hnsw: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let connection =
        crate::ManagedConnection::open(&directory.path().join("vector-spill.db")).unwrap();
    if native {
        crate::Catalog::open(connection.clone()).unwrap();
        let provider = crate::SQLiteStorageProvider::new(connection);
        verify_vector_transaction_spill(&*provider.open_session().unwrap().backend, hnsw).unwrap();
    } else {
        let store = crate::SQLiteKeyValueStore::new(connection).unwrap();
        verify_vector_transaction_spill(&KeyValueStorageBackend::new(Arc::new(store)), hnsw)
            .unwrap();
    }
}
