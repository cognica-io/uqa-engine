//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unchanged publication boundaries retain validated bytes and their original conditions.

use super::*;
use crate::clustered_postings::{encode_occurrence_cluster, OccurrencePosting};
use crate::key_value::occurrence_format::{OccurrenceAddress, OccurrenceProjection};
use crate::mvcc::{commit::RecordWriteKind, resolution::ResolutionMode, *};
use std::sync::Arc;
use uqa_core::{memory::BudgetedVec, TokenOccurrence};

fn keys(control: &StorageReadControl) -> Vec<Vec<u8>> {
    [
        OccurrenceProjection::Score,
        OccurrenceProjection::Positions,
        OccurrenceProjection::Format,
    ]
    .into_iter()
    .map(|projection| {
        let mut address = OccurrenceAddress::table("docs");
        address.projection = Some(projection);
        if projection != OccurrenceProjection::Format {
            address.field = Some("body");
            address.term = Some(b"term");
            address.cluster = Some(0);
        }
        address.encode(control).unwrap().to_vec()
    })
    .collect()
}

fn payload() -> Vec<Vec<u8>> {
    let postings = (0..129)
        .map(|doc_id| OccurrencePosting {
            doc_id,
            doc_length: 1,
            occurrences: vec![TokenOccurrence {
                position: 0,
                position_length: 1,
                offsets: None,
            }],
        })
        .collect::<Vec<_>>();
    let (scores, positions) = encode_occurrence_cluster(&postings).unwrap();
    vec![scores, positions, b"occurrences-v2".to_vec()]
}

fn prepare(
    keys: &[Vec<u8>],
    values: &[Vec<u8>],
    expected: Option<CommitSequence>,
    control: &StorageReadControl,
) -> PreparedRecordCommit {
    let canonical = PreparedRecordCommit::new(
        &keys
            .iter()
            .zip(values)
            .map(|(key, value)| RecordWrite {
                key,
                expected,
                value: Some(value),
            })
            .collect::<Vec<_>>(),
        control,
    )
    .unwrap();
    let mut writes = BudgetedVec::new(control.memory());
    for write in canonical.records() {
        writes
            .push(write.clone().with_kind(RecordWriteKind::Occurrence))
            .unwrap();
    }
    PreparedRecordCommit::from_unique_owned(writes, control).unwrap()
}

#[test]
fn unchanged_publication_shares_validated_cluster_buffers() {
    let control = StorageReadControl::with_limit(1 << 20);
    let store = MemoryVersionStore::new(control.memory());
    let keys = keys(&control);
    let values = payload();
    let base = store.snapshot().unwrap();
    let original = prepare(&keys, &values, None, &control);
    for mode in [ResolutionMode::Command, ResolutionMode::Publication] {
        let resolved = resolve(
            &original,
            &base,
            &base,
            &crate::key_value::KeyValueOccurrenceRecords,
            mode,
            &control,
        )
        .unwrap();
        for key in &keys[..2] {
            let before = original
                .records()
                .iter()
                .find(|row| row.key() == key)
                .unwrap();
            let after = resolved
                .records()
                .iter()
                .find(|row| row.key() == key)
                .unwrap();
            assert!(Arc::ptr_eq(
                &before.shared_value().unwrap(),
                &after.shared_value().unwrap()
            ));
            assert_eq!(after.expected(), before.expected());
            assert_eq!(
                after.kind() == RecordWriteKind::Canonical,
                mode == ResolutionMode::Publication
            );
        }
    }
}

#[test]
fn unchanged_publication_rejects_corrupt_evaluated_and_stored_graphs() {
    for corrupt_base in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let store = MemoryVersionStore::new(control.memory());
        let keys = keys(&control);
        let values = payload();
        let mut corrupt = values.clone();
        corrupt[1].pop();
        let expected = if corrupt_base {
            Some(
                store
                    .commit(
                        &keys
                            .iter()
                            .zip(&corrupt)
                            .map(|(key, value)| RecordWrite {
                                key,
                                expected: None,
                                value: Some(value),
                            })
                            .collect::<Vec<_>>(),
                        &control,
                    )
                    .unwrap(),
            )
        } else {
            None
        };
        let base = store.snapshot().unwrap();
        let original = prepare(
            &keys,
            if corrupt_base { &values } else { &corrupt },
            expected,
            &control,
        );
        assert!(resolve(
            &original,
            &base,
            &base,
            &crate::key_value::KeyValueOccurrenceRecords,
            ResolutionMode::Publication,
            &control
        )
        .is_err());
    }
}
