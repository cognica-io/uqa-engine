//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::vector_index::image::vector_image_input;
use std::io::{Cursor, Read};
use uqa_core::image_io::ImageDecodeWindow;

const DIMENSIONS: usize = 4096;

fn scored(index: &dyn VectorIndex, query: &[f32], k: usize) -> Vec<(u64, u64)> {
    index
        .search_knn(query, k)
        .unwrap()
        .iter()
        .map(|posting| (posting.doc_id, posting.payload.score.to_bits()))
        .collect()
}

fn wide(control: &StorageReadControl) -> DiskANNMemoryIndex {
    let mut options = options(DIMENSIONS as u32);
    options.read.resident_bytes = 1 << 20;
    options.read.max_record_bytes = 1 << 20;
    options.generation.max_record_bytes = 1 << 20;
    let mut source = DiskANNMemoryIndex::new(
        DIMENSIONS as u32,
        options,
        &DiskANNTemporaryBudget::new(32 << 20),
        control,
    )
    .unwrap();
    source
        .add_many(1, vec![vec![1.0; DIMENSIONS], vec![-0.0; DIMENSIONS]])
        .unwrap();
    source.add(2, vec![-1.0; DIMENSIONS]).unwrap();
    let mut axis = vec![0.0; DIMENSIONS];
    axis[0] = 1.0;
    source.add(3, axis).unwrap();
    let mut huge = vec![1.0; DIMENSIONS];
    huge[0] = f32::MAX;
    source.add(4, huge).unwrap();
    source.add_many(8, Vec::new()).unwrap();
    source.initialize().unwrap();
    assert!(
        source
            .manifest()
            .layout()
            .node_address(0)
            .unwrap()
            .fragments
            > 2
    );
    source.add(2, vec![0.5; DIMENSIONS]).unwrap();
    source.delete(3).unwrap();
    source
}

struct Wire<'a> {
    bytes: Cursor<&'a [u8]>,
    end_reads: usize,
}

impl Read for Wire<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        assert_eq!(self.end_reads, 0, "completed input must not be replayed");
        let count = self.bytes.read(bytes)?;
        self.end_reads += usize::from(count == 0);
        Ok(count)
    }
}

fn window(call: usize) -> ImageDecodeWindow {
    ImageDecodeWindow {
        bytes: if call.is_multiple_of(2) { 31 } else { 4096 },
        steps: 1,
    }
}

// Locate the physical/canonical boundary using only the independent outer wire shape.
fn physical_end(bytes: &[u8]) -> usize {
    let mut cursor = 61 + integer(bytes, 53) + 40;
    let records = integer(bytes, cursor);
    cursor += 8;
    for _ in 0..records {
        cursor += 9;
        cursor += 8 + integer(bytes, cursor);
    }
    cursor + 8 + integer(bytes, cursor) * (8 + PAGE_BYTES)
}

#[test]
fn diskann_input_resumes_wide_physical_preparation_without_rebuilding_or_replaying() {
    let original = StorageReadControl::with_limit(32 << 20);
    let source = wide(&original);
    let query = vec![1.0; DIMENSIONS];
    let expected = scored(&source, &query, 99);
    let bytes = encode(&source);
    let boundary = physical_end(&bytes) as u64;
    drop(source);
    assert_eq!(original.memory().used(), 0);
    let control = StorageReadControl::with_limit(32 << 20);
    let temporary = DiskANNTemporaryBudget::new(0);
    let mut input = vector_image_input(&control, Some(&temporary)).unwrap();
    let mut wire = Wire {
        bytes: Cursor::new(&bytes),
        end_reads: 0,
    };
    let mut sealing = 0;
    let mut preparation = 0;
    let mut calls = 0;
    let mut restored = loop {
        calls += 1;
        assert!(calls < 1_000_000);
        let before = wire.bytes.position();
        let window = window(calls);
        let result = input.advance(&mut wire, window).unwrap();
        assert!(wire.bytes.position() - before <= window.bytes as u64);
        if before == wire.bytes.position() && result.is_none() {
            sealing += usize::from(before == boundary);
            preparation += usize::from(before == bytes.len() as u64 && wire.end_reads == 0);
        }
        if let Some(result) = result {
            break result;
        }
    };
    assert!(
        sealing > 32,
        "physical sealing must resume before canonical input"
    );
    assert!(
        preparation > 32,
        "preparation must progress without source bytes"
    );
    assert_eq!(wire.end_reads, 1);
    assert_eq!(temporary.used(), 0);
    assert_eq!(encode(&*restored), bytes);
    assert_eq!(scored(&*restored, &query, 99), expected);
    let snapshot = restored.snapshot().unwrap();
    let capture_control = StorageReadControl::with_limit(32 << 20);
    let image = restored
        .capture_image_with_control(&capture_control)
        .unwrap();
    control.cancellation().cancel();
    restored.add(19, vec![-1.0; DIMENSIONS]).unwrap();
    restored.delete(1).unwrap();
    assert_eq!(scored(&*snapshot, &query, 99), expected);
    assert!(restored.contains_document(19).unwrap());
    assert!(!restored.contains_document(1).unwrap());
    drop((restored, snapshot, input));
    let mut frozen = Vec::new();
    image.write_image(&mut frozen, &capture_control).unwrap();
    assert_eq!(frozen, bytes);
    drop(image);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(capture_control.memory().used(), 0);
}

#[test]
fn diskann_input_failure_during_physical_sealing_and_query_preparation_retires_all_owners() {
    let original = StorageReadControl::with_limit(32 << 20);
    let bytes = encode(&wide(&original));
    assert_eq!(original.memory().used(), 0);
    let boundary = physical_end(&bytes) as u64;
    for phase in [boundary, bytes.len() as u64] {
        for failure in ["drop", "cancel", "memory", "window"] {
            let control = StorageReadControl::with_limit(32 << 20);
            let mut input = vector_image_input(&control, None).unwrap();
            let mut wire = Wire {
                bytes: Cursor::new(&bytes),
                end_reads: 0,
            };
            let mut calls = 0;
            loop {
                calls += 1;
                assert!(calls < 1_000_000);
                let before = wire.bytes.position();
                assert!(input.advance(&mut wire, window(calls)).unwrap().is_none());
                if before == phase && wire.bytes.position() == before {
                    break;
                }
                assert!(
                    wire.bytes.position() <= phase,
                    "missed preparation boundary"
                );
            }
            assert_eq!(wire.end_reads, 0);
            if failure == "drop" {
                drop(input);
            } else {
                if failure == "cancel" {
                    control.cancellation().cancel();
                }
                let held = (failure == "memory").then(|| {
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap()
                });
                let error = loop {
                    calls += 1;
                    assert!(calls < 1_000_000);
                    let window = ImageDecodeWindow {
                        steps: usize::from(failure != "window"),
                        ..window(calls)
                    };
                    match input.advance(&mut wire, window) {
                        Err(error) => break error,
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("invalid candidate completed: {failure}"),
                    }
                };
                match (failure, error) {
                    ("cancel", StorageBackendError::Cancelled(_))
                    | ("memory", StorageBackendError::Memory(_))
                    | ("window", StorageBackendError::Other(_)) => (),
                    (failure, error) => panic!("unexpected {failure} failure: {error}"),
                }
                drop((input, held));
            }
            assert_eq!(
                control.memory().used(),
                0,
                "phase={phase}, failure={failure}"
            );
        }
    }
}
