//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded preparation retains evaluated IVF observations across arbitrary output windows.

use super::{restore, stale_fixture, IVFData, IVFIndex, IVFState, StorageReadControl, VectorIndex};
use crate::{
    image_io::ImageReader,
    ivf_index::image::{reader, write},
    StorageBackendError,
};

fn prepare(reader: &mut ImageReader<'_>, maximum: usize) -> usize {
    let mut visited = 0;
    loop {
        let progress = reader.prepare(maximum).unwrap();
        assert!(progress.positions <= maximum);
        visited += progress.positions;
        if progress.ready {
            break;
        }
        assert_ne!(progress.positions, 0);
    }
    assert_eq!(reader.prepare(maximum).unwrap().positions, 0);
    visited
}

fn output(reader: &mut ImageReader<'_>, width: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buffer = vec![0; width];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= width.min(16 * 1024));
        if count == 0 {
            return result;
        }
        result.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn ivf_image_readers_preserve_independent_stale_state_across_all_window_boundaries() {
    let expected = stale_fixture();
    let restored_control = StorageReadControl::with_limit(1 << 20);
    let index = restore(&expected, &restored_control);
    let original = StorageReadControl::with_limit(1 << 20);
    let snapshot = index.capture_image_with_control(&original).unwrap();
    let mut visited = None;
    for (width, maximum) in [
        (1, 1),
        (3, 2),
        (13, 7),
        (62, 128),
        (4093, 3),
        (32 * 1024, 1),
    ] {
        let invoking = StorageReadControl::with_limit(16 * 1024);
        let mut reader = snapshot.image_reader(&invoking).unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let positions = prepare(&mut reader, maximum);
        assert_eq!(*visited.get_or_insert(positions), positions);
        assert_eq!(output(&mut reader, width), expected);
        assert_eq!(invoking.memory().used(), 0);
    }
    assert_eq!(index.state(), IVFState::Stale);
    drop((index, snapshot));
    assert_eq!(original.memory().used(), 0);
    assert_eq!(restored_control.memory().used(), 0);
}

#[test]
fn ivf_image_preparation_bounds_empty_centroids_and_membership_buckets() {
    let mut index = IVFData::with_params(0, 4096, 1, 1);
    index.state = IVFState::Trained;
    index.centroids = vec![Vec::new(); 4096];
    index.inverted_lists = vec![Vec::new(); 4096];
    let mut expected = Vec::new();
    write(&index, &mut expected, &StorageReadControl::with_limit(0)).unwrap();
    let control = StorageReadControl::with_limit(16 * 1024);
    let mut reader = reader(&index, &control).unwrap();
    let used = control.memory().used();
    assert!(used < 8192);
    let full = control
        .memory()
        .reserve(control.memory().limit() - used)
        .unwrap();
    let mut buffer = [0; 4096];
    let mut emitted = 0;
    let allocation = allocation_counter::measure(|| {
        assert!(prepare(&mut reader, 1) >= 8192);
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            assert_eq!(&buffer[..count], &expected[emitted..emitted + count]);
            emitted += count;
        }
    });
    assert_eq!(emitted, expected.len());
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(control.memory().used(), full.bytes());
    drop(full);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn ivf_image_coordinate_preparation_and_output_remain_bounded_without_heap_scratch() {
    let mut index = IVFIndex::with_params(1 << 19, 1, 1, 2);
    index.add(3, vec![-0.0; 1 << 19]).unwrap();
    let data = index.data.read();
    let mut expected = Vec::new();
    write(&data, &mut expected, &StorageReadControl::with_limit(0)).unwrap();
    let control = StorageReadControl::with_limit(16 * 1024);
    let mut reader = reader(&data, &control).unwrap();
    let full = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    let mut buffer = vec![0; 32 * 1024];
    let mut emitted = 0;
    let allocation = allocation_counter::measure(|| {
        let positions = prepare(&mut reader, 1);
        assert!((1024..1040).contains(&positions));
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
    assert_eq!(allocation.bytes_total, 0);
    assert!(emitted > 4 * 1024 * 1024);
    assert_eq!(emitted, expected.len());
    assert_eq!(control.memory().used(), full.bytes());
    drop(full);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn ivf_image_readers_reject_corrupt_assignments_before_any_output() {
    let corruptions: &[fn(&mut IVFData)] = &[
        |data| data.nprobe = 0,
        |data| {
            data.centroids[0].pop();
        },
        |data| data.vectors.get_mut(&(5, 0)).unwrap().doc_id = 6,
        |data| data.vectors.get_mut(&(5, 0)).unwrap().norm = f32::NAN,
        |data| data.vectors.get_mut(&(5, 0)).unwrap().norm = -1.0,
        |data| data.vectors.get_mut(&(5, 0)).unwrap().centroid = None,
        |data| data.vectors.get_mut(&(5, 0)).unwrap().raw_vector[1] = f32::INFINITY,
        |data| data.vectors.get_mut(&(5, 0)).unwrap().vector[1] = f32::NEG_INFINITY,
        |data| data.inverted_lists[0].push((5, 0)),
        |data| data.inverted_lists[0].push((6, 0)),
        |data| data.inverted_lists[0].clear(),
    ];
    for corrupt in corruptions {
        let original = StorageReadControl::with_limit(1 << 20);
        let index = restore(&stale_fixture(), &original);
        let mut data = index.data.write();
        corrupt(&mut data);
        let control = StorageReadControl::with_limit(16 * 1024);
        let mut reader = reader(&data, &control).unwrap();
        let mut buffer = [19; 64];
        assert!(reader.read(&mut buffer).is_err());
        assert_eq!(buffer, [19; 64]);
        assert_eq!(control.memory().used(), 0);
        assert!(reader.prepare(1).is_err());
        let mut bytes = Vec::new();
        assert!(write(&data, &mut bytes, &control).is_err());
        assert!(bytes.is_empty());
    }
}

#[test]
fn ivf_image_preparation_preserves_absolute_coordinate_errors_and_terminal_admission() {
    let mut index = IVFIndex::with_params(2048, 1, 1, 2);
    index.add(1, vec![1.0; 2048]).unwrap();
    let mut data = index.data.write();
    data.vectors.get_mut(&(1, 0)).unwrap().raw_vector[1500] = f32::NAN;
    let control = StorageReadControl::with_limit(16 * 1024);
    let mut reader = reader(&data, &control).unwrap();
    let required = control.memory().used();
    assert!(reader.prepare(1).is_ok());
    let mut positions = 1;
    loop {
        match reader.prepare(1) {
            Ok(progress) => {
                assert!(!progress.ready);
                positions += progress.positions;
            }
            Err(error) => {
                assert_eq!(
                    error.to_string(),
                    "vector component 1500 must be finite, got NaN"
                );
                break;
            }
        }
    }
    assert!(positions >= 4);
    assert_eq!(control.memory().used(), 0);
    assert!(reader.read(&mut [0; 1]).is_err());
    let denied = StorageReadControl::with_limit(required - 1);
    assert!(matches!(
        crate::ivf_index::image::reader(&data, &denied),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(denied.memory().used(), 0);
}

#[test]
fn owned_ivf_image_readers_survive_live_retraining_and_cross_thread_retirement() {
    let expected = stale_fixture();
    let restored_control = StorageReadControl::with_limit(1 << 20);
    let index = restore(&expected, &restored_control);
    let original = StorageReadControl::with_limit(1 << 20);
    let invoking = StorageReadControl::with_limit(128 * 1024);
    let mut reader = index
        .capture_image_with_control(&original)
        .unwrap()
        .into_image_reader(&invoking)
        .unwrap();
    assert!(!reader.prepare(1).unwrap().ready);
    index.search_knn(&[0.0, 1.0], 1).unwrap();
    assert_eq!(index.state(), IVFState::Untrained);
    drop(index);
    assert_eq!(restored_control.memory().used(), 0);
    let mut prefix = [0; 63];
    assert_eq!(reader.read(&mut prefix).unwrap(), prefix.len());
    assert_eq!(prefix, expected[..prefix.len()]);
    let suffix = std::thread::spawn(move || output(&mut reader, 3))
        .join()
        .unwrap();
    assert_eq!(suffix, expected[prefix.len()..]);
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
    let restored = restore(&expected, &restored_control);
    assert_eq!(restored.state(), IVFState::Stale);
    assert_eq!(restored.metadata_snapshot().assignments, [(5, 0, 0)]);
}

#[test]
fn live_ivf_image_readers_release_state_admission_before_query_retraining() {
    let expected = stale_fixture();
    let original = StorageReadControl::with_limit(1 << 20);
    let index = restore(&expected, &original);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut reader = index.image_reader(&control).unwrap();
    index.search_knn(&[0.0, 1.0], 1).unwrap();
    assert_eq!(index.state(), IVFState::Untrained);
    assert_eq!(output(&mut reader, 1), expected);
    assert_eq!(control.memory().used(), 0);
    drop(reader);
    drop(index);
    assert_eq!(original.memory().used(), 0);
}

#[test]
fn owned_ivf_image_readers_retire_both_controls_during_every_preparation_boundary() {
    for original_cancel in [false, true] {
        for progress in 0..4 {
            let restored_control = StorageReadControl::with_limit(1 << 20);
            let index = restore(&stale_fixture(), &restored_control);
            let original = StorageReadControl::with_limit(1 << 20);
            let invoking = StorageReadControl::with_limit(128 * 1024);
            let mut reader = index
                .capture_image_with_control(&original)
                .unwrap()
                .into_image_reader(&invoking)
                .unwrap();
            drop(index);
            match progress {
                1 => {
                    assert!(!reader.prepare(1).unwrap().ready);
                }
                2 => {
                    prepare(&mut reader, 1);
                }
                3 => {
                    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
                }
                _ => {}
            }
            let cancelled = if original_cancel {
                &original
            } else {
                &invoking
            };
            cancelled.cancellation().cancel();
            assert!(matches!(
                reader.read(&mut []),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(original.memory().used(), 0);
            assert_eq!(invoking.memory().used(), 0);
            assert_eq!(restored_control.memory().used(), 0);
            cancelled.cancellation().reset();
            assert!(reader.prepare(1).is_err());
        }
    }
}
