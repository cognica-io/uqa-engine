//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable record deltas reproduce complete checkpoints without republishing retained predicates.

mod admission;

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::*;
use crate::mvcc::{
    CommitReceipt, CommitSequence, CommitStatus, DatabaseId, SerializableKeySpace,
    SerializablePredicate, StorageTransactionId, VersionError,
};

type Records = BTreeMap<SerializableCheckpointKey, Vec<u8>>;
const DATABASE: DatabaseId = DatabaseId::from_bytes([81; 16]);
const COORDINATOR: [u8; 16] = [82; 16];

fn point(key: &[u8]) -> SerializablePredicate<'_> {
    SerializablePredicate::point([83; 16], SerializableKeySpace::Rows, key)
}

fn encode(graph: &SerializableGraph, control: &StorageReadControl) -> Vec<u8> {
    let mut bytes = Vec::new();
    graph.write_checkpoint(&mut bytes, control).unwrap();
    bytes
}

fn restore(records: &Records, control: &StorageReadControl) -> VersionResult<SerializableGraph> {
    SerializableGraph::read_checkpoint_records(DATABASE, COORDINATOR, control, |visit| {
        for (key, bytes) in records {
            visit(key.as_bytes(), &mut bytes.as_slice())?;
        }
        Ok(())
    })
}

fn publish(
    graph: &SerializableGraph,
    records: &mut Records,
    control: &StorageReadControl,
) -> (usize, usize, usize) {
    let mut counts = (0, 0, 0);
    graph
        .write_checkpoint_changes(control, |key, record| {
            if let Some(record) = record {
                let mut bytes = Vec::new();
                record.write(&mut bytes, control)?;
                assert_eq!(record.encoded_length(control)?, bytes.len() as u64);
                counts.1 += 1;
                counts.2 += bytes.len();
                records.insert(key, bytes);
            } else {
                counts.0 += 1;
                assert!(records.remove(&key).is_some());
            }
            Ok(())
        })
        .unwrap();
    counts
}

fn handoff(
    graph: SerializableGraph,
    records: &mut Records,
    control: &StorageReadControl,
) -> SerializableGraph {
    let expected = encode(&graph, control);
    publish(&graph, records, control);
    drop(graph);
    assert_eq!(control.memory().used(), 0);
    let graph = restore(records, control).unwrap();
    assert_eq!(encode(&graph, control), expected);
    assert!(!graph.checkpoint_records_changed());
    graph
}

#[test]
fn new_reads_publish_only_the_header_and_new_predicate_regardless_of_retained_history() {
    let mut expected = None;
    for retained in [1, 128] {
        let control = StorageReadControl::with_limit(4 << 20);
        let mut graph = SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap();
        let actor = graph.admit(false, &control).unwrap();
        for index in 0_u64..retained {
            let mut key = [3; 1024];
            key[..8].copy_from_slice(&index.to_be_bytes());
            graph.observe_read(actor, point(&key), &control).unwrap();
        }
        let mut records = Records::new();
        let mut graph = handoff(graph, &mut records, &control);
        graph.check_active(actor).unwrap();
        graph.reclaim();
        assert_eq!(publish(&graph, &mut records, &control), (0, 0, 0));
        graph
            .observe_read(actor, point(&[9; 1024]), &control)
            .unwrap();
        let counts = publish(&graph, &mut records, &control);
        assert_eq!((counts.0, counts.1), (0, 2));
        if let Some(expected) = expected {
            assert_eq!(counts, expected);
        }
        expected = Some(counts);
        drop(graph);
        let mut graph = restore(&records, &control).unwrap();
        graph
            .observe_read(actor, point(&[9; 1024]), &control)
            .unwrap();
        assert_eq!(publish(&graph, &mut records, &control), (0, 0, 0));
    }
}

#[test]
fn publication_reserves_the_next_admissions_key_inventory_before_writing() {
    let control = StorageReadControl::with_limit(128 << 10);
    let mut graph = SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap();
    let actor = graph.admit(false, &control).unwrap();
    graph.observe_read(actor, point(b"old"), &control).unwrap();
    let mut records = Records::new();
    let mut graph = handoff(graph, &mut records, &control);
    graph.observe_read(actor, point(b"new"), &control).unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    assert!(matches!(
        graph.write_checkpoint_changes(&control, |_, _| panic!(
            "published without room to restore the new inventory"
        )),
        Err(VersionError::Memory(_))
    ));
    assert!(graph.checkpoint_records_changed());
    drop(occupied);
    let graph = handoff(graph, &mut records, &control);
    graph.check_active(actor).unwrap();
}

#[test]
fn partial_recovery_and_savepoint_deletions_survive_incremental_handoff() {
    let control = StorageReadControl::with_limit(128 << 10);
    let mut graph = SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap();
    let a = graph.admit(false, &control).unwrap();
    let b = graph.admit(false, &control).unwrap();
    let mark = graph.write_mark(a).unwrap();
    graph.observe_write(a, point(b"undone"), &control).unwrap();
    graph.observe_read(a, point(b"kept"), &control).unwrap();
    let mut records = Records::new();
    let mut graph = handoff(graph, &mut records, &control);
    graph.rollback_writes(mark).unwrap();
    let mut graph = handoff(graph, &mut records, &control);
    assert_eq!(graph.predicates.reads.len(), 1);
    assert!(graph.predicates.writes.is_empty());
    let physical = StorageTransactionId::new(DATABASE, 41).unwrap();
    let publication = graph
        .prepare_publication(a, physical, [4; 32], &control)
        .unwrap();
    graph
        .prepare_publication(
            b,
            StorageTransactionId::new(DATABASE, 42).unwrap(),
            [5; 32],
            &control,
        )
        .unwrap();
    let mut graph = handoff(graph, &mut records, &control);
    let committed = CommitStatus::Committed(CommitReceipt {
        transaction: physical,
        sequence: CommitSequence::from_u64(1),
        fingerprint: [4; 32],
    });
    assert!(matches!(
        graph.reconcile_publications(&control, |id| Ok(if id == physical {
            committed
        } else {
            CommitStatus::Unknown
        })),
        Err(VersionError::UnknownTransaction)
    ));
    let mut graph = handoff(graph, &mut records, &control);
    assert_eq!(
        graph
            .resolve_publication(publication, CommitStatus::Unknown)
            .unwrap(),
        committed
    );
    graph
        .reconcile_publications(&control, |_| Ok(CommitStatus::Aborted))
        .unwrap();
    graph.reclaim();
    let graph = handoff(graph, &mut records, &control);
    assert_eq!(records.len(), 1);
    assert!(graph.transactions.is_empty());
}

#[test]
fn invalid_record_sets_never_escape_restore_or_consume_retained_allowances() {
    let control = StorageReadControl::with_limit(128 << 10);
    let mut graph = SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap();
    let actor = graph.admit(false, &control).unwrap();
    graph
        .observe_read(actor, point(b"retained"), &control)
        .unwrap();
    let mut records = Records::new();
    publish(&graph, &mut records, &control);
    drop(graph);
    for corruption in 0..5 {
        let mut invalid = records.clone();
        let key = *invalid.keys().last().unwrap();
        match corruption {
            0 => {
                invalid.remove(&key);
            }
            1 => {
                invalid.remove(&SerializableCheckpointKey::HEADER);
            }
            2 => {
                invalid.get_mut(&key).unwrap()[0] ^= 1;
            }
            3 => {
                invalid.get_mut(&key).unwrap().push(0);
            }
            _ => {
                let bytes = invalid.remove(&key).unwrap();
                invalid.insert(
                    SerializableCheckpointKey::predicate([84; 16], [5; 32], false),
                    bytes,
                );
            }
        }
        assert!(restore(&invalid, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let tiny = StorageReadControl::with_limit(1);
    assert!(matches!(
        restore(&records, &tiny),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    control.cancellation().cancel();
    let error = restore(&records, &control)
        .err()
        .expect("cancelled checkpoint restore")
        .into_storage_error();
    assert!(
        matches!(&error, crate::StorageBackendError::Cancelled(_)),
        "{error}"
    );
    control.cancellation().reset();
    let graph = restore(&records, &control).unwrap();
    graph.check_active(actor).unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn deltas_match_complete_checkpoints_through_dependency_and_completion_histories(
        operations in prop::collection::vec((0u8..9, 0usize..3, 0u8..8), 1..80)
    ) {
        let control = StorageReadControl::with_limit(128 << 10);
        let mut graph = SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap();
        let mut actors = [graph.admit(true, &control).unwrap(), graph.admit(false, &control).unwrap(), graph.admit(false, &control).unwrap()];
        let mut records = Records::new();
        graph = handoff(graph, &mut records, &control);
        for (operation, index, key) in operations {
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
                _ => graph.admit(index == 0, &control).map(|id| actors[index] = id),
            };
            graph = handoff(graph, &mut records, &control);
        }
    }
}
