//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic dependency histories, independent serial-order checks and failure boundaries.

use super::*;

const COORDINATOR: [u8; 16] = [42; 16];
const DATABASE: DatabaseId = DatabaseId::from_bytes([19; 16]);

fn id(allocation: u64) -> SerializableTransactionId {
    SerializableTransactionId::new(DATABASE, COORDINATOR, allocation).unwrap()
}

fn setup() -> (SerializableGraph, StorageReadControl) {
    let control = StorageReadControl::with_limit(64 * 1024);
    (
        SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap(),
        control,
    )
}

fn begin(graph: &mut SerializableGraph, control: &StorageReadControl, ids: &[u64]) {
    for &allocation in ids {
        graph.begin(id(allocation), false, control).unwrap();
    }
}

fn edge(graph: &mut SerializableGraph, control: &StorageReadControl, reader: u64, writer: u64) {
    graph
        .observe_rw(id(reader), id(reader), id(writer), control)
        .unwrap();
}

fn finish(
    graph: &mut SerializableGraph,
    control: &StorageReadControl,
    allocation: u64,
) -> VersionResult<()> {
    graph.prepare_commit(id(allocation), control)?;
    graph.commit(id(allocation))
}

fn serialization_failure(result: VersionResult<()>, allocation: u64) {
    let error = result.unwrap_err();
    assert!(
        matches!(error, VersionError::SerializationConflict { transaction } if transaction == id(allocation)),
        "{error}"
    );
}

#[test]
fn write_skew_rejects_the_uncommitted_pivot_in_either_commit_order() {
    for (winner, victim) in [(1, 2), (2, 1)] {
        let (mut graph, control) = setup();
        begin(&mut graph, &control, &[1, 2]);
        edge(&mut graph, &control, 1, 2);
        edge(&mut graph, &control, 2, 1);
        finish(&mut graph, &control, winner).unwrap();
        serialization_failure(graph.check_active(id(victim)), victim);
        serialization_failure(graph.prepare_commit(id(victim), &control), victim);
        graph.rollback(id(victim)).unwrap();
        graph.reclaim();
        assert!(graph.transactions.is_empty());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn one_dependency_allows_both_commit_orders_without_blocking() {
    for order in [[1, 2], [2, 1]] {
        let (mut graph, control) = setup();
        begin(&mut graph, &control, &[1, 2]);
        edge(&mut graph, &control, 1, 2);
        for allocation in order {
            finish(&mut graph, &control, allocation).unwrap();
        }
    }
}

#[test]
fn dependency_chain_respects_commit_order_instead_of_rejecting_every_pivot() {
    for source_first in [false, true] {
        let (mut graph, control) = setup();
        begin(&mut graph, &control, &[1, 2, 3]);
        edge(&mut graph, &control, 1, 2);
        edge(&mut graph, &control, 2, 3);
        if source_first {
            for allocation in [1, 2, 3] {
                finish(&mut graph, &control, allocation).unwrap();
            }
        } else {
            finish(&mut graph, &control, 3).unwrap();
            serialization_failure(graph.prepare_commit(id(2), &control), 2);
            finish(&mut graph, &control, 1).unwrap();
        }
    }
}

#[test]
fn read_only_source_is_safe_when_the_outgoing_commit_follows_its_snapshot() {
    let (mut graph, control) = setup();
    graph.begin(id(1), true, &control).unwrap();
    begin(&mut graph, &control, &[2, 3]);
    edge(&mut graph, &control, 1, 2);
    edge(&mut graph, &control, 2, 3);
    for allocation in [3, 2, 1] {
        finish(&mut graph, &control, allocation).unwrap();
    }
}

#[test]
fn late_read_of_committed_pivot_rejects_a_read_only_anomaly_after_reclamation() {
    for reclaim in [false, true] {
        let (mut graph, control) = setup();
        begin(&mut graph, &control, &[2, 3]);
        edge(&mut graph, &control, 2, 3);
        finish(&mut graph, &control, 3).unwrap();
        graph.begin(id(1), true, &control).unwrap();
        finish(&mut graph, &control, 2).unwrap();
        if reclaim {
            graph.reclaim();
            assert!(matches!(
                graph.position(id(3)),
                Err(VersionError::UnknownTransaction)
            ));
            assert!(graph.node(2).summarized_out.is_some());
        }
        assert_eq!(
            graph.safe_snapshot(id(1), &control).unwrap(),
            SafeSnapshot::Unsafe
        );
        serialization_failure(graph.observe_rw(id(1), id(1), id(2), &control), 1);
    }
}

#[test]
fn an_earlier_read_only_snapshot_can_read_the_same_committed_pivot() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[2, 3]);
    graph.begin(id(1), true, &control).unwrap();
    edge(&mut graph, &control, 2, 3);
    finish(&mut graph, &control, 3).unwrap();
    finish(&mut graph, &control, 2).unwrap();
    graph.reclaim();
    edge(&mut graph, &control, 1, 2);
    finish(&mut graph, &control, 1).unwrap();
}

#[test]
fn prepared_pivot_is_preserved_and_the_committing_sink_is_rejected() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3]);
    edge(&mut graph, &control, 1, 2);
    graph.prepare_commit(id(2), &control).unwrap();
    graph.observe_rw(id(3), id(2), id(3), &control).unwrap();
    serialization_failure(graph.prepare_commit(id(3), &control), 3);
    graph.commit(id(2)).unwrap();
    finish(&mut graph, &control, 1).unwrap();
}

#[test]
fn uncertain_prepared_outcome_cannot_be_selected_as_a_victim() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3]);
    edge(&mut graph, &control, 2, 3);
    graph.prepare_commit(id(3), &control).unwrap();
    serialization_failure(graph.observe_rw(id(2), id(1), id(2), &control), 2);
    assert!(matches!(
        graph.check_active(id(3)),
        Err(VersionError::TransactionSealed)
    ));
    graph.reclaim();
    assert_eq!(graph.pending_finishes, 1);
    graph.commit(id(3)).unwrap();
    graph.commit(id(3)).unwrap();
    assert_eq!(graph.pending_finishes, 0);
}

#[test]
fn late_outgoing_dependency_checks_the_readers_incoming_edges() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3]);
    edge(&mut graph, &control, 1, 2);
    graph.prepare_commit(id(3), &control).unwrap();
    serialization_failure(graph.observe_rw(id(2), id(2), id(3), &control), 2);
    graph.commit(id(3)).unwrap();
}

#[test]
fn read_registration_can_doom_a_peer_writer_without_failing_the_reader() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3]);
    edge(&mut graph, &control, 2, 3);
    finish(&mut graph, &control, 3).unwrap();
    graph.observe_rw(id(1), id(1), id(2), &control).unwrap();
    graph.check_active(id(1)).unwrap();
    serialization_failure(graph.check_active(id(2)), 2);
    finish(&mut graph, &control, 1).unwrap();
}

#[test]
fn rejecting_a_local_write_preserves_savepoint_recovery_without_clearing_reads() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3]);
    // B read A's old row. A commits before B attempts the write that would close the cycle.
    edge(&mut graph, &control, 2, 1);
    finish(&mut graph, &control, 1).unwrap();
    let prior_edges = graph.outgoing.len();
    serialization_failure(graph.observe_rw(id(2), id(1), id(2), &control), 2);
    assert_eq!(graph.outgoing.len(), prior_edges);
    graph.check_active(id(2)).unwrap();
    // After undoing the rejected write, B can still finish. Its earlier read remains relevant to a later C -> B observation.
    graph.observe_rw(id(3), id(3), id(2), &control).unwrap();
    serialization_failure(graph.check_active(id(2)), 2);
}

#[test]
fn a_rejected_local_read_or_write_can_finish_after_undoing_only_that_operation() {
    for read in [false, true] {
        let (mut graph, control) = setup();
        begin(&mut graph, &control, &[1, 2, 3]);
        if read {
            edge(&mut graph, &control, 2, 3);
            finish(&mut graph, &control, 3).unwrap();
            finish(&mut graph, &control, 2).unwrap();
            serialization_failure(graph.observe_rw(id(1), id(1), id(2), &control), 1);
            finish(&mut graph, &control, 1).unwrap();
        } else {
            edge(&mut graph, &control, 2, 1);
            finish(&mut graph, &control, 1).unwrap();
            serialization_failure(graph.observe_rw(id(2), id(1), id(2), &control), 2);
            finish(&mut graph, &control, 2).unwrap();
        }
    }
}

#[test]
fn completed_readers_retain_dependencies_until_overlapping_writers_finish() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3]);
    edge(&mut graph, &control, 1, 2);
    finish(&mut graph, &control, 2).unwrap();
    graph.reclaim();
    graph.observe_rw(id(3), id(2), id(3), &control).unwrap();
    graph.observe_rw(id(3), id(3), id(1), &control).unwrap();
    serialization_failure(graph.check_active(id(1)), 1);
    finish(&mut graph, &control, 3).unwrap();
}

#[test]
fn duplicate_and_nonoverlapping_dependencies_do_not_consume_more_graph_space() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[9, 3]);
    edge(&mut graph, &control, 9, 3);
    let retained = control.memory().used();
    graph.observe_rw(id(3), id(9), id(3), &control).unwrap();
    assert_eq!(control.memory().used(), retained);
    assert_eq!(graph.outgoing.len(), 1);
    finish(&mut graph, &control, 9).unwrap();
    graph.begin(id(5), false, &control).unwrap();
    graph.observe_rw(id(5), id(9), id(5), &control).unwrap();
    graph.observe_rw(id(5), id(5), id(9), &control).unwrap();
    assert_eq!(graph.outgoing.len(), 1);
}

#[test]
fn safe_snapshot_waits_only_for_writers_present_at_admission() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1]);
    graph.begin(id(2), true, &control).unwrap();
    begin(&mut graph, &control, &[3]);
    assert_eq!(
        graph.safe_snapshot(id(2), &control).unwrap(),
        SafeSnapshot::Pending
    );
    finish(&mut graph, &control, 1).unwrap();
    assert_eq!(
        graph.safe_snapshot(id(2), &control).unwrap(),
        SafeSnapshot::Safe
    );
    graph.check_active(id(3)).unwrap();
}

#[test]
fn safe_snapshot_distinguishes_aborted_and_committed_uncertain_overlaps() {
    for committed in [false, true] {
        let (mut graph, control) = setup();
        begin(&mut graph, &control, &[1, 2]);
        edge(&mut graph, &control, 1, 2);
        finish(&mut graph, &control, 2).unwrap();
        graph.begin(id(3), true, &control).unwrap();
        graph.prepare_commit(id(1), &control).unwrap();
        assert_eq!(
            graph.safe_snapshot(id(3), &control).unwrap(),
            SafeSnapshot::Pending
        );
        if committed {
            graph.commit(id(1)).unwrap();
        } else {
            graph.rollback(id(1)).unwrap();
        }
        graph.reclaim();
        assert_eq!(
            graph.safe_snapshot(id(3), &control).unwrap(),
            if committed {
                SafeSnapshot::Unsafe
            } else {
                SafeSnapshot::Safe
            }
        );
    }
}

#[test]
fn aborted_dependencies_do_not_doom_surviving_transactions() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3]);
    edge(&mut graph, &control, 1, 2);
    edge(&mut graph, &control, 2, 3);
    graph.rollback(id(1)).unwrap();
    graph.reclaim();
    finish(&mut graph, &control, 3).unwrap();
    finish(&mut graph, &control, 2).unwrap();
}

#[test]
fn one_sink_can_doom_multiple_independent_pivots() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3, 4, 5]);
    for (reader, writer) in [(1, 2), (2, 5), (3, 4), (4, 5)] {
        edge(&mut graph, &control, reader, writer);
    }
    finish(&mut graph, &control, 5).unwrap();
    serialization_failure(graph.check_active(id(2)), 2);
    serialization_failure(graph.check_active(id(4)), 4);
}

#[test]
fn exhausted_budget_leaves_no_partial_dependency_and_preserves_retry() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2]);
    let remaining = 64 * 1024 - control.memory().used();
    let full = control.memory().reserve(remaining).unwrap();
    assert!(matches!(
        graph.observe_rw(id(1), id(1), id(2), &control),
        Err(VersionError::Memory(_))
    ));
    assert!(graph.outgoing.is_empty() && graph.incoming.is_empty());
    graph.check_active(id(1)).unwrap();
    drop(full);
    edge(&mut graph, &control, 1, 2);
    assert_eq!(graph.outgoing.len(), 1);
    assert_eq!(graph.incoming.len(), 1);
}

#[test]
fn failure_to_reserve_the_reverse_index_does_not_publish_the_forward_edge() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2]);
    // Allow one edge buffer, then fail while reserving the other orientation.
    graph.outgoing.reserve(1).unwrap();
    let full = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    assert!(matches!(
        graph.observe_rw(id(1), id(1), id(2), &control),
        Err(VersionError::Memory(_))
    ));
    assert!(graph.outgoing.is_empty() && graph.incoming.is_empty());
    drop(full);
    edge(&mut graph, &control, 1, 2);
    finish(&mut graph, &control, 2).unwrap();
    finish(&mut graph, &control, 1).unwrap();
}

#[test]
fn cancellation_does_not_doom_peers_or_prevent_terminal_cleanup() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2]);
    edge(&mut graph, &control, 1, 2);
    edge(&mut graph, &control, 2, 1);
    control.cancellation().cancel();
    assert!(graph.prepare_commit(id(1), &control).is_err());
    graph.check_active(id(1)).unwrap();
    graph.check_active(id(2)).unwrap();
    graph.rollback(id(1)).unwrap();
    graph.rollback(id(2)).unwrap();
    graph.reclaim();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn preparation_reserves_completion_order_before_physical_commit() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1]);
    graph.clock = u64::MAX - 2;
    graph.prepare_commit(id(1), &control).unwrap();
    graph.prepare_commit(id(1), &control).unwrap();
    assert!(matches!(
        graph.begin(id(2), false, &control),
        Err(VersionError::SequenceExhausted)
    ));
    graph.commit(id(1)).unwrap();
    assert_eq!(graph.clock, u64::MAX);
    assert_eq!(graph.pending_finishes, 0);
}

#[test]
fn wrong_incarnations_invalid_observers_and_read_only_writers_are_rejected() {
    let (mut graph, control) = setup();
    begin(&mut graph, &control, &[1, 2, 3]);
    graph.begin(id(4), true, &control).unwrap();
    let other =
        SerializableTransactionId::new(DatabaseId::from_bytes([20; 16]), COORDINATOR, 1).unwrap();
    assert!(matches!(
        graph.begin(other, false, &control),
        Err(VersionError::WrongDatabase)
    ));
    assert!(matches!(
        graph.observe_rw(id(1), id(1), other, &control),
        Err(VersionError::WrongDatabase)
    ));
    assert!(graph.observe_rw(id(3), id(1), id(2), &control).is_err());
    assert!(graph.observe_rw(id(1), id(1), id(4), &control).is_err());
    assert!(graph.begin(id(1), false, &control).is_err());
    assert!(graph.commit(id(1)).is_err());
    assert!(graph.outgoing.is_empty());
}

#[test]
fn every_three_transaction_history_leaves_an_acyclic_committed_dependency_graph() {
    let edges = [(1, 2), (1, 3), (2, 1), (2, 3), (3, 1), (3, 2)];
    let orders = [
        [1, 2, 3],
        [1, 3, 2],
        [2, 1, 3],
        [2, 3, 1],
        [3, 1, 2],
        [3, 2, 1],
    ];
    for mask in 0..64 {
        for order in orders {
            let (mut graph, control) = setup();
            begin(&mut graph, &control, &[1, 2, 3]);
            for (bit, &(reader, writer)) in edges.iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    edge(&mut graph, &control, reader, writer);
                }
            }
            let mut committed = [false; 3];
            for allocation in order {
                match finish(&mut graph, &control, allocation) {
                    Ok(()) => committed[allocation as usize - 1] = true,
                    Err(VersionError::SerializationConflict { .. }) => {
                        graph.rollback(id(allocation)).unwrap();
                    }
                    Err(error) => panic!("unexpected completion failure: {error}"),
                }
            }
            // Compute transitive closure independently of SSI's two-edge and commit-order checks.
            let mut reachable = [[false; 3]; 3];
            for (bit, &(reader, writer)) in edges.iter().enumerate() {
                let (reader, writer) = (reader as usize - 1, writer as usize - 1);
                reachable[reader][writer] =
                    mask & (1 << bit) != 0 && committed[reader] && committed[writer];
            }
            for middle in 0..3 {
                for from in 0..3 {
                    for to in 0..3 {
                        reachable[from][to] |= reachable[from][middle] && reachable[middle][to];
                    }
                }
            }
            assert!(
                (0..3).all(|node| !reachable[node][node]),
                "mask {mask}, order {order:?}"
            );
        }
    }
}

#[test]
fn delayed_read_or_write_observations_cannot_publish_a_dependency_cycle() {
    let edges = [(1, 2), (1, 3), (2, 1), (2, 3), (3, 1), (3, 2)];
    let orders = [
        [1, 2, 3],
        [1, 3, 2],
        [2, 1, 3],
        [2, 3, 1],
        [3, 1, 2],
        [3, 2, 1],
    ];
    // Each edge arrives when either its first or last endpoint finishes. This covers writes after a reader commits as well as reads of earlier concurrent writes.
    for mask in 0..64 {
        for delayed in 0..64 {
            if delayed & !mask != 0 {
                continue;
            }
            for order in orders {
                let (mut graph, control) = setup();
                begin(&mut graph, &control, &[1, 2, 3]);
                let mut done = [false; 3];
                let mut committed = [false; 3];
                let mut observed = [false; 6];
                for allocation in order {
                    let mut result = graph.check_active(id(allocation));
                    for (bit, &(reader, writer)) in edges.iter().enumerate() {
                        if result.is_err()
                            || mask & (1 << bit) == 0
                            || observed[bit]
                            || (reader != allocation && writer != allocation)
                        {
                            continue;
                        }
                        let other = if reader == allocation { writer } else { reader } as usize - 1;
                        if delayed & (1 << bit) != 0 && !done[other] {
                            continue;
                        }
                        observed[bit] = true;
                        result = graph.observe_rw(id(allocation), id(reader), id(writer), &control);
                    }
                    if result.is_ok() {
                        result = finish(&mut graph, &control, allocation);
                    }
                    let index = allocation as usize - 1;
                    done[index] = true;
                    match result {
                        Ok(()) => committed[index] = true,
                        Err(VersionError::SerializationConflict { .. }) => {
                            graph.rollback(id(allocation)).unwrap();
                        }
                        Err(error) => {
                            panic!("mask {mask}, delayed {delayed}, order {order:?}: {error}")
                        }
                    }
                }
                let mut reachable = [[false; 3]; 3];
                for (bit, &(reader, writer)) in edges.iter().enumerate() {
                    let (reader, writer) = (reader as usize - 1, writer as usize - 1);
                    reachable[reader][writer] =
                        mask & (1 << bit) != 0 && committed[reader] && committed[writer];
                }
                for middle in 0..3 {
                    for from in 0..3 {
                        for to in 0..3 {
                            reachable[from][to] |= reachable[from][middle] && reachable[middle][to];
                        }
                    }
                }
                assert!(
                    (0..3).all(|node| !reachable[node][node]),
                    "mask {mask}, delayed {delayed}, order {order:?}"
                );
            }
        }
    }
}
