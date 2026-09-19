//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sealed vector effects cannot reach physical publication before resolution.

use super::*;
use crate::{
    mvcc::{CommitSequence, PreparedRecordCommit, RecordWrite},
    read_control::StorageReadControl,
};

#[test]
fn canonical_only_records_cannot_hide_unresolved_vector_effects() {
    let control = StorageReadControl::with_limit(1 << 20);
    for kind in [IndexKind::IVFIndex, IndexKind::HNSWIndex] {
        let input =
            OwnedVectorMutation::retain(kind, b"header", Mutation::Delete(9), &control).unwrap();
        let prepared = PreparedRecordCommit::new(
            &[RecordWrite {
                key: b"header",
                expected: None,
                value: Some(b"value"),
            }],
            &control,
        )
        .unwrap()
        .with_vector_effects(CommitSequence::INITIAL, &[input], &control)
        .unwrap();
        assert!(matches!(
            prepared.validate_snapshot(CommitSequence::INITIAL),
            Err(VersionError::InvalidEncoding(
                "unresolved storage commit effects"
            ))
        ));
        let mut called = false;
        assert!(matches!(
            prepared.validate(control.cancellation(), |_| {
                called = true;
                Ok(None)
            }),
            Err(VersionError::InvalidEncoding(
                "unresolved storage commit effects"
            ))
        ));
        assert!(!called);
        drop(prepared);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn fingerprints_seal_algorithm_order_and_exact_tensor_bits() {
    let control = StorageReadControl::with_limit(1 << 20);
    let fingerprint = |kind, first: &[Vec<f32>], second: &[Vec<f32>]| {
        let inputs = [first, second].map(|vectors| {
            OwnedVectorMutation::retain(
                kind,
                b"header",
                Mutation::Replace {
                    document: 9,
                    vectors,
                },
                &control,
            )
            .unwrap()
        });
        PreparedRecordCommit::new(&[], &control)
            .unwrap()
            .with_vector_effects(CommitSequence::INITIAL, &inputs, &control)
            .unwrap()
            .fingerprint()
    };
    let first = [vec![1.0, 0.0]];
    let second = [vec![1.0, -0.0]];
    let original = fingerprint(IndexKind::IVFIndex, &first, &second);
    assert_ne!(original, fingerprint(IndexKind::HNSWIndex, &first, &second));
    assert_ne!(original, fingerprint(IndexKind::IVFIndex, &second, &first));
    assert_ne!(original, fingerprint(IndexKind::IVFIndex, &first, &first));
    assert_eq!(control.memory().used(), 0);
}
