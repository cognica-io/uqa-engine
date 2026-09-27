//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_storage::key_value::conformance::{
    verify_diskann_resource_reopen, verify_diskann_resource_source,
};
use uqa_storage::KeyValueStorageBackend;

#[test]
fn diskann_resources_bound_cold_provider_generations_and_concurrent_queries() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("resource-acceptance.redb");
    let generation = {
        let storage = crate::RedbStorage::open(&path).unwrap();
        let backend = KeyValueStorageBackend::new(Arc::new(storage.store()));
        verify_diskann_resource_source(&backend).unwrap()
    };
    let storage = crate::RedbStorage::open(&path).unwrap();
    let backend = KeyValueStorageBackend::new(Arc::new(storage.store()));
    verify_diskann_resource_reopen(&backend, generation).unwrap();
}
