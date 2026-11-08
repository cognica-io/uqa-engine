//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh memory images preserve sealed generations, exact-side inputs and uncovered origins.

use super::*;
use crate::{
    image_io::ImageInput,
    vector_index::image::{read_vector_image, read_vector_image_with_temporary, tests::encode},
};

mod stream;

fn populated(control: &StorageReadControl) -> DiskANNMemoryIndex {
    let mut source = new(control);
    source
        .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    source.add(2, vec![-1.0, 0.0]).unwrap();
    source.add(3, vec![0.0, -0.0]).unwrap();
    source.add(4, vec![f32::MAX, 1.0]).unwrap();
    source.add_many(5, Vec::new()).unwrap();
    source.initialize().unwrap();
    source.delete(2).unwrap();
    source.add(3, vec![0.5, 0.5]).unwrap();
    source
        .add_many(8, vec![vec![1.0, 0.0], vec![1.0e30, 0.0]])
        .unwrap();
    source.add_many(9, Vec::new()).unwrap();
    source
}

fn restore(
    bytes: &[u8],
    control: &StorageReadControl,
    temporary: &DiskANNTemporaryBudget,
) -> DiskANNMemoryIndex {
    let mut bytes = &bytes[13..];
    let mut input = ImageInput {
        reader: &mut bytes,
        control,
    };
    let result = super::super::image::read(&mut input, 2, temporary).unwrap();
    input.end().unwrap();
    result
}

#[test]
fn memory_diskann_empty_image_composes_existing_manifest_with_independent_wire_fields() {
    let control = StorageReadControl::with_limit(1 << 20);
    let source = new(&control);
    let working = StorageReadControl::with_limit(1 << 20);
    let manifest = source.manifest();
    let mut expected = b"UQAVEC01\x03\x02\x00\x00\x00".to_vec();
    expected.extend(manifest.input().generation.database());
    for value in [1_u64, 1, 1] {
        expected.extend(value.to_le_bytes());
    }
    let manifest = manifest.encode(&working).unwrap();
    expected.extend((manifest.len() as u64).to_le_bytes());
    expected.extend(&*manifest);
    for value in [65_536_u64, 0, 8192, 8192, 8192, 0, 0] {
        expected.extend(value.to_le_bytes());
    }
    expected.push(0); // No canonical revision.
    for _ in 0..4 {
        expected.extend(0_u64.to_le_bytes());
    }
    assert_eq!(encode(&source), expected);
    let target = StorageReadControl::with_limit(1 << 20);
    let restored = read_vector_image(&mut expected.as_slice(), &target).unwrap();
    assert_eq!(encode(&*restored), expected);
    assert_eq!(restored.count().unwrap(), 0);
    drop(restored);
    assert_eq!(target.memory().used(), 0);
}

#[test]
fn memory_diskann_images_preserve_physical_metadata_changes_tensor_scores_and_source_loss() {
    let original = StorageReadControl::with_limit(1 << 20);
    let source = populated(&original);
    let expected_metadata = source.diskann_query_metadata(&original).unwrap();
    let bytes = encode(&source);
    let target = StorageReadControl::with_limit(1 << 20);
    let restored = read_vector_image(&mut bytes.as_slice(), &target).unwrap();
    assert_eq!(encode(&*restored), bytes);
    assert_eq!(
        restored.diskann_query_metadata(&target).unwrap(),
        expected_metadata
    );
    for query in [[1.0, 0.0], [0.0, 1.0], [0.0, 0.0], [f32::MAX, 0.0]] {
        for k in [0, 1, 5, 99] {
            let bits = |index: &dyn VectorIndex| {
                index
                    .search_knn(&query, k)
                    .unwrap()
                    .iter()
                    .map(|p| (p.doc_id, p.payload.score.to_bits()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(bits(&*restored), bits(&source));
        }
        for threshold in [-1.0, 0.0, 0.5, 1.0] {
            assert_eq!(
                restored.search_threshold(&query, threshold).unwrap(),
                source.search_threshold(&query, threshold).unwrap()
            );
        }
    }
    let expected = scores(&source);
    drop(source);
    assert_eq!(original.memory().used(), 0);
    assert_eq!(scores(&*restored), expected);
    drop(restored);
    assert_eq!(target.memory().used(), 0);
}

#[test]
fn memory_diskann_restore_retains_build_options_but_establishes_independent_future_origins() {
    let original = StorageReadControl::with_limit(1 << 20);
    let mut source = populated(&original);
    let bytes = encode(&source);
    let target = StorageReadControl::with_limit(1 << 20);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let mut restored = restore(&bytes, &target, &temporary);
    assert_eq!(restored.options, source.options);
    assert_ne!(restored.clock.database, source.clock.database);
    assert_eq!(temporary.used(), 0);
    let base_generation = *restored.manifest();
    let old = restored.capture_image_with_control(&target).unwrap();
    for index in [&mut source, &mut restored] {
        index.add(1, vec![0.0, -1.0]).unwrap();
        index.delete(8).unwrap();
        index.add(12, vec![1.0, 0.0]).unwrap();
    }
    assert_eq!(scores(&source), scores(&restored));
    let version = |index: &DiskANNMemoryIndex| {
        index
            .index
            .canonical()
            .origin(12, &index.control)
            .unwrap()
            .unwrap()
    };
    assert_ne!(
        version(&restored).writer().database(),
        version(&source).writer().database()
    );
    assert_eq!(*restored.manifest(), base_generation);
    restored.initialize().unwrap();
    assert_ne!(
        restored.manifest().input().generation,
        base_generation.input().generation
    );
    source.initialize().unwrap();
    assert_eq!(scores(&source), scores(&restored));
    assert_eq!(temporary.used(), 0);
    let mut old_bytes = Vec::new();
    old.write_image(&mut old_bytes, &original).unwrap();
    assert_eq!(old_bytes, bytes);
    drop((old, restored));
    assert_eq!(target.memory().used(), 0);
}

#[test]
fn memory_diskann_image_capture_is_constant_size_and_retains_original_cancellation() {
    let source_control = StorageReadControl::with_limit(4 << 20);
    let capture_control = StorageReadControl::with_limit(4096);
    let working = StorageReadControl::with_limit(1 << 20);
    let mut source = new(&source_control);
    source.add(1, vec![1.0, 0.0]).unwrap();
    let small = source.capture_image_with_control(&capture_control).unwrap();
    let retained = capture_control.memory().used();
    let mut expected = Vec::new();
    small.write_image(&mut expected, &working).unwrap();
    drop(small);
    for id in 2..=1024 {
        source.add(id, vec![id as f32, 1.0]).unwrap();
    }
    let large = source.capture_image_with_control(&capture_control).unwrap();
    assert_eq!(capture_control.memory().used(), retained);
    let frozen = encode(&source);
    source.clear().unwrap();
    drop(source);
    let held = source_control
        .memory()
        .reserve(source_control.memory().limit() - source_control.memory().used())
        .unwrap();
    let mut actual = Vec::new();
    large.write_image(&mut actual, &working).unwrap();
    assert_eq!(actual, frozen);
    drop(held);
    for token in [
        source_control.cancellation(),
        capture_control.cancellation(),
    ] {
        token.cancel();
        assert!(matches!(
            large.write_image(&mut Vec::new(), &working),
            Err(StorageBackendError::Cancelled(_))
        ));
        token.reset();
    }
    let nested = large.clone();
    drop(large);
    assert_eq!(capture_control.memory().used(), retained);
    drop(nested);
    assert_eq!(capture_control.memory().used(), 0);
    assert_eq!(source_control.memory().used(), 0);
}

#[test]
fn memory_diskann_image_rejects_truncated_corrupt_and_unfunded_candidates() {
    let source_control = StorageReadControl::with_limit(1 << 20);
    let source = new(&source_control);
    let bytes = encode(&source);
    let target = StorageReadControl::with_limit(1 << 20);
    for length in 0..bytes.len() {
        assert!(
            read_vector_image(&mut &bytes[..length], &target).is_err(),
            "prefix {length}"
        );
        assert_eq!(target.memory().used(), 0);
    }
    let source = populated(&source_control);
    let bytes = encode(&source);
    for position in [13, 29, 45, 65, bytes.len() / 2, bytes.len() - 1] {
        let mut corrupt = bytes.clone();
        corrupt[position] ^= 0xff;
        assert!(
            read_vector_image(&mut corrupt.as_slice(), &target).is_err(),
            "position {position}"
        );
        assert_eq!(target.memory().used(), 0);
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(read_vector_image(&mut extra.as_slice(), &target).is_err());
    for limit in [0, 1, 512, 4096, 8192] {
        let limited = StorageReadControl::with_limit(limit);
        assert!(matches!(
            read_vector_image(&mut bytes.as_slice(), &limited),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(limited.memory().used(), 0);
    }
}

#[test]
fn memory_diskann_restore_uses_the_destination_temporary_budget_without_import_rebuilds() {
    let original = StorageReadControl::with_limit(1 << 20);
    let source = populated(&original);
    let bytes = encode(&source);
    let target = StorageReadControl::with_limit(1 << 20);
    let temporary = DiskANNTemporaryBudget::new(0);
    let mut restored =
        read_vector_image_with_temporary(&mut bytes.as_slice(), &target, &temporary).unwrap();
    assert_eq!(encode(&*restored), bytes);
    assert_eq!(temporary.used(), 0);
    assert!(restored.initialize().is_err());
    assert_eq!(encode(&*restored), bytes);
    drop(restored);
    assert_eq!(target.memory().used(), 0);
}

fn integer(bytes: &[u8], offset: usize) -> usize {
    usize::try_from(u64::from_le_bytes(
        bytes[offset..offset + 8].try_into().unwrap(),
    ))
    .unwrap()
}

#[test]
fn memory_diskann_images_reject_self_consistent_counts_with_missing_origin_changes() {
    let source_control = StorageReadControl::with_limit(1 << 20);
    let source = populated(&source_control);
    let bytes = encode(&source);
    let target = StorageReadControl::with_limit(1 << 20);
    // Walk the outer wire independently of the image decoder and remove the
    // complete change map while correcting its declared population to zero.
    let manifest_end = 61 + integer(&bytes, 53);
    let mut cursor = manifest_end + 40;
    let records = integer(&bytes, cursor);
    cursor += 8;
    for _ in 0..records {
        cursor += 9;
        let length = integer(&bytes, cursor);
        cursor += 8 + length;
    }
    let pages = integer(&bytes, cursor);
    cursor += 8 + pages * (8 + PAGE_BYTES);
    assert_eq!(bytes[cursor], 1);
    cursor += 1 + crate::diskann_index::format::CHANGE_IDENTITY_BYTES;
    let changed_count = cursor + 8;
    cursor += 16;
    let documents = integer(&bytes, cursor);
    cursor += 8;
    for _ in 0..documents {
        cursor += 8;
        let count = integer(&bytes, cursor + 48);
        cursor += crate::diskann_index::format::CANONICAL_ORIGIN_BYTES;
        for _ in 0..count {
            let dimensions = integer(&bytes, cursor);
            cursor += 8 + dimensions * 4;
        }
    }
    let mut missing = bytes[..cursor + 8].to_vec();
    missing[cursor..cursor + 8].copy_from_slice(&0_u64.to_le_bytes());
    missing[changed_count..changed_count + 8].copy_from_slice(&0_u64.to_le_bytes());
    assert!(read_vector_image(&mut missing.as_slice(), &target).is_err());
    assert_eq!(target.memory().used(), 0);
    let mut unsupported = encode(&new(&source_control));
    let limit = 61 + integer(&unsupported, 53) + 32;
    unsupported[limit..limit + 8].copy_from_slice(&0_u64.to_le_bytes());
    assert!(read_vector_image(&mut unsupported.as_slice(), &target).is_err());
    assert_eq!(target.memory().used(), 0);
}
