//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_storage::key_value::conformance::{
    verify_diskann_built_generation, verify_diskann_built_reopen, verify_diskann_canonical_origins,
    verify_diskann_canonical_reopen, verify_mutation_origins,
};
use uqa_storage::key_value::conformance::{verify_diskann_generations, verify_diskann_reopen};
use uqa_storage::KeyValueStore;

#[test]
fn diskann_catalog_binding_checks_actual_redb_definitions_and_publication_races() {
    let directory = tempfile::tempdir().unwrap();
    let owner = crate::RedbStorage::open(directory.path().join("binding.redb")).unwrap();
    let foreign = crate::RedbStorage::open(directory.path().join("foreign.redb")).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    let other: Arc<dyn KeyValueStore> = Arc::new(foreign.store());
    uqa_storage::key_value::conformance::verify_diskann_catalog_binding(&store, &other).unwrap();
}

#[test]
fn diskann_canonical_origins_and_tensors_reopen_through_redb() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("origins.redb");
    let origin = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        verify_diskann_canonical_origins(&store).unwrap()
    };
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    verify_diskann_canonical_reopen(&store, origin).unwrap();
}

#[test]
fn diskann_mutation_origins_resolve_actual_redb_receipts_and_failed_evaluation() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Arc::new(redb::Database::create(directory.path().join("lifecycle.redb")).unwrap());
    let persistence = Arc::new(super::RedbRecordStore::new(database).unwrap());
    verify_mutation_origins(persistence).unwrap();
}

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
