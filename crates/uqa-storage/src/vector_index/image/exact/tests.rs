//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent bytes, sparse continuations and retained ownership qualify exact vector image reads.

use super::{MemoryVectorIndex, StorageReadControl, VectorIndex};
use crate::{
    image_io::ImageReader,
    vector_index::image::{read_vector_image, tests::encode},
    StorageBackendError,
};

fn read_all(reader: &mut ImageReader<'_>, width: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = vec![0; width];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= width.min(16 * 1024));
        if count == 0 {
            return bytes;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn exact_image_readers_preserve_independent_bytes_at_every_field_boundary() {
    let mut expected = b"UQAVEC01\x00\x02\x00\x00\x00".to_vec();
    for value in [2_u64, 3, 2, 1, 2] {
        expected.extend(value.to_le_bytes());
    }
    for value in [0x8000_0000_u32, 0x3f80_0000] {
        expected.extend(value.to_le_bytes());
    }
    for value in [9_u64, 2, 2] {
        expected.extend(value.to_le_bytes());
    }
    for value in [0x3f80_0000_u32, 0] {
        expected.extend(value.to_le_bytes());
    }
    expected.extend(2_u64.to_le_bytes());
    for value in [0_u32, 0x3f80_0000] {
        expected.extend(value.to_le_bytes());
    }
    let mut index = MemoryVectorIndex::new(2);
    index.add(2, vec![-0.0, 1.0]).unwrap();
    index
        .add_many(9, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    assert_eq!(encode(&index), expected);
    for width in [1, 2, 3, 7, 13, 29, 4093, 32 * 1024] {
        let control = StorageReadControl::with_limit(16 * 1024);
        let mut reader = index.image_reader(&control).unwrap();
        let prepared = reader.prepare(1).unwrap();
        assert!(prepared.ready);
        assert_eq!(prepared.positions, 0);
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        assert!(control.memory().used() > 0);
        assert_eq!(read_all(&mut reader, width), expected);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn exact_image_readers_bound_coordinate_work_without_read_time_allocations() {
    let mut index = MemoryVectorIndex::new(1 << 19);
    index.add(4, vec![-0.0; 1 << 19]).unwrap();
    let expected = encode(&index);
    let control = StorageReadControl::with_limit(16 * 1024);
    let mut reader = index.image_reader(&control).unwrap();
    let retained = control.memory().used();
    assert!(retained < 8192);
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    let mut position = 0;
    let mut buffer = vec![0; 32 * 1024];
    let allocations = allocation_counter::measure(|| loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= 16 * 1024);
        if count == 0 {
            break;
        }
        assert_eq!(&buffer[..count], &expected[position..position + count]);
        position += count;
    });
    assert_eq!(position, expected.len());
    assert_eq!(allocations.bytes_total, 0);
    assert_eq!(control.memory().used(), full.bytes());
    drop(full);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn exact_image_readers_admit_before_boxing_and_retire_cancelled_cursors() {
    let mut index = MemoryVectorIndex::new(2);
    index.add(1, vec![1.0, -0.0]).unwrap();
    let probe = StorageReadControl::with_limit(16 * 1024);
    let reader = index.image_reader(&probe).unwrap();
    let required = probe.memory().used();
    drop(reader);
    for limit in [0, required - 1] {
        let control = StorageReadControl::with_limit(limit);
        assert!(matches!(
            index.image_reader(&control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
    for started in [false, true] {
        let control = StorageReadControl::with_limit(required);
        let mut reader = index.image_reader(&control).unwrap();
        if started {
            assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
        }
        control.cancellation().cancel();
        assert!(matches!(
            reader.read(&mut []),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(control.memory().used(), 0);
        control.cancellation().reset();
        assert!(reader.read(&mut [0; 1]).is_err());
    }
    assert_eq!(probe.memory().used(), 0);
}

#[test]
fn owned_exact_images_survive_source_mutation_and_move_without_replaying_coordinates() {
    let mut index = MemoryVectorIndex::new(2048);
    index
        .add_many(7, vec![vec![-0.0; 2048], vec![1.0; 2048]])
        .unwrap();
    let expected = encode(&index);
    let captured = StorageReadControl::with_limit(1 << 20);
    let snapshot = index.capture_image_with_control(&captured).unwrap();
    let invoking = StorageReadControl::with_limit(128 * 1024);
    let mut reader = snapshot.into_image_reader(&invoking).unwrap();
    assert!(reader.prepare(1).unwrap().ready);
    let mut prefix = [0; 31];
    assert_eq!(reader.read(&mut prefix).unwrap(), prefix.len());
    assert_eq!(prefix, expected[..prefix.len()]);
    index.clear().unwrap();
    drop(index);
    assert!(captured.memory().used() > 0);
    let suffix = std::thread::spawn(move || read_all(&mut reader, 37))
        .join()
        .unwrap();
    assert_eq!(suffix, expected[prefix.len()..]);
    assert_eq!(captured.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
    let restore = StorageReadControl::with_limit(1 << 20);
    let restored = read_vector_image(&mut expected.as_slice(), &restore).unwrap();
    assert_eq!(restored.count().unwrap(), 2);
    assert_eq!(encode(&*restored), expected);
}

#[test]
fn owned_exact_images_keep_original_cancellation_after_external_owner_loss() {
    for started in [false, true] {
        let mut index = MemoryVectorIndex::new(1);
        index.add(1, vec![-0.0]).unwrap();
        let captured = StorageReadControl::with_limit(1 << 20);
        let snapshot = index.capture_image_with_control(&captured).unwrap();
        let invoking = StorageReadControl::with_limit(128 * 1024);
        let mut reader = snapshot.into_image_reader(&invoking).unwrap();
        drop(index);
        if started {
            assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
        }
        captured.cancellation().cancel();
        assert!(matches!(
            reader.prepare(1),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(captured.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
        captured.cancellation().reset();
        assert!(reader.read(&mut [0; 1]).is_err());
    }
}

#[test]
fn retained_exact_image_readers_keep_source_control_and_do_not_pin_live_membership() {
    let registry = crate::memory_capture::MemoryCaptureRegistry::default();
    let mut original = MemoryVectorIndex::new(1);
    original.add(4, vec![-0.0]).unwrap();
    let expected = encode(&original);
    let mut registered = Box::new(original).into_memory_capture(&registry).unwrap();
    let invoking = StorageReadControl::with_limit(128 * 1024);
    let captured = StorageReadControl::with_limit(1 << 20);
    let selected = registered.capture_image_with_control(&captured).unwrap();
    let mut reader = selected.image_reader(&invoking).unwrap();
    registered.clear().unwrap();
    drop(registered);
    assert_eq!(read_all(&mut reader, 3), expected);
    assert_eq!(invoking.memory().used(), 0);
    drop(reader);
    let mut cancelled = selected.image_reader(&invoking).unwrap();
    assert_eq!(cancelled.read(&mut [0; 1]).unwrap(), 1);
    captured.cancellation().cancel();
    assert!(matches!(
        cancelled.read(&mut []),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(invoking.memory().used(), 0);
    captured.cancellation().reset();
    assert!(cancelled.read(&mut [0; 1]).is_err());
    drop(cancelled);
    drop(selected);
    assert_eq!(captured.memory().used(), 0);
}

#[test]
fn vector_image_readers_reject_unimplemented_codecs_without_buffering_or_leaks() {
    struct SynchronousOnly;
    impl crate::vector_index::image::VectorImageSource for SynchronousOnly {
        fn write_image(
            &self,
            _: &mut dyn std::io::Write,
            _: &StorageReadControl,
        ) -> crate::StorageBackendResult<u64> {
            panic!("resumable output must not buffer the synchronous writer")
        }
    }
    let original = StorageReadControl::with_limit(1 << 20);
    let invoking = StorageReadControl::with_limit(128 * 1024);
    let snapshot =
        crate::vector_index::image::VectorImageSnapshot::new(SynchronousOnly, &original).unwrap();
    assert!(snapshot.image_reader(&invoking).is_err());
    assert_eq!(invoking.memory().used(), 0);
    let mut reader = snapshot.into_image_reader(&invoking).unwrap();
    assert!(reader.prepare(1).is_err());
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
    assert!(reader.read(&mut [0; 1]).is_err());
}

#[test]
fn exact_image_readers_preserve_empty_zero_dimension_and_invalid_source_boundaries() {
    for populated in [false, true] {
        let mut index = MemoryVectorIndex::new(0);
        if populated {
            index.add_many(0, vec![Vec::new(), Vec::new()]).unwrap();
        }
        let control = StorageReadControl::with_limit(16 * 1024);
        assert_eq!(
            read_all(&mut index.image_reader(&control).unwrap(), 1),
            encode(&index)
        );
        assert_eq!(control.memory().used(), 0);
    }
    let mut index = MemoryVectorIndex::new(1 << 19);
    let mut coordinates = vec![1.0; 1 << 19];
    *coordinates.last_mut().unwrap() = f32::NAN;
    std::sync::Arc::get_mut(&mut index.state)
        .unwrap()
        .replace(1, vec![coordinates]);
    let control = StorageReadControl::with_limit(16 * 1024);
    let mut reader = index.image_reader(&control).unwrap();
    // Opening and the first read do not validate the complete coordinate payload.
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
    let mut buffer = [0; 4096];
    let mut emitted = 1;
    loop {
        match reader.read(&mut buffer) {
            Ok(count) => {
                assert_ne!(count, 0);
                emitted += count;
            }
            Err(error) => {
                assert!(error.to_string().contains("invalid memory vector image"));
                break;
            }
        }
    }
    assert!(emitted > 2 * 1024 * 1024 - 8192);
    assert_eq!(control.memory().used(), 0);
    assert!(reader.read(&mut buffer).is_err());
}
