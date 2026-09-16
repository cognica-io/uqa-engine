//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::key_value::conformance::*;

use super::*;

#[test]
fn evaluated_batches_discard_errors_unwinds_and_cancellation() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    verify_compound_mutations(&store).unwrap();
    assert!(!store.in_transaction());
    assert_eq!(store.retention_control().memory().used(), 0);
}

#[test]
fn compound_reads_pin_visibility_and_mutations_pin_original_preconditions() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = a.new_session();
    verify_compound_concurrency(&a, &b).unwrap();
    assert_eq!(a.retention_control().memory().used(), 0);
}

#[test]
fn hnsw_handles_follow_versioned_undo_and_reject_canonical_drift() {
    let persistence = Persistence::new();
    verify_hnsw_undo(Arc::new(persistence.session(1 << 20))).unwrap();
}

#[test]
fn hnsw_handles_follow_independent_commits_and_pinned_reads() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
    let b = a.open_session().unwrap();
    verify_hnsw_concurrency(&a, &b).unwrap();
    verify_hnsw_reopen(a).unwrap();
}

#[test]
fn ivf_handles_follow_versioned_undo_and_definition_changes() {
    let persistence = Persistence::new();
    verify_ivf_undo(Arc::new(persistence.session(1 << 20))).unwrap();
}

#[test]
fn ivf_handles_follow_independent_commits_and_pinned_reads() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
    let b = a.open_session().unwrap();
    verify_ivf_concurrency(&a, &b).unwrap();
    verify_ivf_reopen(a).unwrap();
}

#[test]
fn vector_snapshots_retain_tensor_generations_and_are_read_only() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
    verify_vector_snapshots(&store).unwrap();
}

#[test]
fn exact_snapshots_remain_pinned_through_independent_commits() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
    let b = a.open_session().unwrap();
    verify_exact_snapshot_concurrency(&a, &b).unwrap();
}

#[test]
fn exact_snapshots_retain_their_allowance_and_release_failed_loads() {
    use uqa_storage::{KeyValueVectorIndex, VectorIndex};

    let persistence = Persistence::new();
    let writer = Arc::new(persistence.session(1 << 20));
    let mut vectors = KeyValueVectorIndex::new(writer.clone(), "quota", "v", 64);
    for doc in 1..=128 {
        vectors.add(doc, vec![1.0; 64]).unwrap();
    }
    let small = Arc::new(persistence.session(4096));
    let bounded = KeyValueVectorIndex::new(small.clone(), "quota", "v", 64);
    assert!(matches!(
        bounded.snapshot(),
        Err(StorageBackendError::Memory(
            uqa_core::memory::MemoryError::Limit { .. }
        ))
    ));
    assert_eq!(small.retention_control().memory().used(), 0);
    let retained = vectors.snapshot().unwrap();
    let control = writer.retention_control();
    let charged = control.memory().used();
    assert!(charged >= 128 * 64 * size_of::<f32>());
    let nested = retained.snapshot().unwrap();
    assert_eq!(control.memory().used(), charged);
    control.cancellation().cancel();
    assert!(matches!(
        retained.search_knn(&[1.0; 64], 1),
        Err(StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
    assert_eq!(retained.count().unwrap(), 128);
    drop(retained);
    assert_eq!(control.memory().used(), charged);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(vectors.count().unwrap(), 128);
}
