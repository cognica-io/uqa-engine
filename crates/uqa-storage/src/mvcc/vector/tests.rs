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
        let mut inputs = VectorInputs::new(control.memory());
        inputs.push(&input, &control).unwrap();
        drop(input);
        let prepared = PreparedRecordCommit::new(
            &[RecordWrite {
                key: b"header",
                expected: None,
                value: Some(b"value"),
            }],
            &control,
        )
        .unwrap()
        .with_vector_effects(CommitSequence::INITIAL, &inputs, &control)
        .unwrap();
        assert!(matches!(
            prepared.validate_snapshot(CommitSequence::INITIAL),
            Err(VersionError::InvalidEncoding(
                "unresolved storage commit effects"
            ))
        ));
        let mut called = false;
        assert!(matches!(
            prepared.validate(&control, |_| {
                called = true;
                Ok(None)
            }),
            Err(VersionError::InvalidEncoding(
                "unresolved storage commit effects"
            ))
        ));
        assert!(!called);
        drop(prepared);
        drop(inputs);
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
        let mut journal = VectorInputs::new(control.memory());
        for input in &inputs {
            journal.push(input, &control).unwrap();
        }
        PreparedRecordCommit::new(&[], &control)
            .unwrap()
            .with_vector_effects(CommitSequence::INITIAL, &journal, &control)
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

#[test]
fn spilled_vector_inputs_preserve_order_bits_prefixes_and_fingerprints() {
    use crate::spill_map::Record;
    let bounded = StorageReadControl::with_limit(128 << 10);
    let resident = StorageReadControl::with_limit(16 << 20);
    let mut spilled = VectorInputs::new(bounded.memory());
    let mut memory = VectorInputs::new(resident.memory());
    let mut prefix = None;
    for position in 0..96 {
        let mut vector = vec![position as f32; 512];
        vector[0] = -0.0;
        vector[1] = f32::from_bits(1);
        let tensors = [vector];
        let mutation = if position % 5 == 0 {
            Mutation::Delete(position % 7)
        } else {
            Mutation::Replace {
                document: position % 7,
                vectors: if position % 7 == 0 { &[] } else { &tensors },
            }
        };
        let input = OwnedVectorMutation::retain(
            IndexKind::HNSWIndex,
            b"field\0header",
            mutation,
            &resident,
        )
        .unwrap();
        memory.push(&input, &resident).unwrap();
        spilled.push(&input, &bounded).unwrap();
        if position == 31 {
            prefix = Some(spilled.clone());
        }
        let stored = spilled.get(position).unwrap();
        let mut encoded = Vec::new();
        input.encode(&mut encoded);
        let mut actual = Vec::new();
        stored.encode(&mut actual);
        assert_eq!(actual, encoded);
        assert_eq!(actual.len(), stored.encoded_bytes().unwrap());
        assert!(OwnedVectorMutation::decode(&actual[..actual.len() - 1]).is_err());
        actual.push(0);
        assert!(OwnedVectorMutation::decode(&actual).is_err());
    }
    let fingerprint = |inputs: &VectorInputs, control: &StorageReadControl| {
        PreparedRecordCommit::new(&[], control)
            .unwrap()
            .with_vector_effects(CommitSequence::INITIAL, inputs, control)
            .unwrap()
            .fingerprint()
    };
    assert_eq!(
        fingerprint(&spilled, &bounded),
        fingerprint(&memory, &resident)
    );
    let prefix = prefix.unwrap();
    assert_eq!(prefix.len(), 32);
    assert!(prefix.get(32).is_err());
    assert!(bounded.memory().peak() <= bounded.memory().limit());
    drop((spilled, memory, prefix));
    assert_eq!(bounded.memory().used(), 0);
    assert_eq!(resident.memory().used(), 0);
}
