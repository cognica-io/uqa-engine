//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Handoff preserves decisions and private observation marks; corrupt or incomplete state never escapes decoding.

use std::ops::Bound::{Excluded, Included};

use sha2::{Digest, Sha256};

use super::*;
use crate::mvcc::{
    CommitReceipt, CommitSequence, CommitStatus, SafeSnapshot, SerializableKeySpace,
    SerializablePredicate, SerializableTransactionId, StorageTransactionId,
};

const DATABASE: DatabaseId = DatabaseId::from_bytes([61; 16]);
const COORDINATOR: [u8; 16] = [62; 16];
const TABLE: [u8; 16] = [63; 16];

fn setup() -> (SerializableGraph, StorageReadControl) {
    let control = StorageReadControl::with_limit(128 * 1024);
    let graph = SerializableGraph::new(DATABASE, COORDINATOR, control.memory()).unwrap();
    (graph, control)
}

fn point(key: &[u8]) -> SerializablePredicate<'_> {
    SerializablePredicate::point(TABLE, SerializableKeySpace::Rows, key)
}

fn encode(graph: &SerializableGraph, control: &StorageReadControl) -> Vec<u8> {
    let mut bytes = Vec::new();
    graph.write_checkpoint(&mut bytes, control).unwrap();
    assert_eq!(
        graph.checkpoint_length(control).unwrap(),
        bytes.len() as u64
    );
    bytes
}

fn handoff(graph: SerializableGraph, control: &StorageReadControl) -> SerializableGraph {
    let bytes = encode(&graph, control);
    drop(graph);
    assert_eq!(control.memory().used(), 0);
    let restored =
        SerializableGraph::read_checkpoint(DATABASE, COORDINATOR, &mut bytes.as_slice(), control)
            .unwrap();
    assert_eq!(encode(&restored, control), bytes);
    restored
}

fn finish(
    graph: &mut SerializableGraph,
    participant: SerializableTransactionId,
    control: &StorageReadControl,
) {
    graph.prepare_commit(participant, control).unwrap();
    graph.commit(participant).unwrap();
}

#[test]
fn read_only_maintenance_keeps_physical_receipts_without_allowing_logical_writes() {
    let (mut graph, control) = setup();
    let reader = graph.admit(true, &control).unwrap();
    assert!(graph
        .observe_write(reader, point(b"user_row"), &control)
        .is_err());
    let physical = StorageTransactionId::new(DATABASE, 1).unwrap();
    let publication = graph
        .prepare_publication(reader, physical, [8; 32], &control)
        .unwrap();
    let encoded = encode(&graph, &control);
    for magic in [b"UQASER01", b"UQASER02"] {
        let mut legacy = encoded.clone();
        legacy[..8].copy_from_slice(magic);
        let end = legacy.len() - 32;
        let digest: [u8; 32] = Sha256::digest(&legacy[..end]).into();
        legacy[end..].copy_from_slice(&digest);
        assert!(SerializableGraph::read_checkpoint(
            DATABASE,
            COORDINATOR,
            &mut legacy.as_slice(),
            &control
        )
        .is_err());
    }
    let mut graph = handoff(graph, &control);
    assert_eq!(graph.publication(reader).unwrap(), Some(publication));
    let receipt = CommitReceipt {
        transaction: physical,
        sequence: CommitSequence::from_u64(1),
        fingerprint: [8; 32],
    };
    graph
        .reconcile_publications(&control, |_| Ok(CommitStatus::Committed(receipt)))
        .unwrap();
    let graph = handoff(graph, &control);
    assert_eq!(
        graph.status(reader).unwrap(),
        crate::mvcc::SerializableStatus::Committed
    );
}

#[test]
fn earlier_checkpoint_versions_preserve_manual_and_leased_participants() {
    for magic in [b"UQASER01", b"UQASER02"] {
        let (mut graph, control) = setup();
        let leases =
            std::sync::Arc::new(crate::mvcc::LocalSerializableLeases::new(control.memory()));
        let participant = (magic == b"UQASER02").then(|| {
            graph
                .admit_with_lease(true, &control, |id| leases.retain(id, &control))
                .unwrap()
        });
        let id = participant.as_ref().map_or_else(
            || graph.admit(true, &control).unwrap(),
            crate::mvcc::SerializableParticipant::id,
        );
        let mut encoded = encode(&graph, &control);
        encoded[..8].copy_from_slice(magic);
        let end = encoded.len() - 32;
        let digest: [u8; 32] = Sha256::digest(&encoded[..end]).into();
        encoded[end..].copy_from_slice(&digest);
        let restored = SerializableGraph::read_checkpoint(
            DATABASE,
            COORDINATOR,
            &mut encoded.as_slice(),
            &control,
        )
        .unwrap();
        restored.check_active(id).unwrap();
        assert_eq!(&encode(&restored, &control)[..8], b"UQASER03");
    }
}

#[test]
fn independent_owners_keep_phantom_dependencies_and_both_commit_orders() {
    for first_wins in [false, true] {
        let (mut graph, control) = setup();
        let first = graph.admit(false, &control).unwrap();
        let second = graph.admit(false, &control).unwrap();
        let predicate = SerializablePredicate::range(
            TABLE,
            SerializableKeySpace::Index([64; 16]),
            Included(b"a"),
            Excluded(b"z"),
        );
        graph.observe_read(first, predicate, &control).unwrap();
        graph.observe_read(second, predicate, &control).unwrap();
        graph
            .observe_write(
                first,
                SerializablePredicate::point(TABLE, SerializableKeySpace::Index([64; 16]), b"b"),
                &control,
            )
            .unwrap();
        let mut graph = handoff(graph, &control);
        graph
            .observe_write(
                second,
                SerializablePredicate::point(TABLE, SerializableKeySpace::Index([64; 16]), b"y"),
                &control,
            )
            .unwrap();
        let mut graph = handoff(graph, &control);
        let (winner, victim) = if first_wins {
            (first, second)
        } else {
            (second, first)
        };
        finish(&mut graph, winner, &control);
        assert!(matches!(
            graph.check_active(victim),
            Err(VersionError::SerializationConflict { .. })
        ));
        let mut graph = handoff(graph, &control);
        assert!(matches!(
            graph.prepare_commit(victim, &control),
            Err(VersionError::SerializationConflict { .. })
        ));
        graph.rollback(victim).unwrap();
        graph.reclaim();
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn savepoint_marks_remove_only_cancelled_intents_after_handoff() {
    let (mut graph, control) = setup();
    let first = graph.admit(false, &control).unwrap();
    let second = graph.admit(false, &control).unwrap();
    graph.observe_read(first, point(b"read"), &control).unwrap();
    let mark = graph.write_mark(first).unwrap();
    graph
        .observe_write(first, point(b"cancelled"), &control)
        .unwrap();
    let mut graph = handoff(graph, &control);
    graph.rollback_writes(mark).unwrap();
    graph
        .observe_read(second, point(b"cancelled"), &control)
        .unwrap();
    assert!(graph.outgoing.is_empty());
    graph
        .observe_write(second, point(b"read"), &control)
        .unwrap();
    assert!(graph
        .outgoing
        .iter()
        .any(|edge| edge.0 == first.allocation() && edge.1 == second.allocation()));
    finish(&mut graph, second, &control);
    finish(&mut graph, first, &control);
}

#[test]
fn vector_candidate_ranges_and_intents_survive_handoff_without_aliasing_rows() {
    let (mut graph, control) = setup();
    let first = graph.admit(false, &control).unwrap();
    let second = graph.admit(false, &control).unwrap();
    let range = SerializablePredicate::range(
        TABLE,
        SerializableKeySpace::Vectors,
        Included(b"a"),
        Included(b"z"),
    );
    graph.observe_read(first, range, &control).unwrap();
    graph.observe_write(second, point(b"b"), &control).unwrap();
    assert!(graph.outgoing.is_empty());
    graph.observe_read(second, range, &control).unwrap();
    graph
        .observe_write(
            first,
            SerializablePredicate::point(TABLE, SerializableKeySpace::Vectors, b"b"),
            &control,
        )
        .unwrap();
    let mut graph = handoff(graph, &control);
    graph
        .observe_write(
            second,
            SerializablePredicate::point(TABLE, SerializableKeySpace::Vectors, b"c"),
            &control,
        )
        .unwrap();
    let mut graph = handoff(graph, &control);
    finish(&mut graph, first, &control);
    assert!(matches!(
        graph.prepare_commit(second, &control),
        Err(VersionError::SerializationConflict { .. })
    ));
}

#[test]
fn text_and_graph_ranges_survive_handoff_without_aliasing_other_access_paths() {
    for selected in [SerializableKeySpace::Text, SerializableKeySpace::Graph] {
        for other in [
            SerializableKeySpace::Rows,
            SerializableKeySpace::Index([64; 16]),
            SerializableKeySpace::Vectors,
            SerializableKeySpace::Text,
            SerializableKeySpace::Graph,
        ] {
            if selected == other {
                continue;
            }
            let (mut graph, control) = setup();
            let first = graph.admit(false, &control).unwrap();
            let second = graph.admit(false, &control).unwrap();
            let range =
                SerializablePredicate::range(TABLE, selected, Included(b"term"), Excluded(b"tern"));
            graph.observe_read(first, range, &control).unwrap();
            graph
                .observe_write(
                    second,
                    SerializablePredicate::point(TABLE, other, b"term/doc"),
                    &control,
                )
                .unwrap();
            assert!(graph.outgoing.is_empty());
            graph.observe_read(second, range, &control).unwrap();
            graph
                .observe_write(
                    first,
                    SerializablePredicate::point(TABLE, selected, b"term/doc"),
                    &control,
                )
                .unwrap();
            let mut graph = handoff(graph, &control);
            graph
                .observe_write(
                    second,
                    SerializablePredicate::point(TABLE, selected, b"term/phantom"),
                    &control,
                )
                .unwrap();
            let mut graph = handoff(graph, &control);
            finish(&mut graph, first, &control);
            assert!(matches!(
                graph.prepare_commit(second, &control),
                Err(VersionError::SerializationConflict { .. })
            ));
        }
    }
}

#[test]
fn prepared_receipts_and_terminal_precedence_survive_a_new_state_owner() {
    for committed in [false, true] {
        let (mut graph, control) = setup();
        let writer = graph.admit(false, &control).unwrap();
        let reader = graph.admit(true, &control).unwrap();
        graph
            .observe_write(writer, point(b"published"), &control)
            .unwrap();
        let publication = graph
            .prepare_publication(
                writer,
                StorageTransactionId::new(DATABASE, 71).unwrap(),
                [72; 32],
                &control,
            )
            .unwrap();
        let mut graph = handoff(graph, &control);
        assert_eq!(graph.publication(writer).unwrap(), Some(publication));
        assert_eq!(
            graph.safe_snapshot(reader, &control).unwrap(),
            SafeSnapshot::Pending
        );
        assert!(matches!(
            graph.rollback(writer),
            Err(VersionError::InvalidEncoding(_))
        ));
        let status = if committed {
            CommitStatus::Committed(CommitReceipt {
                transaction: publication.transaction(),
                sequence: CommitSequence::from_u64(5),
                fingerprint: publication.fingerprint(),
            })
        } else {
            CommitStatus::Aborted
        };
        graph
            .reconcile_publications(&control, |physical| {
                assert_eq!(physical, publication.transaction());
                Ok(status)
            })
            .unwrap();
        let mut graph = handoff(graph, &control);
        assert_eq!(
            graph
                .resolve_publication(publication, CommitStatus::Unknown)
                .unwrap(),
            status
        );
        let next = graph.admit(true, &control).unwrap();
        assert_eq!(next.allocation(), 3);
    }
}

#[test]
fn reclaimed_conflict_summaries_keep_read_only_anomalies_visible() {
    let (mut graph, control) = setup();
    let pivot = graph.admit(false, &control).unwrap();
    let sink = graph.admit(false, &control).unwrap();
    graph.observe_rw(pivot, pivot, sink, &control).unwrap();
    finish(&mut graph, sink, &control);
    let reader = graph.admit(true, &control).unwrap();
    finish(&mut graph, pivot, &control);
    graph.reclaim();
    let mut graph = handoff(graph, &control);
    assert_eq!(
        graph.safe_snapshot(reader, &control).unwrap(),
        SafeSnapshot::Unsafe
    );
    assert!(matches!(
        graph.observe_rw(reader, reader, pivot, &control),
        Err(VersionError::SerializationConflict { .. })
    ));
}

#[test]
fn quiescent_checkpoints_preserve_the_incarnation_and_allocation_watermark() {
    let (mut graph, control) = setup();
    let old = graph.admit(true, &control).unwrap();
    finish(&mut graph, old, &control);
    graph.reclaim();
    let bytes = encode(&graph, &control);
    assert!(matches!(
        SerializableGraph::read_checkpoint(
            DatabaseId::from_bytes([99; 16]),
            COORDINATOR,
            &mut bytes.as_slice(),
            &control
        ),
        Err(VersionError::WrongDatabase)
    ));
    assert!(matches!(
        SerializableGraph::read_checkpoint(DATABASE, [99; 16], &mut bytes.as_slice(), &control),
        Err(VersionError::WrongSerializableCoordinator)
    ));
    let mut graph = handoff(graph, &control);
    assert_eq!(
        graph.admit(true, &control).unwrap().allocation(),
        old.allocation() + 1
    );
}

#[test]
fn truncation_bad_checksum_and_trailing_data_never_return_partial_state() {
    let (mut graph, control) = setup();
    let reader = graph.admit(true, &control).unwrap();
    graph
        .observe_read(reader, point(b"protected"), &control)
        .unwrap();
    let bytes = encode(&graph, &control);
    let decode = StorageReadControl::with_limit(128 * 1024);
    for length in 0..bytes.len() {
        assert!(SerializableGraph::read_checkpoint(
            DATABASE,
            COORDINATOR,
            &mut &bytes[..length],
            &decode
        )
        .is_err());
        assert_eq!(decode.memory().used(), 0);
    }
    let mut broken = bytes.clone();
    *broken.last_mut().unwrap() ^= 1;
    assert!(SerializableGraph::read_checkpoint(
        DATABASE,
        COORDINATOR,
        &mut broken.as_slice(),
        &decode
    )
    .is_err());
    let mut extra = bytes;
    extra.push(0);
    assert!(SerializableGraph::read_checkpoint(
        DATABASE,
        COORDINATOR,
        &mut extra.as_slice(),
        &decode
    )
    .is_err());
    assert_eq!(decode.memory().used(), 0);
}

#[test]
fn invalid_order_and_dangling_dependencies_are_rejected_even_with_a_valid_checksum() {
    let (mut graph, control) = setup();
    let first = graph.admit(false, &control).unwrap();
    let second = graph.admit(false, &control).unwrap();
    graph.observe_rw(first, first, second, &control).unwrap();
    let bytes = encode(&graph, &control);
    // The checkpoint header precedes two fixed participant records without publication bindings.
    let header = 8 + 16 + 16 + 3 * 8 + 4 * 8;
    let node = 6 * 8 + 2;
    for (offset, value) in [
        (header + 8, 0_u64),
        (header + node, first.allocation()),
        (header + 2 * node, 999),
        (56, 1),
    ] {
        let mut broken = bytes.clone();
        broken[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
        let end = broken.len() - 32;
        let digest: [u8; 32] = Sha256::digest(&broken[..end]).into();
        broken[end..].copy_from_slice(&digest);
        let decode = StorageReadControl::with_limit(128 * 1024);
        assert!(matches!(
            SerializableGraph::read_checkpoint(
                DATABASE,
                COORDINATOR,
                &mut broken.as_slice(),
                &decode
            ),
            Err(VersionError::InvalidEncoding(_))
        ));
        assert_eq!(decode.memory().used(), 0);
    }
}

struct ShortReads<'a>(&'a [u8]);

impl Read for ShortReads<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let length = output.len().min(3).min(self.0.len());
        output[..length].copy_from_slice(&self.0[..length]);
        self.0 = &self.0[length..];
        Ok(length)
    }
}

#[test]
fn streamed_keys_respect_resource_limits_without_an_encoded_state_copy() {
    let (mut graph, control) = setup();
    let reader = graph.admit(true, &control).unwrap();
    let key = vec![b'k'; 20 * 1024];
    graph.observe_read(reader, point(&key), &control).unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    let bytes = encode(&graph, &control);
    drop(occupied);
    let limited = StorageReadControl::with_limit(16 * 1024);
    assert!(matches!(
        SerializableGraph::read_checkpoint(DATABASE, COORDINATOR, &mut bytes.as_slice(), &limited),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(limited.memory().used(), 0);
    let decode = StorageReadControl::with_limit(128 * 1024);
    let restored =
        SerializableGraph::read_checkpoint(DATABASE, COORDINATOR, &mut ShortReads(&bytes), &decode)
            .unwrap();
    assert_eq!(encode(&restored, &decode), bytes);
    drop(restored);
    assert_eq!(decode.memory().used(), 0);
}

#[test]
fn cancellation_stops_streaming_without_changing_the_live_graph() {
    let (mut graph, control) = setup();
    let actor = graph.admit(false, &control).unwrap();
    let bytes = encode(&graph, &control);
    control.cancellation().cancel();
    let mut output = Vec::new();
    assert!(graph.write_checkpoint(&mut output, &control).is_err());
    assert!(output.is_empty());
    graph.check_active(actor).unwrap();
    let decode = StorageReadControl::with_limit(128 * 1024);
    decode.cancellation().cancel();
    assert!(SerializableGraph::read_checkpoint(
        DATABASE,
        COORDINATOR,
        &mut bytes.as_slice(),
        &decode
    )
    .is_err());
    assert_eq!(decode.memory().used(), 0);
    graph.rollback(actor).unwrap();
    graph.reclaim();
    assert_eq!(control.memory().used(), 0);
}
