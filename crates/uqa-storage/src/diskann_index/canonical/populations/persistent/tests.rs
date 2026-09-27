//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::format::DiskANNVectorVersion;
use crate::mvcc::{DatabaseId, StorageTransactionId};

fn generation() -> DiskANNGeneration {
    DiskANNGeneration::new([7; 16], 11, 13, 17).unwrap()
}

fn origin(revision: u64, count: u64) -> DiskANNCanonicalOrigin {
    DiskANNCanonicalOrigin::new(
        DiskANNVectorVersion::new(
            StorageTransactionId::new(DatabaseId::from_bytes([19; 16]), 23).unwrap(),
            revision,
        )
        .unwrap(),
        2,
        count,
    )
    .unwrap()
}

fn state() -> DiskANNPopulationState {
    DiskANNPopulationState {
        generation: generation(),
        dimensions: 2,
        counts: DiskANNCanonicalCounts::new(7, 3).unwrap(),
    }
}

fn witness() -> DiskANNPopulationWitness {
    DiskANNPopulationWitness {
        generation: generation(),
        document: 29,
        origin: origin(31, 4),
        covered: true,
    }
}

#[test]
fn diskann_population_records_have_explicit_fixed_layouts_and_context() {
    let state = state();
    let mut expected = [0; 72];
    expected[..8].copy_from_slice(b"UQAVPOP1");
    expected[8..24].fill(7);
    expected[24] = 11;
    expected[32] = 13;
    expected[40] = 17;
    expected[48] = 2;
    expected[56] = 7;
    expected[64] = 3;
    assert_eq!(state.encode(), expected);
    assert_eq!(
        DiskANNPopulationState::decode(&expected, generation(), 2).unwrap(),
        state
    );
    let witness = witness();
    let mut expected = [0; 120];
    expected[..8].copy_from_slice(b"UQAVCOV1");
    expected[8..24].fill(7);
    expected[24] = 11;
    expected[32] = 13;
    expected[40] = 17;
    expected[48] = 29;
    expected[56..64].copy_from_slice(b"UQAVORG1");
    expected[64..80].fill(19);
    expected[80] = 23;
    expected[88] = 31;
    expected[96] = 2;
    expected[104] = 4;
    expected[112] = 1;
    assert_eq!(witness.encode(), expected);
    assert_eq!(
        DiskANNPopulationWitness::decode(&expected, generation(), 29, origin(31, 4)).unwrap(),
        witness
    );
    for other in [
        DiskANNGeneration::new([8; 16], 11, 13, 17).unwrap(),
        DiskANNGeneration::new([7; 16], 12, 13, 17).unwrap(),
        DiskANNGeneration::new([7; 16], 11, 14, 17).unwrap(),
        DiskANNGeneration::new([7; 16], 11, 13, 18).unwrap(),
    ] {
        assert!(DiskANNPopulationState::decode(&state.encode(), other, 2).is_err());
        assert!(DiskANNPopulationWitness::decode(&expected, other, 29, origin(31, 4)).is_err());
    }
    assert!(DiskANNPopulationState::decode(&state.encode(), generation(), 3).is_err());
    assert!(DiskANNPopulationWitness::decode(&expected, generation(), 30, origin(31, 4)).is_err());
    assert!(DiskANNPopulationWitness::decode(&expected, generation(), 29, origin(32, 4)).is_err());
    assert!(DiskANNPopulationWitness::decode(&expected, generation(), 29, origin(31, 5)).is_err());
}

#[test]
fn diskann_population_records_reject_malformed_envelopes_and_invalid_subsets() {
    let bytes = state().encode();
    for length in 0..bytes.len() {
        assert!(DiskANNPopulationState::decode(&bytes[..length], generation(), 2).is_err());
    }
    assert!(DiskANNPopulationState::decode(&[0; 73], generation(), 2).is_err());
    for (offset, value) in [(0, 0), (7, b'2'), (48, 0), (52, 1), (64, 8)] {
        let mut malformed = bytes;
        malformed[offset] = value;
        assert!(DiskANNPopulationState::decode(&malformed, generation(), 2).is_err());
    }
    let bytes = witness().encode();
    for length in 0..bytes.len() {
        assert!(DiskANNPopulationWitness::decode(
            &bytes[..length],
            generation(),
            29,
            origin(31, 4)
        )
        .is_err());
    }
    assert!(DiskANNPopulationWitness::decode(&[0; 121], generation(), 29, origin(31, 4)).is_err());
    for (offset, value) in [(0, 0), (7, b'2'), (80, 0), (100, 1), (112, 2), (119, 1)] {
        let mut malformed = bytes;
        malformed[offset] = value;
        assert!(
            DiskANNPopulationWitness::decode(&malformed, generation(), 29, origin(31, 4)).is_err()
        );
    }
    let empty = DiskANNPopulationWitness {
        origin: origin(32, 0),
        covered: false,
        ..witness()
    };
    assert_eq!(
        DiskANNPopulationWitness::decode(&empty.encode(), generation(), 29, origin(32, 0)).unwrap(),
        empty
    );
}

#[test]
fn diskann_population_replacement_requires_the_actual_preceding_witness() {
    let state = state();
    let before = origin(31, 4);
    let after = origin(32, 2);
    let kept = state;
    let (state, replaced) = state
        .replaced(29, Some((before, witness())), after)
        .unwrap();
    assert_eq!(state.counts(), DiskANNCanonicalCounts::new(5, 5).unwrap());
    assert_eq!(kept.counts(), DiskANNCanonicalCounts::new(7, 3).unwrap());
    assert!(!replaced.covered());
    assert_eq!(replaced.origin(), after);
    assert_eq!(replaced.document(), 29);
    assert!(kept.replaced(30, Some((before, witness())), after).is_err());
    assert!(kept
        .replaced(29, Some((origin(31, 3), witness())), after)
        .is_err());
    assert!(kept
        .replaced(29, Some((before, witness())), before)
        .is_err());
    let other = DiskANNPopulationWitness {
        generation: DiskANNGeneration::new([7; 16], 11, 13, 18).unwrap(),
        ..witness()
    };
    assert!(kept.replaced(29, Some((before, other)), after).is_err());
    let other_width = DiskANNCanonicalOrigin::new(after.version(), 3, 2).unwrap();
    assert!(kept
        .replaced(29, Some((before, witness())), other_width)
        .is_err());
    let (state, empty) = state
        .replaced(29, Some((after, replaced)), origin(33, 0))
        .unwrap();
    assert_eq!(state.counts(), DiskANNCanonicalCounts::new(3, 3).unwrap());
    let (state, _) = state
        .replaced(29, Some((empty.origin(), empty)), origin(34, 1))
        .unwrap();
    assert_eq!(state.counts(), DiskANNCanonicalCounts::new(4, 4).unwrap());
    let full = DiskANNPopulationState {
        counts: DiskANNCanonicalCounts::new(u64::MAX, u64::MAX).unwrap(),
        ..kept
    };
    assert!(full.replaced(30, None, origin(35, 1)).is_err());
    let inconsistent = DiskANNPopulationState {
        counts: DiskANNCanonicalCounts::new(3, 3).unwrap(),
        ..kept
    };
    assert!(inconsistent
        .replaced(29, Some((before, witness())), after)
        .is_err());
}

#[test]
fn diskann_population_replacements_match_independent_full_origin_sums() {
    let mut current = std::collections::BTreeMap::from([(2, origin(1, 3)), (5, origin(1, 0))]);
    let built = current.clone();
    let mut witnesses = current
        .iter()
        .map(|(&document, &origin)| {
            (
                document,
                DiskANNPopulationWitness {
                    generation: generation(),
                    document,
                    origin,
                    covered: true,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut state = DiskANNPopulationState {
        generation: generation(),
        dimensions: 2,
        counts: DiskANNCanonicalCounts::new(3, 0).unwrap(),
    };
    for revision in 100..356 {
        let document = (revision * 17) % 11;
        let replacement = origin(revision, revision % 5);
        let previous = current
            .get(&document)
            .map(|&origin| (origin, witnesses[&document]));
        let (next, witness) = state.replaced(document, previous, replacement).unwrap();
        current.insert(document, replacement);
        witnesses.insert(document, witness);
        state = next;
        let total = current.values().map(|origin| origin.count()).sum();
        let changed = current
            .iter()
            .filter(|(document, origin)| built.get(document) != Some(origin))
            .map(|(_, origin)| origin.count())
            .sum();
        assert_eq!(
            state.counts(),
            DiskANNCanonicalCounts::new(total, changed).unwrap()
        );
    }
}
