//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt::Write;

use super::*;
use crate::mvcc::{DatabaseId, StorageTransactionId};

mod failures;

pub(super) fn generation() -> DiskANNGeneration {
    DiskANNGeneration::new([1; 16], 2, 3, 4).unwrap()
}

pub(super) fn version() -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([9; 16]), 7).unwrap(),
        3,
    )
    .unwrap()
}

fn options() -> PQTrainingOptions {
    PQTrainingOptions {
        max_samples: 4,
        max_iterations: 3,
        max_centroids: 2,
        seed: 42,
    }
}

const RAW: [(DocId, u32, [u32; 2]); 5] = [
    (10, 0, [0x4040_0000, 0x4080_0000]),
    (10, 1, [0x8000_0000, 0]),
    (20, 0, [0x7f7f_ffff, 0x7f7f_ffff]),
    (21, 0, [0xbf80_0000, 0]),
    (21, 1, [0x0080_0000, 0]),
];

fn fixture(
    directory: &Path,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNBuildInput> {
    DiskANNBuildInput::capture(generation(), 2, directory, temporary, control, |visitor| {
        for (doc, ordinal, raw) in RAW {
            visitor(doc, ordinal, version(), &raw.map(f32::from_bits))?;
        }
        Ok(())
    })
}

fn empty(directory: &Path) -> bool {
    std::fs::read_dir(directory).unwrap().next().is_none()
}

#[test]
fn captured_bits_origins_classification_and_coverage_match_independent_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = fixture(directory.path(), &temporary, &control).unwrap();
    assert_eq!(input.dimensions(), 2);
    assert_eq!(input.node_count(), 2);
    assert_eq!(input.side_count(), 3);
    assert_eq!(input.coverage().vector_count(), 5);
    let mut digest = String::new();
    for byte in input.coverage().digest() {
        write!(digest, "{byte:02x}").unwrap();
    }
    // Independently fixed with Python struct/hashlib before this capture implementation; see the fixture provenance.
    assert_eq!(
        digest,
        "7e20a0b4972bf9be57997a63789ddd7b79c73f6e5f248748eb9f256a211dd2f1"
    );
    for (node, original) in [0, 3].into_iter().enumerate() {
        let vector = input.read_node(node as u64).unwrap();
        assert_eq!((vector.doc_id(), vector.ordinal()), (RAW[original].0, 0));
        assert_eq!(vector.version(), version());
        assert_eq!(vector.exact_reason(), None);
        assert_eq!(
            vector.raw().iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            RAW[original].2
        );
    }
    for (index, (original, reason)) in [
        (1, ExactVectorReason::ZeroNorm),
        (2, ExactVectorReason::NonFiniteNorm),
        (4, ExactVectorReason::ZeroNorm),
    ]
    .into_iter()
    .enumerate()
    {
        let vector = input.read_side(index as u64).unwrap();
        assert_eq!(
            (vector.doc_id(), vector.ordinal()),
            (RAW[original].0, RAW[original].1)
        );
        assert_eq!(vector.version(), version());
        assert_eq!(vector.exact_reason(), Some(reason));
        assert_eq!(
            vector.raw().iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            RAW[original].2
        );
    }
    assert!(input.read_node(2).is_err());
    assert!(input.read_side(u64::MAX).is_err());
    assert_eq!(control.memory().used(), 0);
    let codebook = input.train(1, options()).unwrap().unwrap();
    assert_eq!(codebook.training().observed_vectors, 2);
    let mut direct = PQTrainer::new(2, 1, options(), &control).unwrap();
    for original in [0, 3] {
        let NavigationInput::Navigable(vector) =
            NavigationInput::from_raw(2, &RAW[original].2.map(f32::from_bits), &control).unwrap()
        else {
            panic!("expected navigable fixture");
        };
        direct.observe(&vector).unwrap();
    }
    let direct = direct.finish().unwrap();
    assert_eq!(codebook.chunk_centroids(0), direct.chunk_centroids(0));
    drop((codebook, direct, input));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(temporary.used(), 0);
    assert!(empty(directory.path()));
}

#[test]
fn a_corpus_larger_than_memory_is_captured_once_and_trained_from_encrypted_records() {
    const COUNT: u64 = 256;
    const DIMENSIONS: u32 = 16;
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(4096);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let mut observations = 0;
    let input = DiskANNBuildInput::capture(
        generation(),
        DIMENSIONS,
        directory.path(),
        &temporary,
        &control,
        |visitor| {
            for doc in 1..=COUNT {
                observations += 1;
                visitor(doc, 0, version(), &[doc as f32; DIMENSIONS as usize])?;
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(input.node_count(), COUNT);
    assert!(COUNT * u64::from(DIMENSIONS) * 4 > control.memory().limit() as u64);
    let codebook = input.train(4, options()).unwrap().unwrap();
    assert_eq!(observations, COUNT);
    assert_eq!(codebook.training().observed_vectors, COUNT);
    assert_eq!(codebook.training().sampled_vectors, 4);
    let physical = std::fs::metadata(input.navigation.path()).unwrap().len();
    assert_eq!(temporary.used(), physical);
    assert_eq!(temporary.peak(), physical);
    assert!(physical > COUNT * u64::from(DIMENSIONS) * 4);
    assert!(control.memory().peak() <= 4096);
    assert_eq!(input.read_node(COUNT - 1).unwrap().doc_id(), COUNT);
    let bytes = std::fs::read(input.navigation.path()).unwrap();
    let raw = vec![(COUNT as f32).to_le_bytes(); DIMENSIONS as usize].concat();
    assert!(!bytes.windows(raw.len()).any(|window| window == raw));
    let retained = input.read_node(0).unwrap();
    drop((codebook, input));
    assert!(control.memory().used() >= DIMENSIONS as usize * 4);
    assert_eq!(retained.raw(), &[1.0; DIMENSIONS as usize]);
    assert_eq!(temporary.used(), 0);
    assert!(empty(directory.path()));
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn empty_and_all_side_captures_have_no_fabricated_quantization() {
    for side in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let control = StorageReadControl::with_limit(1024);
        let input = DiskANNBuildInput::capture(
            generation(),
            2,
            directory.path(),
            &temporary,
            &control,
            |visitor| {
                if side {
                    visitor(1, 0, version(), &[0.0, -0.0])?;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(input.node_count(), 0);
        assert_eq!(input.side_count(), u64::from(side));
        assert!(input.train(1, options()).unwrap().is_none());
        assert!(input.train(0, options()).is_err());
        if !side {
            assert_eq!(temporary.used(), 0);
        }
        drop(input);
        assert_eq!(temporary.used(), 0);
        assert_eq!(control.memory().used(), 0);
        assert!(empty(directory.path()));
    }
}
