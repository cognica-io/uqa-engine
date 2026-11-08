//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resumable output preserves the predecessor's sealed bytes and original resource owners.

use super::*;
use crate::image_io::ImageReader;

// Captured with populated() and the synchronous encoder at bfbbab1519defa10e3c7945fce3f4eeeb417951b.
// SHA-256: b47336d76e29efd63ce346d0d2e427e5544463d05013e40874a2b8be15400432.
const EVALUATED: &[u8] = include_bytes!("fixtures/evaluated.uqa");

fn prepare(reader: &mut ImageReader<'_>, maximum: usize) -> usize {
    let mut positions = 0;
    loop {
        let progress = reader.prepare(maximum).unwrap();
        assert!(progress.positions <= maximum);
        positions += progress.positions;
        if progress.ready {
            break;
        }
        assert_ne!(progress.positions, 0);
    }
    assert_eq!(reader.prepare(maximum).unwrap().positions, 0);
    positions
}

fn output(reader: &mut ImageReader<'_>, width: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = vec![0; width];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= width.min(16 * 1024));
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    bytes
}

#[test]
fn memory_diskann_image_readers_preserve_predecessor_bytes_across_every_window_boundary() {
    let original = StorageReadControl::with_limit(1 << 20);
    let source = restore(EVALUATED, &original, &DiskANNTemporaryBudget::new(1 << 20));
    assert_eq!(encode(&source), EVALUATED);
    let snapshot = source.capture_image_with_control(&original).unwrap();
    let mut positions = None;
    for (width, maximum) in [
        (1, 1),
        (3, 2),
        (61, 7),
        (4093, 128),
        (16 * 1024, 3),
        (32 * 1024, 1),
    ] {
        let invoking = StorageReadControl::with_limit(128 * 1024);
        let mut reader = snapshot.image_reader(&invoking).unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let visited = prepare(&mut reader, maximum);
        assert_eq!(*positions.get_or_insert(visited), visited);
        assert_eq!(output(&mut reader, width), EVALUATED);
        assert_eq!(invoking.memory().used(), 0);
    }
    let invoking = StorageReadControl::with_limit(128 * 1024);
    assert_eq!(
        output(&mut source.image_reader(&invoking).unwrap(), 17),
        EVALUATED
    );
    drop((snapshot, source));
    assert_eq!(original.memory().used(), 0);
}

#[test]
fn memory_diskann_image_preparation_bounds_empty_tensors_origin_batches_and_uncovered_gaps() {
    let original = StorageReadControl::with_limit(16 << 20);
    let mut source = new(&original);
    for document in (1024..2048).step_by(4) {
        source.add_many(document, Vec::new()).unwrap();
    }
    source.initialize().unwrap();
    assert_eq!(source.manifest().origins().unwrap().documents(), 256);
    for document in 0..4096 {
        if !(1024..2048).contains(&document) || document % 4 != 0 {
            source.add_many(document, Vec::new()).unwrap();
        }
    }
    let expected = encode(&source);
    let invoking = StorageReadControl::with_limit(128 * 1024);
    let mut reader = source.image_reader(&invoking).unwrap();
    let full = invoking
        .memory()
        .reserve(invoking.memory().limit() - invoking.memory().used())
        .unwrap();
    let source_full = original
        .memory()
        .reserve(original.memory().limit() - original.memory().used())
        .unwrap();
    let mut buffer = vec![0; 32 * 1024];
    let mut emitted = 0;
    let allocation = allocation_counter::measure(|| {
        let positions = prepare(&mut reader, 1);
        // Both ordered document passes, all changes and all sealed origins must advance individually.
        assert!((2 * 4096 + 3840 + 256..2 * 4096 + 3840 + 256 + 32).contains(&positions));
        loop {
            let count = reader.read(&mut buffer).unwrap();
            assert!(count <= 16 * 1024);
            if count == 0 {
                break;
            }
            assert_eq!(&buffer[..count], &expected[emitted..emitted + count]);
            emitted += count;
        }
    });
    assert_eq!(emitted, expected.len());
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(invoking.memory().used(), full.bytes());
    drop(reader);
    drop((source_full, full, source));
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
}

#[test]
fn memory_diskann_image_coordinate_output_uses_fixed_scratch_without_copying_the_tensor() {
    let original = StorageReadControl::with_limit(16 << 20);
    let mut source = DiskANNMemoryIndex::new(
        1024,
        options(1024),
        &DiskANNTemporaryBudget::new(1 << 20),
        &original,
    )
    .unwrap();
    source.add_many(7, vec![vec![-0.0; 1024]; 1024]).unwrap();
    let expected = encode(&source);
    assert!(expected.len() > 4 << 20);
    let invoking = StorageReadControl::with_limit(128 * 1024);
    let mut reader = source.image_reader(&invoking).unwrap();
    assert!(invoking.memory().used() < 32 * 1024);
    let full = invoking
        .memory()
        .reserve(invoking.memory().limit() - invoking.memory().used())
        .unwrap();
    let mut buffer = vec![0; 32 * 1024];
    let mut emitted = 0;
    let allocation = allocation_counter::measure(|| {
        assert!(prepare(&mut reader, 1) < 16);
        loop {
            let count = reader.read(&mut buffer).unwrap();
            assert!(count <= 16 * 1024);
            if count == 0 {
                break;
            }
            assert_eq!(&buffer[..count], &expected[emitted..emitted + count]);
            emitted += count;
        }
    });
    assert_eq!(emitted, expected.len());
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(invoking.memory().used(), full.bytes());
    drop(reader);
    drop((full, source));
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
}

#[test]
fn owned_memory_diskann_image_readers_preserve_selected_state_after_rebuild_and_thread_transfer() {
    let original = StorageReadControl::with_limit(4 << 20);
    let capture = StorageReadControl::with_limit(4096);
    let invoking = StorageReadControl::with_limit(128 * 1024);
    let mut source = populated(&original);
    let expected = encode(&source);
    let mut reader = source
        .capture_image_with_control(&capture)
        .unwrap()
        .into_image_reader(&invoking)
        .unwrap();
    assert!(!reader.prepare(1).unwrap().ready);
    source.add(1, vec![-1.0, 0.0]).unwrap();
    source.initialize().unwrap();
    source.clear().unwrap();
    drop(source);
    assert_ne!(original.memory().used(), 0);
    let actual = std::thread::spawn(move || {
        prepare(&mut reader, 1);
        output(&mut reader, 4093)
    })
    .join()
    .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(original.memory().used(), 0);
    assert_eq!(capture.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
}

#[test]
fn owned_memory_diskann_image_readers_retire_all_controls_at_every_preparation_and_output_boundary()
{
    for owner in 0..3 {
        for boundary in 0..5 {
            let original = StorageReadControl::with_limit(1 << 20);
            let capture = StorageReadControl::with_limit(4096);
            let invoking = StorageReadControl::with_limit(128 * 1024);
            let source = populated(&original);
            let mut reader = source
                .capture_image_with_control(&capture)
                .unwrap()
                .into_image_reader(&invoking)
                .unwrap();
            drop(source);
            if boundary == 1 {
                assert!(!reader.prepare(1).unwrap().ready);
            }
            if boundary >= 2 {
                prepare(&mut reader, 1);
            }
            if boundary >= 3 {
                let mut buffer = vec![0; if boundary == 3 { 1 } else { 4093 }];
                assert_eq!(reader.read(&mut buffer).unwrap(), buffer.len());
            }
            let token = [&original, &capture, &invoking][owner].cancellation();
            token.cancel();
            assert!(matches!(
                reader.read(&mut []),
                Err(StorageBackendError::Cancelled(_))
            ));
            token.reset();
            assert!(reader.prepare(1).is_err());
            assert!(reader.read(&mut [0; 1]).is_err());
            assert_eq!(original.memory().used(), 0);
            assert_eq!(capture.memory().used(), 0);
            assert_eq!(invoking.memory().used(), 0);
        }
    }
}

#[test]
fn memory_diskann_image_admission_charges_exact_cursor_and_manifest_before_output() {
    let original = StorageReadControl::with_limit(1 << 20);
    let source = populated(&original);
    let probe = StorageReadControl::with_limit(128 * 1024);
    let reader = source.image_reader(&probe).unwrap();
    let required = probe.memory().used();
    drop(reader);
    assert_eq!(probe.memory().used(), 0);
    for bytes in [0, required - 1] {
        let denied = StorageReadControl::with_limit(bytes);
        assert!(source.image_reader(&denied).is_err());
        assert_eq!(denied.memory().used(), 0);
    }
    let exact = StorageReadControl::with_limit(required);
    let mut reader = source.image_reader(&exact).unwrap();
    assert_eq!(output(&mut reader, 32 * 1024), encode(&source));
    assert_eq!(exact.memory().used(), 0);
    original.cancellation().cancel();
    exact.cancellation().cancel();
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
    assert!(reader.prepare(1).unwrap().ready);
}

#[test]
fn memory_diskann_synchronous_images_preserve_cancellation_during_partial_and_interrupted_writes() {
    struct Writer<'a> {
        owner: &'a StorageReadControl,
        interrupted: bool,
        calls: usize,
    }
    impl std::io::Write for Writer<'_> {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            assert_eq!(
                self.calls, 1,
                "cancellation must precede another output attempt"
            );
            self.owner.cancellation().cancel();
            if self.interrupted {
                Err(std::io::ErrorKind::Interrupted.into())
            } else {
                Ok(1)
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for owner in 0..3 {
        for interrupted in [false, true] {
            let original = StorageReadControl::with_limit(1 << 20);
            let capture = StorageReadControl::with_limit(4096);
            let invoking = StorageReadControl::with_limit(128 * 1024);
            let source = populated(&original);
            let image = source.capture_image_with_control(&capture).unwrap();
            drop(source);
            let mut writer = Writer {
                owner: [&original, &capture, &invoking][owner],
                interrupted,
                calls: 0,
            };
            assert!(matches!(
                image.write_image(&mut writer, &invoking),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(writer.calls, 1);
            assert_eq!(invoking.memory().used(), 0);
            drop(image);
            assert_eq!(original.memory().used(), 0);
            assert_eq!(capture.memory().used(), 0);
        }
    }
}
