//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_storage::key_value::conformance::*;
use uqa_storage::KeyValueStore;
use uqa_storage_redb::RedbStorage;

#[test]
fn command_refresh_preserves_index_generations_and_reopens() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("commands.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let b: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        verify_command_refresh(&a, &b).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    verify_command_refresh_reopen(Arc::new(reopened.store())).unwrap();
}

#[test]
fn compound_reads_and_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("compound.redb")).unwrap();
    verify_compound_mutations(&storage.store()).unwrap();
    verify_compound_concurrency(&storage.store(), &storage.store()).unwrap();
}

#[test]
fn hnsw_undo_and_canonical_drift() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("undo.redb")).unwrap();
    verify_hnsw_undo(Arc::new(storage.store())).unwrap();
}

#[test]
fn hnsw_independent_commits_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hnsw.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let b: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        verify_hnsw_concurrency(&a, &b).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    verify_hnsw_reopen(Arc::new(reopened.store())).unwrap();
}

#[test]
fn ivf_undo_and_tensor_snapshot_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("ivf-undo.redb")).unwrap();
    verify_ivf_undo(Arc::new(storage.store())).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(storage.store());
    verify_vector_snapshots(&store).unwrap();
}

#[test]
fn ivf_and_exact_independent_commits_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ivf.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let b: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        verify_ivf_concurrency(&a, &b).unwrap();
        verify_exact_snapshot_concurrency(&a, &b).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    verify_ivf_reopen(Arc::new(reopened.store())).unwrap();
}

#[test]
fn occurrence_snapshots_independent_commits_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("occurrences.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let b: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        verify_occurrence_snapshots(&a).unwrap();
        verify_occurrence_concurrency(&a, &b).unwrap();
        verify_occurrence_accelerators(&a, &b).unwrap();
    }
    let storage = RedbStorage::open(&path).unwrap();
    verify_occurrence_reopen(Arc::new(storage.store())).unwrap();
}

#[test]
fn merged_vector_generations_conflicts_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ivf-merges.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        let b: Arc<dyn KeyValueStore> = Arc::new(storage.store());
        verify_vector_document_merges(&a, &b, VectorMergeKind::IVF).unwrap();
        verify_vector_document_merges(&a, &b, VectorMergeKind::HNSW).unwrap();
        verify_vector_merge_conflicts(&a, &b, VectorMergeKind::IVF).unwrap();
        verify_vector_merge_conflicts(&a, &b, VectorMergeKind::HNSW).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(reopened.store());
    verify_vector_merge_reopen(&store, VectorMergeKind::IVF).unwrap();
    verify_vector_merge_reopen(&store, VectorMergeKind::HNSW).unwrap();
}
