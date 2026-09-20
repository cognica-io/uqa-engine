//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical phantom histories, registration order, savepoint undo and bounded failure atomicity.

use std::ops::Bound::{Excluded, Included, Unbounded};

use super::*;
use crate::mvcc::{DatabaseId, SerializableKeySpace};

const COORDINATOR: [u8; 16] = [42; 16];
const DATABASE: DatabaseId = DatabaseId::from_bytes([31; 16]);
const TABLE: [u8; 16] = [7; 16];
const INDEX: SerializableKeySpace = SerializableKeySpace::Index([8; 16]);

fn id(value: u64) -> SerializableTransactionId {
    SerializableTransactionId::new(DATABASE, COORDINATOR, value).unwrap()
}

fn setup(count: u64) -> (SerializableGraph, StorageReadControl) {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut graph = SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap();
    for value in 1..=count {
        graph.begin(id(value), false, &control).unwrap();
    }
    (graph, control)
}

fn point(key: &[u8]) -> SerializablePredicate<'_> {
    SerializablePredicate::point(TABLE, INDEX, key)
}

fn finish(graph: &mut SerializableGraph, control: &StorageReadControl, value: u64) {
    graph.prepare_commit(id(value), control).unwrap();
    graph.commit(id(value)).unwrap();
}

fn has_edge(graph: &SerializableGraph, reader: u64, writer: u64) -> bool {
    graph
        .outgoing
        .iter()
        .any(|edge| edge.0 == reader && edge.1 == writer)
}

#[test]
fn empty_query_results_protect_range_boundaries_in_either_registration_order() {
    let cases = [
        (Unbounded, Unbounded, b"b".as_slice(), true),
        (
            Included(b"b".as_slice()),
            Included(b"d".as_slice()),
            b"b".as_slice(),
            true,
        ),
        (
            Excluded(b"b".as_slice()),
            Included(b"d".as_slice()),
            b"b".as_slice(),
            false,
        ),
        (
            Included(b"b".as_slice()),
            Included(b"d".as_slice()),
            b"d".as_slice(),
            true,
        ),
        (
            Included(b"b".as_slice()),
            Excluded(b"d".as_slice()),
            b"d".as_slice(),
            false,
        ),
        (
            Included(b"b".as_slice()),
            Excluded(b"d".as_slice()),
            b"c".as_slice(),
            true,
        ),
        (
            Included(b"b".as_slice()),
            Included(b"b".as_slice()),
            b"b".as_slice(),
            true,
        ),
        (
            Excluded(b"b".as_slice()),
            Included(b"b".as_slice()),
            b"b".as_slice(),
            false,
        ),
        (
            Included(b"d".as_slice()),
            Included(b"b".as_slice()),
            b"c".as_slice(),
            false,
        ),
        (Unbounded, Excluded(b"".as_slice()), b"".as_slice(), false),
        (
            Included(b"".as_slice()),
            Included(b"".as_slice()),
            b"".as_slice(),
            true,
        ),
        (
            Included(b"\xff".as_slice()),
            Unbounded,
            b"\xff\x00".as_slice(),
            true,
        ),
    ];
    for (lower, upper, key, expected) in cases {
        for write_first in [false, true] {
            let (mut graph, control) = setup(2);
            let read = SerializablePredicate::range(TABLE, INDEX, lower, upper);
            if write_first {
                graph.observe_write(id(2), point(key), &control).unwrap();
            }
            graph.observe_read(id(1), read, &control).unwrap();
            if !write_first {
                graph.observe_write(id(2), point(key), &control).unwrap();
            }
            assert_eq!(
                has_edge(&graph, 1, 2),
                expected,
                "{read:?}, {key:?}, {write_first}"
            );
            finish(&mut graph, &control, 2);
            finish(&mut graph, &control, 1);
        }
    }
}

#[test]
fn empty_byte_intervals_do_not_register_object_wide_conflicts() {
    for (lower, upper) in [
        (Unbounded, Excluded(b"".as_slice())),
        (Excluded(b"b".as_slice()), Included(b"b".as_slice())),
        (Excluded(b"b".as_slice()), Excluded(b"b\x00".as_slice())),
    ] {
        let (mut graph, control) = setup(2);
        graph
            .observe_read(
                id(1),
                SerializablePredicate::range(TABLE, INDEX, lower, upper),
                &control,
            )
            .unwrap();
        graph
            .observe_write(id(2), SerializablePredicate::object(TABLE), &control)
            .unwrap();
        assert!(graph.predicates.reads.is_empty());
        assert!(!has_edge(&graph, 1, 2));
    }
}

#[test]
fn object_incarnations_and_index_key_spaces_do_not_alias() {
    let same = point(b"key");
    let row = SerializablePredicate::point(TABLE, SerializableKeySpace::Rows, b"key");
    let other_index =
        SerializablePredicate::point(TABLE, SerializableKeySpace::Index([9; 16]), b"key");
    let other_table = SerializablePredicate::point([9; 16], INDEX, b"key");
    let object = SerializablePredicate::object(TABLE);
    for (reader, writer, expected) in [
        (same, same, true),
        (same, row, false),
        (same, other_index, false),
        (same, other_table, false),
        (object, same, true),
        (object, row, true),
        (same, object, true),
        (object, other_table, false),
    ] {
        let (mut graph, control) = setup(2);
        graph.observe_read(id(1), reader, &control).unwrap();
        graph.observe_write(id(2), writer, &control).unwrap();
        assert_eq!(has_edge(&graph, 1, 2), expected, "{reader:?}, {writer:?}");
    }
}

#[test]
fn logical_observations_reject_write_skew_without_physical_record_conflicts() {
    for (winner, victim) in [(1, 2), (2, 1)] {
        let (mut graph, control) = setup(2);
        let absent_range =
            SerializablePredicate::range(TABLE, INDEX, Included(b"a"), Excluded(b"z"));
        graph.observe_read(id(1), absent_range, &control).unwrap();
        graph.observe_read(id(2), absent_range, &control).unwrap();
        graph.observe_write(id(1), point(b"b"), &control).unwrap();
        graph.observe_write(id(2), point(b"y"), &control).unwrap();
        finish(&mut graph, &control, winner);
        assert!(matches!(
            graph.prepare_commit(id(victim), &control),
            Err(VersionError::SerializationConflict { .. })
        ));
    }
}

#[test]
fn late_reads_inspect_active_prepared_and_committed_write_intents() {
    for status in 0..3 {
        let (mut graph, control) = setup(2);
        graph.observe_write(id(2), point(b"x"), &control).unwrap();
        if status > 0 {
            graph.prepare_commit(id(2), &control).unwrap();
        }
        if status > 1 {
            graph.commit(id(2)).unwrap();
            graph.reclaim();
        }
        graph.observe_read(id(1), point(b"x"), &control).unwrap();
        assert!(has_edge(&graph, 1, 2));
        if status < 2 {
            finish(&mut graph, &control, 2);
        }
        finish(&mut graph, &control, 1);
        graph.reclaim();
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn a_writer_committed_before_the_readers_snapshot_does_not_conflict() {
    let (mut graph, control) = setup(2);
    graph.observe_write(id(1), point(b"x"), &control).unwrap();
    finish(&mut graph, &control, 1);
    graph.begin(id(3), false, &control).unwrap();
    graph.observe_read(id(3), point(b"x"), &control).unwrap();
    assert!(!has_edge(&graph, 3, 1));
    graph.rollback(id(2)).unwrap();
    graph.reclaim();
    assert!(graph.predicates.writes.is_empty());
}

#[test]
fn committed_readers_retain_predicates_until_overlapping_writers_finish() {
    let (mut graph, control) = setup(2);
    graph.observe_read(id(1), point(b"x"), &control).unwrap();
    finish(&mut graph, &control, 1);
    graph.reclaim();
    graph.observe_write(id(2), point(b"x"), &control).unwrap();
    assert!(has_edge(&graph, 1, 2));
    finish(&mut graph, &control, 2);
    graph.reclaim();
    assert!(graph.predicates.reads.is_empty());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn savepoint_undo_removes_later_intents_but_keeps_reads_and_observed_dependencies() {
    let (mut graph, control) = setup(3);
    graph.observe_read(id(1), point(b"read"), &control).unwrap();
    graph
        .observe_write(id(1), point(b"earlier"), &control)
        .unwrap();
    let mark = graph.write_mark(id(1)).unwrap();
    graph
        .observe_write(id(1), point(b"later"), &control)
        .unwrap();
    graph
        .observe_read(id(2), point(b"later"), &control)
        .unwrap();
    assert!(has_edge(&graph, 2, 1));
    graph.rollback_writes(mark).unwrap();
    graph
        .observe_read(id(3), point(b"later"), &control)
        .unwrap();
    assert!(!has_edge(&graph, 3, 1));
    graph
        .observe_read(id(3), point(b"earlier"), &control)
        .unwrap();
    graph
        .observe_write(id(3), point(b"read"), &control)
        .unwrap();
    assert!(has_edge(&graph, 3, 1));
    assert!(has_edge(&graph, 1, 3));
    assert!(has_edge(&graph, 2, 1));
}

#[test]
fn cancelled_or_doomed_transactions_can_undo_intents_without_erasing_their_reads() {
    let (mut graph, control) = setup(2);
    let mark = graph.write_mark(id(2)).unwrap();
    for value in 1..=2 {
        graph
            .observe_read(id(value), SerializablePredicate::object(TABLE), &control)
            .unwrap();
    }
    graph.observe_write(id(1), point(b"a"), &control).unwrap();
    graph.observe_write(id(2), point(b"b"), &control).unwrap();
    finish(&mut graph, &control, 1);
    control.cancellation().cancel();
    graph.rollback_writes(mark).unwrap();
    assert!(graph.predicates.reads.iter().any(|entry| entry.owner == 2));
    assert!(!graph.predicates.writes.iter().any(|entry| entry.owner == 2));
    assert!(matches!(
        graph.check_active(id(2)),
        Err(VersionError::SerializationConflict { .. })
    ));
    graph.rollback(id(2)).unwrap();
    graph.reclaim();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_failed_multi_match_observation_publishes_no_edges_or_peer_victims() {
    let (mut graph, control) = setup(5);
    graph.observe_write(id(2), point(b"a"), &control).unwrap();
    graph.observe_write(id(3), point(b"b"), &control).unwrap();
    graph.observe_rw(id(2), id(2), id(4), &control).unwrap();
    graph.observe_rw(id(5), id(5), id(1), &control).unwrap();
    finish(&mut graph, &control, 4);
    finish(&mut graph, &control, 3);
    assert!(matches!(
        graph.observe_read(id(1), SerializablePredicate::object(TABLE), &control),
        Err(VersionError::SerializationConflict { .. })
    ));
    assert!(graph.predicates.reads.is_empty());
    assert_eq!(graph.outgoing.len(), 2);
    graph.check_active(id(1)).unwrap();
    graph.check_active(id(2)).unwrap();
    // The first match becomes a peer victim only when its own observation succeeds.
    graph.observe_read(id(1), point(b"a"), &control).unwrap();
    assert!(matches!(
        graph.check_active(id(2)),
        Err(VersionError::SerializationConflict { .. })
    ));
}

#[test]
fn observation_budget_failure_cannot_publish_a_partial_match_set() {
    let mut failures = 0;
    let mut successes = 0;
    for available in [0, 16, 64, 128, 256, 512, 1024, 2048] {
        let (mut graph, control) = setup(3);
        graph.observe_read(id(1), point(b"a"), &control).unwrap();
        graph.observe_read(id(2), point(b"b"), &control).unwrap();
        let occupied = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used() - available)
            .unwrap();
        match graph.observe_write(id(3), SerializablePredicate::object(TABLE), &control) {
            Ok(()) => {
                successes += 1;
                assert_eq!(graph.outgoing.len(), 2);
                assert_eq!(graph.incoming.len(), 2);
                assert_eq!(graph.predicates.writes.len(), 1);
            }
            Err(VersionError::Memory(_)) => {
                failures += 1;
                assert!(graph.outgoing.is_empty() && graph.incoming.is_empty());
                assert!(graph.predicates.writes.is_empty());
                assert_eq!(graph.transactions[2].writes, 0);
            }
            Err(error) => panic!("unexpected observation error: {error}"),
        }
        drop(occupied);
        for value in 1..=3 {
            graph.rollback(id(value)).unwrap();
        }
        graph.reclaim();
        assert_eq!(control.memory().used(), 0);
    }
    assert!(failures > 0 && successes > 0);
}

#[test]
fn duplicate_observations_reuse_budget_and_owned_keys_survive_the_input_buffer() {
    let (mut graph, control) = setup(2);
    let mut input = b"old".to_vec();
    graph.observe_read(id(1), point(&input), &control).unwrap();
    input.copy_from_slice(b"new");
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    graph.observe_read(id(1), point(b"old"), &control).unwrap();
    assert_eq!(graph.predicates.reads.len(), 1);
    drop(occupied);
    graph.observe_write(id(2), point(b"new"), &control).unwrap();
    assert!(!has_edge(&graph, 1, 2));
    graph.observe_write(id(2), point(b"old"), &control).unwrap();
    assert!(has_edge(&graph, 1, 2));
    let mark = graph.write_mark(id(2)).unwrap();
    graph.observe_write(id(2), point(b"old"), &control).unwrap();
    assert_eq!(graph.write_mark(id(2)).unwrap().writes, mark.writes);
}

#[test]
fn observation_admission_rejects_invalid_identities_ranges_and_read_only_writes() {
    let (mut graph, control) = setup(1);
    graph.begin(id(2), true, &control).unwrap();
    for predicate in [
        SerializablePredicate::object([0; 16]),
        SerializablePredicate::point(TABLE, SerializableKeySpace::Index([0; 16]), b"x"),
    ] {
        assert!(matches!(
            graph.observe_read(id(1), predicate, &control),
            Err(VersionError::InvalidEncoding(_))
        ));
    }
    assert!(matches!(
        graph.observe_write(
            id(1),
            SerializablePredicate::range(TABLE, INDEX, Unbounded, Unbounded),
            &control
        ),
        Err(VersionError::InvalidEncoding(_))
    ));
    assert!(matches!(
        graph.observe_write(id(2), point(b"x"), &control),
        Err(VersionError::InvalidEncoding(_))
    ));
    let foreign =
        SerializableTransactionId::new(DatabaseId::from_bytes([99; 16]), COORDINATOR, 1).unwrap();
    assert!(matches!(
        graph.observe_read(foreign, point(b"x"), &control),
        Err(VersionError::WrongDatabase)
    ));
    let mark = graph.write_mark(id(1)).unwrap();
    graph.prepare_commit(id(1), &control).unwrap();
    assert!(matches!(
        graph.rollback_writes(mark),
        Err(VersionError::TransactionSealed)
    ));
    assert!(matches!(
        graph.observe_read(id(1), point(b"x"), &control),
        Err(VersionError::TransactionSealed)
    ));
    graph.commit(id(1)).unwrap();
    assert!(matches!(
        graph.rollback_writes(mark),
        Err(VersionError::TransactionFinished)
    ));
    assert!(graph.predicates.reads.is_empty() && graph.predicates.writes.is_empty());
}
