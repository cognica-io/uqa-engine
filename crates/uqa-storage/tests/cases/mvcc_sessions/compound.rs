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
