//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Undo releases empty evaluated buffers without discarding retained record readers.

use super::*;
use crate::mvcc::vector::{IndexKind, Mutation};
use crate::mvcc::{GraphMutation, MemoryVersionStore};
use uqa_core::memory::MemoryBudget;

fn transaction(control: &StorageReadControl) -> Transaction {
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    Transaction::at_snapshot(Arc::new(store.snapshot().unwrap()), false, control)
}

fn stage(transaction: &mut Transaction, control: &StorageReadControl) -> VersionResult<()> {
    let graph = OwnedGraphMutation::retain(GraphMutation::InvalidateGraph("graph"), control)?;
    transaction.graph_mutation(&graph)?;
    let vector = OwnedVectorMutation::retain(
        IndexKind::IVFIndex,
        b"metadata",
        Mutation::Replace {
            document: 1,
            vectors: &[vec![1.0; 1024]],
        },
        control,
    )?;
    transaction.vector_mutation(&vector)?;
    transaction.require_unchanged(b"definition", control)?;
    transaction.replace(b"row", Some(b"evaluated payload"), control)
}

#[test]
fn failed_atomic_mutation_releases_empty_buffers_under_exhaustion_and_cancellation() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut transaction = transaction(&control);
    let mut exhausted = None;
    let result: VersionResult<()> = transaction.atomic(|transaction| {
        stage(transaction, &control)?;
        exhausted = Some(
            control
                .memory()
                .reserve(control.memory().limit() - control.memory().used())?,
        );
        control.cancellation().cancel();
        Err(VersionError::InvalidEncoding("failed evaluated mutation"))
    });
    assert!(matches!(result, Err(VersionError::InvalidEncoding(_))));
    assert_eq!(transaction.graph.capacity(), 0);
    assert_eq!(transaction.vector.capacity(), 0);
    assert_eq!(transaction.requirements.capacity(), 0);
    drop(exhausted);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn savepoint_undo_releases_empty_effects_and_last_savepoint_but_keeps_a_reader() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut transaction = transaction(&control);
    transaction.savepoint("before", &control).unwrap();
    stage(&mut transaction, &control).unwrap();
    let retained = transaction.view().unwrap();
    transaction.rollback_to("before", &control).unwrap();
    assert_eq!(transaction.graph.capacity(), 0);
    assert_eq!(transaction.vector.capacity(), 0);
    assert_eq!(transaction.requirements.capacity(), 0);
    transaction.release("before").unwrap();
    assert_eq!(transaction.savepoints.capacity(), 0);
    assert!(control.memory().used() > 0);
    retained
        .visit_value(b"row", &control, &mut |record| {
            assert_eq!(record.unwrap().value, Some(b"evaluated payload".as_slice()));
            Ok(())
        })
        .unwrap();
    drop(retained);
    assert_eq!(control.memory().used(), 0);
    assert!(transaction
        .view()
        .unwrap()
        .metadata(b"row", &control)
        .unwrap()
        .is_none());
}
