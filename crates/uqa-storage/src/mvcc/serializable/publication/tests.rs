//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Receipt identity, indeterminate outcomes, terminal precedence and cleanup after cancellation.

use super::*;
use crate::mvcc::{DatabaseId, SafeSnapshot};

const DATABASE: DatabaseId = DatabaseId::from_bytes([51; 16]);

fn transaction(allocation: u64) -> StorageTransactionId {
    StorageTransactionId::new(DATABASE, allocation).unwrap()
}

fn setup() -> (
    SerializableGraph,
    SerializableTransactionId,
    StorageReadControl,
) {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut graph = SerializableGraph::new(DATABASE, [52; 16], control.memory()).unwrap();
    let participant = graph.admit(false, &control).unwrap();
    (graph, participant, control)
}

fn receipt(publication: SerializablePublication) -> CommitReceipt {
    CommitReceipt {
        transaction: publication.transaction(),
        sequence: CommitSequence::from_u64(9),
        fingerprint: publication.fingerprint(),
    }
}

#[test]
fn admission_reconciles_committed_data_before_registering_the_next_snapshot() {
    use crate::mvcc::SerializablePredicate;
    let (mut graph, first, control) = setup();
    let second = graph.admit(false, &control).unwrap();
    graph
        .observe_write(first, SerializablePredicate::object([1; 16]), &control)
        .unwrap();
    graph
        .observe_write(second, SerializablePredicate::object([2; 16]), &control)
        .unwrap();
    let committed = graph
        .prepare_publication(first, transaction(7), [3; 32], &control)
        .unwrap();
    graph
        .prepare_publication(second, transaction(8), [4; 32], &control)
        .unwrap();
    let mut queried = Vec::new();
    graph
        .reconcile_publications(&control, |physical| {
            queried.push(physical);
            Ok(if physical == transaction(7) {
                CommitStatus::Committed(receipt(committed))
            } else {
                CommitStatus::Pending
            })
        })
        .unwrap();
    assert_eq!(queried, [transaction(7), transaction(8)]);
    let reader = graph.admit(true, &control).unwrap();
    graph
        .observe_read(reader, SerializablePredicate::object([1; 16]), &control)
        .unwrap();
    graph
        .observe_read(reader, SerializablePredicate::object([2; 16]), &control)
        .unwrap();
    assert!(!graph
        .outgoing
        .iter()
        .any(|edge| edge.0 == reader.allocation() && edge.1 == first.allocation()));
    assert!(graph
        .outgoing
        .iter()
        .any(|edge| edge.0 == reader.allocation() && edge.1 == second.allocation()));
    assert_eq!(
        graph.safe_snapshot(reader, &control).unwrap(),
        SafeSnapshot::Pending
    );
}

#[test]
fn unavailable_receipts_stop_admission_without_discarding_confirmed_resolutions() {
    let (mut graph, first, control) = setup();
    let second = graph.admit(false, &control).unwrap();
    let committed = graph
        .prepare_publication(first, transaction(7), [3; 32], &control)
        .unwrap();
    let uncertain = graph
        .prepare_publication(second, transaction(8), [4; 32], &control)
        .unwrap();
    assert!(matches!(
        graph.reconcile_publications(&control, |physical| {
            Ok(if physical == transaction(7) {
                CommitStatus::Committed(receipt(committed))
            } else {
                CommitStatus::Unknown
            })
        }),
        Err(VersionError::UnknownTransaction)
    ));
    assert_eq!(
        graph
            .resolve_publication(committed, CommitStatus::Unknown)
            .unwrap(),
        CommitStatus::Committed(receipt(committed))
    );
    assert!(matches!(
        graph.check_active(second),
        Err(VersionError::TransactionSealed)
    ));
    graph
        .reconcile_publications(&control, |physical| {
            assert_eq!(physical, uncertain.transaction());
            Ok(CommitStatus::Aborted)
        })
        .unwrap();
    graph.reclaim();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn pending_and_unknown_receipts_retain_the_original_physical_binding() {
    let (mut graph, participant, control) = setup();
    let publication = graph
        .prepare_publication(participant, transaction(7), [3; 32], &control)
        .unwrap();
    assert_ne!(
        participant.allocation(),
        publication.transaction().allocation()
    );
    assert_eq!(publication.participant(), participant);
    assert_eq!(graph.publication(participant).unwrap(), Some(publication));
    assert_eq!(
        graph
            .prepare_publication(participant, transaction(7), [3; 32], &control)
            .unwrap(),
        publication
    );
    let reader = graph.admit(true, &control).unwrap();
    for status in [CommitStatus::Pending, CommitStatus::Unknown] {
        assert_eq!(
            graph.resolve_publication(publication, status).unwrap(),
            status
        );
        graph.reclaim();
        assert!(matches!(
            graph.check_active(participant),
            Err(VersionError::TransactionSealed)
        ));
        assert_eq!(
            graph.safe_snapshot(reader, &control).unwrap(),
            SafeSnapshot::Pending
        );
    }
    assert!(matches!(
        graph.commit(participant),
        Err(VersionError::InvalidEncoding(_))
    ));
    assert!(matches!(
        graph.rollback(participant),
        Err(VersionError::InvalidEncoding(_))
    ));
    graph
        .resolve_publication(publication, CommitStatus::Aborted)
        .unwrap();
    assert_eq!(
        graph.safe_snapshot(reader, &control).unwrap(),
        SafeSnapshot::Safe
    );
}

#[test]
fn confirmed_commit_finishes_without_memory_or_cancellation_and_cannot_be_downgraded() {
    let (mut graph, participant, control) = setup();
    let publication = graph
        .prepare_publication(participant, transaction(7), [3; 32], &control)
        .unwrap();
    let committed = receipt(publication);
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    control.cancellation().cancel();
    assert_eq!(
        graph
            .resolve_publication(publication, CommitStatus::Committed(committed))
            .unwrap(),
        CommitStatus::Committed(committed)
    );
    for status in [
        CommitStatus::Unknown,
        CommitStatus::Pending,
        CommitStatus::Committed(committed),
    ] {
        assert_eq!(
            graph.resolve_publication(publication, status).unwrap(),
            CommitStatus::Committed(committed)
        );
    }
    assert!(
        matches!(graph.resolve_publication(publication, CommitStatus::Aborted), Err(VersionError::AlreadyCommitted(actual)) if actual == committed)
    );
    drop(occupied);
    graph.reclaim();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn mismatched_receipts_or_repreparation_cannot_replace_a_sealed_publication() {
    let (mut graph, participant, control) = setup();
    let publication = graph
        .prepare_publication(participant, transaction(7), [3; 32], &control)
        .unwrap();
    for (physical, fingerprint) in [(transaction(8), [3; 32]), (transaction(7), [4; 32])] {
        assert!(matches!(
            graph.prepare_publication(participant, physical, fingerprint, &control),
            Err(VersionError::CommitMismatch)
        ));
        let mismatched = CommitReceipt {
            transaction: physical,
            fingerprint,
            ..receipt(publication)
        };
        assert!(matches!(
            graph.resolve_publication(publication, CommitStatus::Committed(mismatched)),
            Err(VersionError::CommitMismatch)
        ));
        assert_eq!(graph.publication(participant).unwrap(), Some(publication));
        assert!(matches!(
            graph.check_active(participant),
            Err(VersionError::TransactionSealed)
        ));
    }
    graph
        .resolve_publication(publication, CommitStatus::Committed(receipt(publication)))
        .unwrap();
}

#[test]
fn confirmed_abort_releases_intents_and_preserves_its_terminal_outcome() {
    let (mut graph, participant, control) = setup();
    graph
        .observe_write(
            participant,
            crate::mvcc::SerializablePredicate::object([1; 16]),
            &control,
        )
        .unwrap();
    let publication = graph
        .prepare_publication(participant, transaction(7), [3; 32], &control)
        .unwrap();
    control.cancellation().cancel();
    assert_eq!(
        graph
            .resolve_publication(publication, CommitStatus::Aborted)
            .unwrap(),
        CommitStatus::Aborted
    );
    for status in [
        CommitStatus::Unknown,
        CommitStatus::Pending,
        CommitStatus::Aborted,
    ] {
        assert_eq!(
            graph.resolve_publication(publication, status).unwrap(),
            CommitStatus::Aborted
        );
    }
    assert!(
        matches!(graph.resolve_publication(publication, CommitStatus::Committed(receipt(publication))), Err(VersionError::AlreadyAborted(actual)) if actual == transaction(7))
    );
    graph.reclaim();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_durable_allocation_cannot_be_bound_to_two_live_participants() {
    let (mut graph, first, control) = setup();
    let second = graph.admit(false, &control).unwrap();
    graph
        .prepare_publication(first, transaction(7), [3; 32], &control)
        .unwrap();
    assert!(matches!(
        graph.prepare_publication(second, transaction(7), [3; 32], &control),
        Err(VersionError::CommitMismatch)
    ));
    assert!(graph.publication(second).unwrap().is_none());
    graph.check_active(second).unwrap();
    graph
        .prepare_publication(second, transaction(8), [3; 32], &control)
        .unwrap();
}

#[test]
fn read_only_noop_and_invalid_database_paths_do_not_allocate_publication_state() {
    let (mut graph, writable, control) = setup();
    let read_only = graph.admit(true, &control).unwrap();
    let foreign = StorageTransactionId::new(DatabaseId::from_bytes([99; 16]), 7).unwrap();
    assert!(matches!(
        graph.prepare_publication(writable, foreign, [3; 32], &control),
        Err(VersionError::WrongDatabase)
    ));
    for participant in [read_only, writable] {
        assert!(graph.publication(participant).unwrap().is_none());
        graph.prepare_commit(participant, &control).unwrap();
        graph.commit(participant).unwrap();
    }
    graph.reclaim();
    assert_eq!(control.memory().used(), 0);
}

fn restored_publication(
    committed: bool,
) -> (
    SerializableGraph,
    SerializablePublication,
    CommitStatus,
    StorageReadControl,
    Vec<u8>,
) {
    let (mut graph, participant, control) = setup();
    let publication = graph
        .prepare_publication(participant, transaction(7), [3; 32], &control)
        .unwrap();
    let outcome = if committed {
        CommitStatus::Committed(receipt(publication))
    } else {
        CommitStatus::Aborted
    };
    graph.resolve_publication(publication, outcome).unwrap();
    let mut encoded = Vec::new();
    graph.write_checkpoint(&mut encoded, &control).unwrap();
    let restored = SerializableGraph::read_checkpoint(
        graph.database(),
        graph.coordinator(),
        &mut encoded.as_slice(),
        &control,
    )
    .unwrap();
    (restored, publication, outcome, control, encoded)
}

#[test]
fn restored_terminal_publications_require_their_exact_durable_receipts() {
    for committed in [false, true] {
        let (mut graph, publication, expected, control, original) = restored_publication(committed);
        let committed_receipt = receipt(publication);
        let alternatives = [
            CommitStatus::Unknown,
            CommitStatus::Pending,
            if committed {
                CommitStatus::Aborted
            } else {
                CommitStatus::Committed(committed_receipt)
            },
            CommitStatus::Committed(CommitReceipt {
                transaction: transaction(8),
                ..committed_receipt
            }),
            CommitStatus::Committed(CommitReceipt {
                sequence: CommitSequence::from_u64(10),
                ..committed_receipt
            }),
            CommitStatus::Committed(CommitReceipt {
                fingerprint: [4; 32],
                ..committed_receipt
            }),
        ];
        for authoritative in alternatives {
            let error = graph
                .validate_persisted_publications(&control, |queried| {
                    assert_eq!(queried, publication.transaction());
                    Ok(authoritative)
                })
                .unwrap_err();
            assert!(
                if authoritative == CommitStatus::Unknown {
                    matches!(error, VersionError::UnknownTransaction)
                } else {
                    matches!(error, VersionError::CommitMismatch)
                },
                "accepted {authoritative:?} for persisted {expected:?}"
            );
            assert!(!graph.checkpoint_changed());
            let mut encoded = Vec::new();
            graph.write_checkpoint(&mut encoded, &control).unwrap();
            assert_eq!(encoded, original);
            // Persisted-state validation cannot downgrade a retained live completion result.
            assert_eq!(
                graph
                    .resolve_publication(publication, CommitStatus::Unknown)
                    .unwrap(),
                expected
            );
        }
        let occupied = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        graph
            .validate_persisted_publications(&control, |queried| {
                assert_eq!(queried, publication.transaction());
                Ok(expected)
            })
            .unwrap();
        drop(occupied);
        assert!(!graph.checkpoint_changed());
    }
}

#[test]
fn persisted_publication_validation_preserves_cancellation_and_lookup_errors() {
    let (graph, _, expected, control, original) = restored_publication(true);
    control.cancellation().cancel();
    assert!(matches!(
        graph.validate_persisted_publications(&control, |_| panic!("cancelled receipt lookup")),
        Err(VersionError::Cancelled(_))
    ));
    control.cancellation().reset();
    assert!(matches!(
        graph.validate_persisted_publications(&control, |_| Err(VersionError::SequenceExhausted)),
        Err(VersionError::SequenceExhausted)
    ));
    let mut encoded = Vec::new();
    graph.write_checkpoint(&mut encoded, &control).unwrap();
    assert_eq!(encoded, original);
    graph
        .validate_persisted_publications(&control, |_| Ok(expected))
        .unwrap();
}

#[test]
fn persisted_validation_leaves_unresolved_publications_to_receipt_reconciliation() {
    let (mut graph, participant, control) = setup();
    let publication = graph
        .prepare_publication(participant, transaction(7), [3; 32], &control)
        .unwrap();
    graph
        .validate_persisted_publications(&control, |_| panic!("prepared receipt validation"))
        .unwrap();
    graph
        .reconcile_publications(&control, |_| {
            Ok(CommitStatus::Committed(receipt(publication)))
        })
        .unwrap();
    graph
        .validate_persisted_publications(&control, |_| {
            Ok(CommitStatus::Committed(receipt(publication)))
        })
        .unwrap();
}
