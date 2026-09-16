//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn evaluated_memory_batches_discard_errors_unwinds_and_cancellation() {
    super::super::conformance::verify_compound_mutations(&MemoryKeyValueStore::new()).unwrap();
}

#[test]
fn hnsw_handles_follow_memory_undo_and_reject_canonical_drift() {
    super::super::conformance::verify_hnsw_undo(store()).unwrap();
}

#[test]
fn ivf_handles_follow_memory_undo_and_definition_changes() {
    super::super::conformance::verify_ivf_undo(store()).unwrap();
}

#[test]
fn vector_snapshots_retain_tensors_and_reject_mutation_after_live_handles_close() {
    super::super::conformance::verify_vector_snapshots(&store()).unwrap();
}
