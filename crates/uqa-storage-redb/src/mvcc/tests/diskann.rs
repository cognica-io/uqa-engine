//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_storage::key_value::conformance::{
    verify_diskann_built_generation, verify_diskann_built_reopen,
};
use uqa_storage::key_value::conformance::{verify_diskann_generations, verify_diskann_reopen};
use uqa_storage::KeyValueStore;

#[test]
fn diskann_generations_use_shared_redb_ownership_and_cold_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diskann.redb");
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    let generation = verify_diskann_generations(&store).unwrap();
    drop(store);
    drop(owner);
    let reopened = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(reopened.store());
    verify_diskann_reopen(&store, generation).unwrap();
}

#[test]
fn diskann_bounded_build_seals_and_reopens_complete_redb_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let path = directory.path().join("built.redb");
    let (generation, memory_peak, temporary_peak) = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        verify_diskann_built_generation(&store, temporary.path()).unwrap()
    };
    assert!(std::fs::read_dir(temporary.path())
        .unwrap()
        .next()
        .is_none());
    eprintln!("redb DiskANN build: memory={memory_peak}, encrypted temporary={temporary_peak}");
    drop(temporary);
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    verify_diskann_built_reopen(&store, generation).unwrap();
}
