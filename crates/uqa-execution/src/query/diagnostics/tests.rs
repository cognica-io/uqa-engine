//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    vector_execution::{DiskANNExecutionRoute, DiskANNQueryWork, DiskANNTraversalStats},
    PostingList,
};
use uqa_storage::{diskann_index::format::DiskANNGeneration, vector_index::DiskANNExecutionStats};

fn result() -> VectorQueryResult {
    VectorQueryResult {
        postings: PostingList::new(),
        diskann: Some(DiskANNExecutionStats {
            generation: DiskANNGeneration::new([1; 16], 2, 3, 4).unwrap(),
            route: DiskANNExecutionRoute::Approximate,
            traversal: DiskANNTraversalStats {
                pq_estimates: 7,
                ..DiskANNTraversalStats::default()
            },
            work: DiskANNQueryWork::default(),
        }),
    }
}

fn record(collector: &VectorDiagnostics, relation: &str) -> StorageBackendResult<()> {
    collector.record(
        relation,
        "embedding",
        VectorSearchOperation::KNN { k: 2 },
        &result(),
    )
}

#[test]
fn diskann_diagnostics_nested_invocations_are_inclusive_and_release_their_allowance() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner.enter(control.clone()).unwrap();
    record(&outer.collector, "before").unwrap();
    let inner = owner.enter(control.clone()).unwrap();
    record(&inner.collector, "nested").unwrap();
    let child = inner.finish().unwrap();
    assert_eq!(child.len(), 1);
    assert_eq!(&*child[0].relation, "nested");
    assert!(Arc::ptr_eq(&owner.capture().unwrap(), &outer.collector));
    record(&outer.collector, "after").unwrap();
    let parent = outer.finish().unwrap();
    assert_eq!(parent.len(), 3);
    assert!(Arc::ptr_eq(&parent[1], &child[0]));
    assert_eq!(parent[1].traversal.pq_estimates, 7);
    assert!(owner.capture().is_none());
    drop((parent, child));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_diagnostics_parallel_callbacks_keep_their_captured_parent_and_session_identity() {
    let owner = QueryDiagnostics::default();
    let independent = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner.enter(control.clone()).unwrap();
    let captured = owner.capture().unwrap();
    let worker = owner.fork();
    std::thread::scope(|threads| {
        for _ in 0..4 {
            threads.spawn(|| {
                let _bound = worker.bind(&captured).unwrap();
                assert!(Arc::ptr_eq(&owner.capture().unwrap(), &captured));
                assert!(independent.capture().is_none());
                let inner = owner.enter(control.clone()).unwrap();
                record(&inner.collector, "worker").unwrap();
                assert_eq!(inner.finish().unwrap().len(), 1);
                assert!(Arc::ptr_eq(&worker.capture().unwrap(), &captured));
            });
        }
    });
    assert_eq!(outer.finish().unwrap().len(), 4);
    assert!(owner.capture().is_none());
}

#[test]
fn diskann_diagnostics_restore_scope_after_errors_and_unwind() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner.enter(control.clone()).unwrap();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _inner = owner.enter(control).unwrap();
        panic!("simulated host failure");
    }));
    assert!(caught.is_err());
    assert!(Arc::ptr_eq(&owner.capture().unwrap(), &outer.collector));
    let retained = owner.capture().unwrap();
    drop(outer);
    assert!(owner.capture().is_none());
    assert!(record(&retained, "late").is_err());
}

#[test]
fn diskann_diagnostics_caught_retention_failure_cannot_publish_incomplete_analysis() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(4096);
    let outer = owner.enter(control.clone()).unwrap();
    let child = owner.enter(StorageReadControl::with_limit(65_536)).unwrap();
    let _exhausted = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    assert!(matches!(
        record(&child.collector, "child"),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(child.finish().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(outer.finish().unwrap_err().sqlstate(), Some("53200"));
    assert!(owner.capture().is_none());
}

#[test]
fn diskann_diagnostics_do_not_invent_work_for_an_unsupported_index_and_preserve_cancellation() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(4096);
    let scope = owner.enter(control.clone()).unwrap();
    scope
        .collector
        .record(
            "docs",
            "embedding",
            VectorSearchOperation::KNN { k: 1 },
            &VectorQueryResult {
                postings: PostingList::new(),
                diskann: None,
            },
        )
        .unwrap();
    assert!(scope.finish().unwrap().is_empty());
    let scope = owner.enter(control.clone()).unwrap();
    control.cancellation().cancel();
    assert!(matches!(
        record(&scope.collector, "cancelled"),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(scope.finish().unwrap_err().sqlstate(), Some("57014"));
    assert!(owner.capture().is_none());
}
