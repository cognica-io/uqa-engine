//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checkpoint contents independently determine whether an admitted operation needs persistence.

use proptest::prelude::*;

use super::*;
use crate::mvcc::{LocalSerializableLeases, SerializableStatus};

#[test]
fn initialization_and_encoding_do_not_acknowledge_durable_publication() {
    let (mut graph, control) = setup();
    assert!(graph.checkpoint_changed());
    encode(&graph, &control);
    assert!(graph.checkpoint_changed());
    graph = handoff(graph, &control);
    assert!(!graph.checkpoint_changed());
    graph.reclaim();
    assert!(!graph.checkpoint_changed());
    let actor = graph.admit(false, &control).unwrap();
    assert!(graph.checkpoint_changed());
    graph = handoff(graph, &control);
    graph
        .observe_read(actor, point(b"absent"), &control)
        .unwrap();
    assert!(graph.checkpoint_changed());
    graph = handoff(graph, &control);
    graph
        .observe_read(actor, point(b"absent"), &control)
        .unwrap();
    graph.check_active(actor).unwrap();
    graph.reclaim();
    assert!(!graph.checkpoint_changed());
}

#[test]
fn duplicate_preparation_and_terminal_receipts_need_no_replacement() {
    for committed in [false, true] {
        let (mut graph, control) = setup();
        let actor = graph.admit(false, &control).unwrap();
        let physical = StorageTransactionId::new(DATABASE, 10).unwrap();
        let publication = graph
            .prepare_publication(actor, physical, [9; 32], &control)
            .unwrap();
        graph = handoff(graph, &control);
        graph.prepare_commit(actor, &control).unwrap();
        graph
            .prepare_publication(actor, physical, [9; 32], &control)
            .unwrap();
        graph
            .resolve_publication(publication, CommitStatus::Pending)
            .unwrap();
        assert!(!graph.checkpoint_changed());
        let status = if committed {
            CommitStatus::Committed(CommitReceipt {
                transaction: physical,
                sequence: CommitSequence::from_u64(1),
                fingerprint: [9; 32],
            })
        } else {
            CommitStatus::Aborted
        };
        graph.resolve_publication(publication, status).unwrap();
        assert!(graph.checkpoint_changed());
        graph = handoff(graph, &control);
        assert_eq!(
            graph
                .resolve_publication(publication, CommitStatus::Unknown)
                .unwrap(),
            status
        );
        assert_eq!(
            graph.resolve_publication(publication, status).unwrap(),
            status
        );
        assert!(!graph.checkpoint_changed());
    }
}

#[test]
fn partial_receipt_recovery_keeps_changes_after_a_later_error() {
    let (mut graph, control) = setup();
    let a = graph.admit(false, &control).unwrap();
    let b = graph.admit(false, &control).unwrap();
    let physical = StorageTransactionId::new(DATABASE, 11).unwrap();
    graph
        .prepare_publication(a, physical, [9; 32], &control)
        .unwrap();
    graph
        .prepare_publication(
            b,
            StorageTransactionId::new(DATABASE, 12).unwrap(),
            [8; 32],
            &control,
        )
        .unwrap();
    graph = handoff(graph, &control);
    assert!(matches!(
        graph.reconcile_publications(&control, |transaction| {
            Ok(if transaction == physical {
                CommitStatus::Committed(CommitReceipt {
                    transaction,
                    sequence: CommitSequence::from_u64(1),
                    fingerprint: [9; 32],
                })
            } else {
                CommitStatus::Unknown
            })
        }),
        Err(VersionError::UnknownTransaction)
    ));
    assert!(graph.checkpoint_changed());
    graph = handoff(graph, &control);
    assert_eq!(graph.status(a).unwrap(), SerializableStatus::Committed);
    assert_eq!(graph.status(b).unwrap(), SerializableStatus::Prepared);
    assert!(graph
        .reconcile_publications(&control, |_| Ok(CommitStatus::Unknown))
        .is_err());
    assert!(!graph.checkpoint_changed());
}

#[test]
fn released_terminal_leases_require_persistence_before_reclamation() {
    let (mut graph, control) = setup();
    let leases = std::sync::Arc::new(LocalSerializableLeases::new(control.memory()));
    let actor = graph
        .admit_with_lease(true, &control, |id| leases.retain(id, &control))
        .unwrap();
    finish(&mut graph, actor.id(), &control);
    // The lease uses the same allowance, so restore without the unleased handoff helper.
    let bytes = encode(&graph, &control);
    drop(graph);
    let mut graph =
        SerializableGraph::read_checkpoint(DATABASE, COORDINATOR, &mut bytes.as_slice(), &control)
            .unwrap();
    graph.reclaim();
    assert!(!graph.checkpoint_changed());
    let id = actor.id();
    drop(actor);
    graph
        .recover_abandoned(
            &control,
            |id| Ok(leases.is_alive(id)),
            |_| panic!("terminal receipt"),
        )
        .unwrap();
    assert!(graph.checkpoint_changed());
    assert_eq!(graph.status(id).unwrap(), SerializableStatus::Committed);
    graph.reclaim();
    assert!(matches!(
        graph.status(id),
        Err(VersionError::UnknownTransaction)
    ));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn mutation_tracking_matches_encoded_state_across_admitted_histories(
        operations in prop::collection::vec((0u8..14, 0usize..3, 0u8..8), 1..80)
    ) {
        let (mut graph, control) = setup();
        let mut actors = [
            graph.admit(true, &control).unwrap(),
            graph.admit(false, &control).unwrap(),
            graph.admit(false, &control).unwrap(),
        ];
        let mut marks = actors.map(|id| graph.write_mark(id).ok());
        graph = handoff(graph, &control);
        for (operation, index, key) in operations {
            let before = encode(&graph, &control);
            let id = actors[index];
            let bytes = [key];
            let _ = match operation {
                0 => graph.observe_read(id, point(&bytes), &control),
                1 => graph.observe_write(id, point(&bytes), &control),
                2 => graph.observe_rw(id, id, actors[(index + 1) % actors.len()], &control),
                3 => graph.prepare_commit(id, &control),
                4 => graph.commit(id),
                5 => graph.rollback(id),
                6 => { graph.reclaim(); Ok(()) }
                7 => graph.safe_snapshot(id, &control).map(|_| ()),
                8 => graph.check_active(id),
                9 => graph.prepare_publication(id, StorageTransactionId::new(DATABASE, id.allocation() + 100).unwrap(), [7; 32], &control).map(|_| ()),
                10 => graph.publication(id).and_then(|publication| {
                    publication.map_or(Ok(()), |publication| {
                        let status = if key % 2 == 0 {
                            CommitStatus::Aborted
                        } else {
                            CommitStatus::Committed(CommitReceipt {
                                transaction: publication.transaction(),
                                sequence: CommitSequence::from_u64(1),
                                fingerprint: publication.fingerprint(),
                            })
                        };
                        graph.resolve_publication(publication, status).map(|_| ())
                    })
                }),
                11 => marks[index].map_or(Ok(()), |mark| graph.rollback_writes(mark)),
                12 => { marks[index] = graph.write_mark(id).ok(); Ok(()) }
                _ => graph.admit(index == 0, &control).map(|id| { actors[index] = id; marks[index] = graph.write_mark(id).ok(); }),
            };
            let after = encode(&graph, &control);
            prop_assert_eq!(graph.checkpoint_changed(), before != after, "operation {}", operation);
            graph = handoff(graph, &control);
            prop_assert!(!graph.checkpoint_changed());
        }
    }
}
