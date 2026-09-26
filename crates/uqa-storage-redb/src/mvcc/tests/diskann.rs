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
fn diskann_runtime_reclamation_preserves_redb_undo_recreation_and_cold_reopen() {
    use redb::{ReadableDatabase, ReadableTable};
    use uqa_storage::key_value::conformance::{
        verify_diskann_reclamation_bounds, verify_diskann_reclamation_reopen,
        verify_diskann_runtime_reclaimed_reopen, verify_diskann_runtime_reclamation,
    };
    for private in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reclamation.redb");
        let (generations, partial) = {
            let owner = crate::RedbStorage::open(&path).unwrap();
            let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
            let generations = verify_diskann_runtime_reclamation(&store, private).unwrap();
            let partial = verify_diskann_reclamation_bounds(&store).unwrap();
            (generations, partial)
        };
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        verify_diskann_runtime_reclaimed_reopen(&store, generations).unwrap();
        verify_diskann_reclamation_reopen(&store, partial).unwrap();
        let records = owner.record_store().unwrap();
        let read = records.database.begin_read().unwrap();
        for entry in read
            .open_table(super::super::VERSIONS)
            .unwrap()
            .iter()
            .unwrap()
        {
            assert!(
                entry.unwrap().1.value().len() < 32768,
                "final-reader release reclaims large historical payloads"
            );
        }
    }
}

#[test]
fn diskann_runtime_retirement_preserves_redb_undo_recreation_and_cold_reopen() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_runtime_retirement, verify_diskann_runtime_retirement_reopen,
    };
    for private in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retirement.redb");
        let generations = {
            let owner = crate::RedbStorage::open(&path).unwrap();
            let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
            verify_diskann_runtime_retirement(&store, private).unwrap()
        };
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        verify_diskann_runtime_retirement_reopen(&store, generations).unwrap();
    }
}

#[test]
fn diskann_runtime_adoption_conflicts_with_redb_unstamped_insertions() {
    let directory = tempfile::tempdir().unwrap();
    let owner = crate::RedbStorage::open(directory.path().join("adoption.redb")).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    uqa_storage::key_value::conformance::verify_diskann_runtime_adoption_conflicts(&store).unwrap();
}

#[test]
fn diskann_runtime_lifecycle_preserves_redb_transactions_and_cold_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime.redb");
    let generation = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        uqa_storage::key_value::conformance::verify_diskann_runtime_lifecycle(&store).unwrap()
    };
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    uqa_storage::key_value::conformance::verify_diskann_runtime_reopen(&store, generation).unwrap();
}

#[test]
fn diskann_live_writes_keep_actual_catalog_visibility_and_redb_cold_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("live.redb");
    let generation = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        uqa_storage::key_value::conformance::verify_diskann_live_writes(&store).unwrap()
    };
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    uqa_storage::key_value::conformance::verify_diskann_live_reopen(&store, generation).unwrap();
}

#[test]
fn diskann_query_views_retain_private_and_old_committed_generations_in_redb() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("query-views.redb");
    let generation = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        uqa_storage::key_value::conformance::verify_diskann_query_views(&store).unwrap()
    };
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    uqa_storage::key_value::conformance::verify_diskann_query_reopen(&store, generation).unwrap();
}

#[test]
fn diskann_publication_is_atomic_and_reopens_in_redb() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_publication, verify_diskann_publication_reopen,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("publication.redb");
    let generation = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        verify_diskann_publication(&store).unwrap()
    };
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    verify_diskann_publication_reopen(&store, generation).unwrap();
}

#[test]
fn diskann_catalog_identity_handles_survive_redb_cold_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.redb");
    let generation = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let foreign = crate::RedbStorage::open(directory.path().join("foreign.redb")).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        let other: Arc<dyn KeyValueStore> = Arc::new(foreign.store());
        uqa_storage::key_value::conformance::verify_diskann_catalog_identity(&store, &other)
            .unwrap()
    };
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    uqa_storage::key_value::conformance::verify_diskann_catalog_identity_reopen(&store, generation)
        .unwrap();
}

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

#[test]
fn diskann_pruning_preserves_late_changes_and_reopens_in_redb() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_pruning, verify_diskann_pruning_reopen,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pruning.redb");
    let generation = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        verify_diskann_pruning(&store).unwrap()
    };
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    verify_diskann_pruning_reopen(&store, generation).unwrap();
}
