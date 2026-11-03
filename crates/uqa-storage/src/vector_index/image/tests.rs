//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Image acceptance exercises independent wire values, observable queries and resource ownership.

use super::read_vector_image;
use crate::{
    read_control::StorageReadControl, vector_index::MemoryVectorIndex, HNSWIndex, IVFIndex,
    StorageBackendError, VectorIndex,
};
use std::io::{Read, Write};

pub(crate) fn encode(index: &dyn VectorIndex) -> Vec<u8> {
    let mut bytes = Vec::new();
    let control = StorageReadControl::with_limit(1 << 24);
    assert_eq!(
        index.write_image(&mut bytes, &control).unwrap(),
        bytes.len() as u64
    );
    assert_eq!(control.memory().used(), 0);
    bytes
}

fn sources() -> Vec<Box<dyn VectorIndex>> {
    let mut sources: Vec<Box<dyn VectorIndex>> = vec![
        Box::new(MemoryVectorIndex::new(2)),
        Box::new(IVFIndex::with_params(2, 2, 1, 4)),
        Box::new(HNSWIndex::new(2)),
    ];
    for source in &mut sources {
        for (id, tensor) in [
            (9, vec![vec![1.0, 0.0], vec![0.0, 1.0]]),
            (1, vec![vec![1.0e30, -0.0], vec![1.0, 0.0]]),
            (3, vec![vec![0.0, 0.0]]),
            (5, vec![vec![-1.0, 0.0]]),
            (7, Vec::new()),
        ] {
            source.add_many(id, tensor).unwrap();
        }
    }
    sources
}

fn scored(index: &dyn VectorIndex, query: &[f32], k: usize) -> Vec<(u64, u64)> {
    index
        .search_knn(query, k)
        .unwrap()
        .iter()
        .map(|p| (p.doc_id, p.payload.score.to_bits()))
        .collect()
}

#[test]
fn exact_image_matches_independent_tensor_bytes_and_preserves_coordinate_bits() {
    let mut fixture = b"UQAVEC01\x00\x02\x00\x00\x00".to_vec();
    for count in [2_u64, 3, 2, 1, 2] {
        fixture.extend(count.to_le_bytes());
    }
    fixture.extend(0x8000_0000_u32.to_le_bytes());
    fixture.extend(0x3f80_0000_u32.to_le_bytes());
    for count in [9_u64, 2, 2] {
        fixture.extend(count.to_le_bytes());
    }
    fixture.extend(0x3f80_0000_u32.to_le_bytes());
    fixture.extend(0_u32.to_le_bytes());
    fixture.extend(2_u64.to_le_bytes());
    fixture.extend(0_u32.to_le_bytes());
    fixture.extend(0x3f80_0000_u32.to_le_bytes());
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = read_vector_image(&mut fixture.as_slice(), &control).unwrap();
    assert_eq!(restored.count().unwrap(), 3);
    assert_eq!(
        scored(&**restored, &[1.0, 0.0], 1),
        vec![(9, 1.0_f64.to_bits())]
    );
    assert_eq!(encode(&**restored), fixture);
    let mut original = MemoryVectorIndex::new(2);
    original.add(2, vec![-0.0, 1.0]).unwrap();
    original
        .add_many(9, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    assert_eq!(encode(&original), fixture);
    assert!(control.memory().used() > 0);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn images_preserve_search_tensor_reduction_and_future_mutations() {
    for mut source in sources() {
        let bytes = encode(&*source);
        let control = StorageReadControl::with_limit(1 << 24);
        let restored = read_vector_image(&mut bytes.as_slice(), &control).unwrap();
        assert_eq!(encode(&**restored), bytes);
        assert_eq!(restored.index_kind(), source.index_kind());
        for query in [[1.0, 0.0], [-1.0, 0.0], [0.0, 0.0], [1.0e30, 0.0]] {
            for k in [0, 1, 3, 20] {
                assert_eq!(scored(&**restored, &query, k), scored(&*source, &query, k));
            }
            for threshold in [-1.0, 0.0, 0.75, 1.0] {
                assert_eq!(
                    restored.search_threshold(&query, threshold).unwrap(),
                    source.search_threshold(&query, threshold).unwrap()
                );
            }
        }
        let (mut restored, lease) = restored.into_parts();
        for index in [&mut *source, &mut *restored] {
            index.add_many(9, vec![vec![0.0, -1.0]]).unwrap();
            index.delete(5).unwrap();
            index.add_many(10, Vec::new()).unwrap();
            index.add(12, vec![0.0, 1.0]).unwrap();
        }
        assert_eq!(encode(&*restored), encode(&*source));
        assert_eq!(
            scored(&*restored, &[0.0, 1.0], 20),
            scored(&*source, &[0.0, 1.0], 20)
        );
        drop((restored, lease));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn retained_images_survive_source_changes_and_observe_original_cancellation() {
    for mut source in sources() {
        let capture = StorageReadControl::with_limit(1 << 24);
        let retained = source.snapshot_with_control(&capture).unwrap();
        let expected = encode(&*retained);
        let used = capture.memory().used();
        source.clear().unwrap();
        drop(source);
        let working = StorageReadControl::with_limit(1 << 24);
        let full = capture
            .memory()
            .reserve(capture.memory().limit() - used)
            .unwrap();
        let mut output = Vec::new();
        retained.write_image(&mut output, &working).unwrap();
        assert_eq!(output, expected);
        assert_eq!(working.memory().used(), 0);
        drop(full);
        capture.cancellation().cancel();
        let mut output = Vec::new();
        assert!(matches!(
            retained.write_image(&mut output, &working),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert!(output.is_empty());
        capture.cancellation().reset();
        assert_eq!(encode(&*retained), expected);
        drop(retained);
        assert_eq!(capture.memory().used(), 0);
    }
}

#[test]
fn export_only_vector_captures_keep_exact_ivf_and_hnsw_state_after_live_queries() {
    for mut source in sources() {
        source.delete(9).unwrap();
        let expected = encode(&*source);
        let capture = StorageReadControl::with_limit(1 << 24);
        let image = source.capture_image_with_control(&capture).unwrap();
        source.search_knn(&[1.0, 0.0], 5).unwrap();
        source.clear().unwrap();
        drop(source);
        let working = StorageReadControl::with_limit(1 << 24);
        let used = capture.memory().used();
        assert!(used > 0);
        let full = capture
            .memory()
            .reserve(capture.memory().limit() - used)
            .unwrap();
        let mut bytes = Vec::new();
        image.write_image(&mut bytes, &working).unwrap();
        assert_eq!(bytes, expected);
        drop(full);
        let nested = image.clone();
        drop(image);
        capture.cancellation().cancel();
        assert!(matches!(
            nested.write_image(&mut Vec::new(), &working),
            Err(StorageBackendError::Cancelled(_))
        ));
        drop(nested);
        assert_eq!(capture.memory().used(), 0);
    }
}

#[test]
fn every_truncated_image_and_trailing_data_rejects_without_retained_allocations() {
    for source in sources() {
        let bytes = encode(&*source);
        let control = StorageReadControl::with_limit(1 << 20);
        for end in 0..bytes.len() {
            assert!(
                read_vector_image(&mut &bytes[..end], &control).is_err(),
                "{} prefix {end}",
                source.index_kind()
            );
            assert_eq!(control.memory().used(), 0);
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(read_vector_image(&mut trailing.as_slice(), &control).is_err());
        for (offset, value) in [(0, b'X'), (7, b'2'), (8, 255)] {
            let mut bad = trailing[..trailing.len() - 1].to_vec();
            bad[offset] = value;
            assert!(read_vector_image(&mut bad.as_slice(), &control).is_err());
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn image_import_admission_releases_every_partial_candidate() {
    for source in sources() {
        let bytes = encode(&*source);
        let probe = StorageReadControl::with_limit(1 << 20);
        let restored = read_vector_image(&mut bytes.as_slice(), &probe).unwrap();
        let peak = probe.memory().peak();
        let retained = probe.memory().used();
        assert!(retained > 0);
        drop(restored);
        for limit in [0, 1, 63, retained / 2, peak - 1] {
            let control = StorageReadControl::with_limit(limit);
            let result = read_vector_image(&mut bytes.as_slice(), &control);
            // Spillable HNSW may choose a smaller valid representation at a lower limit.
            if let Err(error) = &result {
                assert!(matches!(error, StorageBackendError::Memory(_)), "{error}");
            }
            if limit == 0 {
                assert!(result.is_err());
            }
            drop(result);
            assert_eq!(control.memory().used(), 0);
            assert!(control.memory().peak() <= limit);
        }
    }
}

#[test]
fn empty_and_zero_dimension_images_preserve_the_owners_existing_carriers() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut indexes: Vec<Box<dyn VectorIndex>> = vec![
        Box::new(MemoryVectorIndex::new(0)),
        Box::new(IVFIndex::with_params(0, 1, 1, 1)),
        Box::new(HNSWIndex::new(0)),
    ];
    for index in &mut indexes {
        for populate in [false, true] {
            if populate {
                index.add_many(0, vec![Vec::new(), Vec::new()]).unwrap();
            }
            let bytes = encode(&**index);
            let restored = read_vector_image(&mut bytes.as_slice(), &control).unwrap();
            assert_eq!(encode(&**restored), bytes);
            assert_eq!(restored.count().unwrap(), usize::from(populate) * 2);
            assert_eq!(scored(&**restored, &[], 5), scored(&**index, &[], 5));
        }
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn exact_image_rejects_wrong_counts_nonfinite_values_and_unordered_documents() {
    let mut source = MemoryVectorIndex::new(2);
    source.add(1, vec![1.0, 0.0]).unwrap();
    source.add(2, vec![0.0, 1.0]).unwrap();
    let bytes = encode(&source);
    let control = StorageReadControl::with_limit(1 << 20);
    for (offset, replacement) in [
        (21, 1_u64.to_le_bytes().to_vec()),
        (37, 0_u64.to_le_bytes().to_vec()),
        (45, 3_u64.to_le_bytes().to_vec()),
        (53, f32::INFINITY.to_bits().to_le_bytes().to_vec()),
        (61, 1_u64.to_le_bytes().to_vec()),
    ] {
        let mut invalid = bytes.clone();
        invalid[offset..offset + replacement.len()].copy_from_slice(&replacement);
        assert!(
            read_vector_image(&mut invalid.as_slice(), &control).is_err(),
            "offset {offset}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

struct Bounded<'a> {
    bytes: &'a [u8],
    calls: usize,
}
impl Read for Bounded<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(output.len() <= 4096);
        self.calls += 1;
        self.bytes.read(output)
    }
}

struct Sink(usize);
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert!(bytes.len() <= 4096);
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn large_coordinates_use_bounded_chunks_without_a_full_export_cache() {
    let mut source = MemoryVectorIndex::new(1 << 19);
    source.add(4, vec![-0.0; 1 << 19]).unwrap();
    let mut sink = Sink(0);
    let control = StorageReadControl::with_limit(0);
    let allocation = allocation_counter::measure(|| {
        source.write_image(&mut sink, &control).unwrap();
    });
    assert_eq!(allocation.bytes_total, 0);
    assert!(sink.0 > 2 * 1024 * 1024);
    let bytes = encode(&source);
    let control = StorageReadControl::with_limit(4 << 20);
    let mut input = Bounded {
        bytes: &bytes,
        calls: 0,
    };
    let restored = read_vector_image(&mut input, &control).unwrap();
    assert!(input.calls > 500);
    assert_eq!(encode(&**restored), bytes);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

struct CancelRead<'a> {
    bytes: &'a [u8],
    control: &'a StorageReadControl,
}
impl Read for CancelRead<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = self.bytes.read(output)?;
        if self.bytes.len() < 50 {
            self.control.cancellation().cancel();
        }
        Ok(count)
    }
}

struct FailWrite(usize);
impl Write for FailWrite {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0 == 0 {
            return Err(std::io::Error::other("injected output failure"));
        }
        let count = bytes.len().min(self.0);
        self.0 -= count;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn image_io_failures_and_midstream_cancellation_preserve_typed_errors() {
    for source in sources() {
        let bytes = encode(&*source);
        let control = StorageReadControl::with_limit(1 << 20);
        assert!(matches!(
            read_vector_image(
                &mut CancelRead {
                    bytes: &bytes,
                    control: &control
                },
                &control
            ),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(control.memory().used(), 0);
        control.cancellation().reset();
        assert!(source
            .write_image(&mut FailWrite(43), &control)
            .unwrap_err()
            .to_string()
            .contains("injected output failure"));
        assert_eq!(encode(&*source), bytes);
        assert_eq!(control.memory().used(), 0);
    }
}
