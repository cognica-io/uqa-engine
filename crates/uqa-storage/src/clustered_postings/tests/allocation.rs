//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, MemoryError},
    QueryCancelled, TokenOffsets,
};

mod reference;

fn graph_fixture(cluster: u64, count: usize) -> Vec<OccurrencePosting> {
    let base = cluster_base(cluster).unwrap();
    (0..count)
        .map(|index| OccurrencePosting {
            doc_id: base
                + if index + 1 == count {
                    65_535
                } else {
                    (index * 2) as u64
                },
            doc_length: 1,
            occurrences: (0..=index % 4)
                .map(|ordinal| TokenOccurrence {
                    position: (ordinal / 2) as u32,
                    position_length: 1 + (ordinal % 3) as u32,
                    offsets: (ordinal % 2 == 0).then_some(TokenOffsets {
                        start_utf8: 3,
                        end_utf8: 7,
                        start_utf16: 3,
                        end_utf16: 4,
                    }),
                })
                .collect(),
        })
        .collect()
}

fn compare_decoders(cluster: u64, scores: &[u8], positions: &[u8]) {
    let expected = reference::decode_occurrence_cluster(cluster, scores, positions);
    let budget = MemoryBudget::new(1 << 24);
    let unrelated = budget.reserve(7).unwrap();
    let actual = decode_occurrence_cluster_budgeted(cluster, scores, positions, &budget, || Ok(()));
    match (&expected, actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(actual.as_slice(), expected),
        (Err(_), Err(_)) => {}
        (expected, actual) => {
            panic!("decoder acceptance changed: expected={expected:?}, actual={actual:?}")
        }
    }
    assert_eq!(budget.used(), 7);
    for target in [
        cluster_base(cluster).unwrap(),
        cluster_base(cluster).unwrap() + 1,
        cluster_base(cluster).unwrap() + 65_535,
    ] {
        let actual = decode_occurrence_document_budgeted(
            cluster,
            scores,
            positions,
            target,
            &budget,
            || Ok(()),
        );
        match (&expected, actual) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(actual.as_deref(), expected.iter().find(|posting| posting.doc_id == target));
            }
            (Err(_), Err(_)) => {},
            (expected, actual) => panic!("selected decoder acceptance changed for {target}: expected={expected:?}, actual={actual:?}"),
        }
        assert_eq!(budget.used(), 7);
    }
    drop(unrelated);
    assert_eq!(budget.used(), 0);
}

#[test]
fn borrowed_decoders_preserve_complete_format_and_corruption_acceptance() {
    for cluster in [0, 17, cluster_id(DocId::MAX)] {
        for count in [1, 2, 127, 128, 129, 260] {
            let entries = graph_fixture(cluster, count);
            let (scores, positions) = encode_occurrence_cluster(&entries).unwrap();
            assert_eq!(
                reference::decode_occurrence_cluster(cluster, &scores, &positions).unwrap(),
                entries
            );
            compare_decoders(cluster, &scores, &positions);
            for (which, original) in [(0, &scores), (1, &positions)] {
                for at in 0..original.len() {
                    if count > 2 && at >= 80 && at % 127 != 0 && at + 4 < original.len() {
                        continue;
                    }
                    let mut bytes = original.clone();
                    bytes[at] ^= 0x80;
                    let (score, position) = if which == 0 {
                        (bytes.as_slice(), positions.as_slice())
                    } else {
                        (scores.as_slice(), bytes.as_slice())
                    };
                    compare_decoders(cluster, score, position);
                    let (score, position) = if which == 0 {
                        (&scores[..at], positions.as_slice())
                    } else {
                        (scores.as_slice(), &positions[..at])
                    };
                    compare_decoders(cluster, score, position);
                }
            }
        }
    }
}

#[test]
fn selected_reads_validate_other_documents_with_only_selected_output_memory() {
    let entries = graph_fixture(cluster_id(DocId::MAX), 260);
    let (scores, positions) = encode_occurrence_cluster(&entries).unwrap();
    let bytes = entries[0].occurrences.len() * size_of::<TokenOccurrence>();
    let budget = MemoryBudget::new(bytes + 7);
    let unrelated = budget.reserve(7).unwrap();
    let selected = decode_occurrence_document_budgeted(
        cluster_id(DocId::MAX),
        &scores,
        &positions,
        entries[0].doc_id,
        &budget,
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(*selected, entries[0]);
    assert_eq!(budget.used(), bytes + 7);
    assert_eq!(budget.peak(), bytes + 7);
    let missing = decode_occurrence_document_budgeted(
        cluster_id(DocId::MAX),
        &scores,
        &positions,
        entries[0].doc_id + 1,
        &budget,
        || Ok(()),
    )
    .unwrap();
    assert!(missing.is_none());
    let mut corrupt_positions = positions.clone();
    let offset_count = read_u32(&positions, 12).unwrap() as usize;
    let payload_start = HEADER_LEN + offset_count * 4;
    let final_start = read_u32(&positions, HEADER_LEN + (entries.len() - 1) * 4).unwrap() as usize;
    corrupt_positions[payload_start + final_start + 2] = 2;
    assert!(reference::decode_occurrence_cluster(
        cluster_id(DocId::MAX),
        &scores,
        &corrupt_positions
    )
    .is_err());
    assert!(decode_occurrence_document_budgeted(
        cluster_id(DocId::MAX),
        &scores,
        &corrupt_positions,
        entries[0].doc_id + 1,
        &budget,
        || Ok(())
    )
    .is_err());
    assert_eq!(budget.used(), bytes + 7);
    drop(selected);
    assert_eq!(budget.used(), 7);
    drop(unrelated);
}

#[test]
fn decoder_limits_and_every_callback_failure_release_only_their_output() {
    let entries = graph_fixture(0, 4);
    let (scores, positions) = encode_occurrence_cluster(&entries).unwrap();
    let baseline = MemoryBudget::new(1 << 20);
    let mut calls = 0;
    let reference = decode_occurrence_cluster_budgeted(0, &scores, &positions, &baseline, || {
        calls += 1;
        Ok(())
    })
    .unwrap();
    let required = baseline.peak();
    assert_eq!(required, reference.reserved_bytes());
    for limit in 0..=required {
        let budget = MemoryBudget::new(limit + 7);
        let unrelated = budget.reserve(7).unwrap();
        match decode_occurrence_cluster_budgeted(0, &scores, &positions, &budget, || Ok(())) {
            Ok(actual) => {
                assert_eq!(*actual, *reference);
                assert_eq!(limit, required);
            }
            Err(StorageBackendError::Memory(MemoryError::Limit { .. })) => {
                assert!(limit < required);
            }
            result => panic!("unexpected allocation result at {limit}: {result:?}"),
        }
        assert_eq!(budget.used(), 7);
        drop(unrelated);
    }
    let budget = MemoryBudget::new(1 << 20);
    let prior = decode_occurrence_document_budgeted(
        0,
        &scores,
        &positions,
        entries[0].doc_id,
        &budget,
        || Ok(()),
    )
    .unwrap()
    .unwrap();
    let retained = budget.used();
    for stop in 1..=calls {
        let mut current = 0;
        assert!(matches!(
            decode_occurrence_cluster_budgeted(0, &scores, &positions, &budget, || {
                current += 1;
                if current == stop {
                    Err(QueryCancelled.into())
                } else {
                    Ok(())
                }
            }),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(budget.used(), retained);
        assert_eq!(*prior, entries[0]);
    }
    for target in [
        entries[0].doc_id,
        entries[0].doc_id + 1,
        entries.last().unwrap().doc_id,
    ] {
        let mut calls = 0;
        let output =
            decode_occurrence_document_budgeted(0, &scores, &positions, target, &budget, || {
                calls += 1;
                Ok(())
            })
            .unwrap();
        drop(output);
        for stop in 1..=calls {
            let mut current = 0;
            assert!(matches!(
                decode_occurrence_document_budgeted(
                    0,
                    &scores,
                    &positions,
                    target,
                    &budget,
                    || {
                        current += 1;
                        if current == stop {
                            Err(QueryCancelled.into())
                        } else {
                            Ok(())
                        }
                    }
                ),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(budget.used(), retained);
        }
    }
    drop(prior);
    assert_eq!(budget.used(), 0);
}
