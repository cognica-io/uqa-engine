//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document ownership and durable reservation transfer use redb's real common consumers.

use super::*;
use uqa_storage::key_value::conformance::{verify_document_ownership, verify_document_reopen};

#[test]
fn document_owners_coordinate_and_reopen_after_all_handles_close() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("documents.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: std::sync::Arc<dyn KeyValueStore> = std::sync::Arc::new(storage.store());
        let b = a.open_session().unwrap();
        verify_document_ownership(&a, &b).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    let store: std::sync::Arc<dyn KeyValueStore> = std::sync::Arc::new(reopened.store());
    verify_document_reopen(&store).unwrap();
}
