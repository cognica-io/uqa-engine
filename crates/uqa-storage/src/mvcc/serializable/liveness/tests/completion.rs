//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A live participant retains its terminal result independently of obsolete conflict history.

use super::*;
use crate::mvcc::{SerializableKeySpace, SerializablePredicate, SerializableStatus};

fn recover(
    graph: &mut SerializableGraph,
    leases: &LocalSerializableLeases,
    control: &StorageReadControl,
) {
    graph
        .recover_abandoned(
            control,
            |id| Ok(leases.is_alive(id)),
            |_| panic!("completed actors need no physical outcome lookup"),
        )
        .unwrap();
    graph.reclaim();
    leases.reclaim();
}

#[test]
fn read_only_completion_survives_reclamation_checkpoint_and_nested_handle_lifetimes() {
    let (mut graph, leases, control) = setup();
    let actor = graph
        .admit_with_lease(true, &control, |id| leases.retain(id, &control))
        .unwrap();
    let id = actor.id();
    assert_eq!(graph.status(id).unwrap(), SerializableStatus::Active);
    graph
        .observe_read(id, SerializablePredicate::object([7; 16]), &control)
        .unwrap();
    graph.prepare_commit(id, &control).unwrap();
    assert_eq!(graph.status(id).unwrap(), SerializableStatus::Prepared);
    graph.commit(id).unwrap();
    graph.reclaim();
    assert_eq!(graph.status(id).unwrap(), SerializableStatus::Committed);
    assert!(graph.publication(id).unwrap().is_none());
    assert!(graph.predicates.reads.is_empty());
    let mut checkpoint = Vec::new();
    graph.write_checkpoint(&mut checkpoint, &control).unwrap();
    let nested = actor.clone();
    drop((graph, actor));
    let mut graph = SerializableGraph::read_checkpoint(
        id.database(),
        id.coordinator(),
        &mut checkpoint.as_slice(),
        &control,
    )
    .unwrap();
    recover(&mut graph, &leases, &control);
    graph.commit(id).unwrap();
    assert_eq!(graph.status(id).unwrap(), SerializableStatus::Committed);
    drop(nested);
    recover(&mut graph, &leases, &control);
    assert!(matches!(
        graph.status(id),
        Err(VersionError::UnknownTransaction)
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(
        graph.admit(true, &control).unwrap().allocation(),
        id.allocation() + 1
    );
}

#[test]
fn completed_physical_publications_retain_terminal_precedence_until_lease_release() {
    let (mut graph, leases, control) = setup();
    let a = graph
        .admit_with_lease(false, &control, |id| leases.retain(id, &control))
        .unwrap();
    let b = graph
        .admit_with_lease(false, &control, |id| leases.retain(id, &control))
        .unwrap();
    let committed = graph
        .prepare_publication(
            a.id(),
            StorageTransactionId::new(graph.database(), 41).unwrap(),
            [2; 32],
            &control,
        )
        .unwrap();
    let aborted = graph
        .prepare_publication(
            b.id(),
            StorageTransactionId::new(graph.database(), 42).unwrap(),
            [3; 32],
            &control,
        )
        .unwrap();
    let receipt = CommitReceipt {
        transaction: committed.transaction(),
        sequence: CommitSequence::from_u64(9),
        fingerprint: [2; 32],
    };
    graph
        .resolve_publication(committed, CommitStatus::Committed(receipt))
        .unwrap();
    graph
        .resolve_publication(aborted, CommitStatus::Aborted)
        .unwrap();
    recover(&mut graph, &leases, &control);
    assert_eq!(graph.status(a.id()).unwrap(), SerializableStatus::Committed);
    assert_eq!(graph.status(b.id()).unwrap(), SerializableStatus::Aborted);
    assert_eq!(
        graph
            .resolve_publication(committed, CommitStatus::Unknown)
            .unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert_eq!(
        graph
            .resolve_publication(aborted, CommitStatus::Unknown)
            .unwrap(),
        CommitStatus::Aborted
    );
    drop((a, b));
    recover(&mut graph, &leases, &control);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_outcomes_do_not_keep_obsolete_predicates_or_edges_for_new_snapshots() {
    let (mut graph, leases, control) = setup();
    let reader = graph
        .admit_with_lease(true, &control, |id| leases.retain(id, &control))
        .unwrap();
    let writer = graph
        .admit_with_lease(false, &control, |id| leases.retain(id, &control))
        .unwrap();
    let predicate = SerializablePredicate::point([7; 16], SerializableKeySpace::Rows, b"key");
    graph
        .observe_read(reader.id(), predicate, &control)
        .unwrap();
    graph
        .observe_write(writer.id(), predicate, &control)
        .unwrap();
    for id in [writer.id(), reader.id()] {
        graph.prepare_commit(id, &control).unwrap();
        graph.commit(id).unwrap();
    }
    // Keep both old handles while a later snapshot establishes a new history horizon.
    let later = graph.admit(true, &control).unwrap();
    graph.reclaim();
    assert_eq!(
        graph.status(reader.id()).unwrap(),
        SerializableStatus::Committed
    );
    assert_eq!(
        graph.status(writer.id()).unwrap(),
        SerializableStatus::Committed
    );
    assert!(graph.outgoing.is_empty() && graph.incoming.is_empty());
    assert!(graph.predicates.reads.is_empty() && graph.predicates.writes.is_empty());
    assert_eq!(
        graph.safe_snapshot(later, &control).unwrap(),
        crate::mvcc::SafeSnapshot::Safe
    );
    graph.observe_read(later, predicate, &control).unwrap();
    assert!(graph.outgoing.is_empty());
    let newer = graph.admit(false, &control).unwrap();
    graph.observe_write(newer, predicate, &control).unwrap();
    assert_eq!(graph.outgoing.len(), 1);
    for id in [newer, later] {
        graph.prepare_commit(id, &control).unwrap();
        graph.commit(id).unwrap();
    }
    drop((reader, writer));
    recover(&mut graph, &leases, &control);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_later_liveness_failure_cannot_discard_a_live_terminal_result() {
    let (mut graph, leases, control) = setup();
    let a = graph
        .admit_with_lease(true, &control, |id| leases.retain(id, &control))
        .unwrap();
    let b = graph
        .admit_with_lease(true, &control, |id| leases.retain(id, &control))
        .unwrap();
    for id in [a.id(), b.id()] {
        graph.prepare_commit(id, &control).unwrap();
        graph.commit(id).unwrap();
    }
    let first = a.id();
    drop(a);
    assert!(graph
        .recover_abandoned(
            &control,
            |id| if id == first {
                Ok(false)
            } else {
                Err(VersionError::InvalidEncoding("liveness unavailable"))
            },
            |_| panic!("terminal outcomes must not resolve another receipt"),
        )
        .is_err());
    graph.reclaim();
    assert!(matches!(
        graph.status(first),
        Err(VersionError::UnknownTransaction)
    ));
    assert_eq!(graph.status(b.id()).unwrap(), SerializableStatus::Committed);
    drop(b);
    recover(&mut graph, &leases, &control);
    assert_eq!(control.memory().used(), 0);
}
