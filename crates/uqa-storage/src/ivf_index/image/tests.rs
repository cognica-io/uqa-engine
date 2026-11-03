//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete training state survives image transfer without eager retraining.

use super::*;
use crate::{
    vector_index::{image::read_vector_image, image::tests::encode},
    VectorIndex,
};

fn restore(bytes: &[u8], control: &StorageReadControl) -> Budgeted<IVFIndex> {
    let mut bytes = &bytes[13..];
    let mut input = ImageInput {
        reader: &mut bytes,
        control,
    };
    let result = read(&mut input, 2).unwrap();
    input.end().unwrap();
    result
}

#[test]
fn independent_stale_training_fixture_preserves_exact_values_and_defers_training() {
    let mut bytes = b"UQAVEC01\x01\x02\x00\x00\x00".to_vec();
    for value in [1_u64, 3, 2] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.push(2); // Stale.
    for value in [2_u64, 1, 1, 2] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(1.0_f32.to_bits().to_le_bytes());
    bytes.extend(0_u32.to_le_bytes());
    bytes.extend(1_u64.to_le_bytes()); // One remaining vector.
    bytes.extend(5_u64.to_le_bytes());
    bytes.extend(0_u32.to_le_bytes());
    for values in [[-0.0_f32, 2.0], [-0.0, 1.0]] {
        bytes.extend(2_u64.to_le_bytes());
        for value in values {
            bytes.extend(value.to_bits().to_le_bytes());
        }
    }
    bytes.extend(2.0_f32.to_bits().to_le_bytes());
    bytes.push(1);
    bytes.extend(0_u64.to_le_bytes());
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = restore(&bytes, &control);
    assert_eq!(restored.state(), IVFState::Stale);
    assert_eq!(restored.nprobe(), 3);
    assert_eq!(restored.metadata_snapshot().assignments, [(5, 0, 0)]);
    assert_eq!(encode(&*restored), bytes);
    assert_eq!(
        restored.vectors.lock()[&(5, 0)].vector[0].to_bits(),
        (-0.0_f32).to_bits()
    );
    assert_eq!(
        restored
            .search_knn(&[0.0, 1.0], 1)
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .doc_id,
        5
    );
    // One remaining vector is below the original threshold of two.
    assert_eq!(restored.state(), IVFState::Untrained);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn actual_training_states_and_reconstructed_sorted_lists_preserve_queries() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut index = IVFIndex::with_params(2, 2, 1, 8);
    for id in [9, 2, 7, 4, 1, 8] {
        index.add(id, vec![id as f32, 10.0 - id as f32]).unwrap();
    }
    assert_eq!(index.state(), IVFState::Untrained);
    let bytes = encode(&index);
    assert_eq!(restore(&bytes, &control).state(), IVFState::Untrained);
    index.train().unwrap();
    for id in [5, 3] {
        index
            .add_many(id, vec![vec![1.0, -0.0], vec![0.0, 1.0]])
            .unwrap();
    }
    assert_eq!(index.state(), IVFState::Trained);
    for state in [IVFState::Trained, IVFState::Stale] {
        if state == IVFState::Stale {
            for id in [9, 2, 7] {
                index.delete(id).unwrap();
            }
        }
        assert_eq!(index.state(), state);
        let bytes = encode(&index);
        let restored = restore(&bytes, &control);
        assert_eq!(restored.metadata_snapshot(), index.metadata_snapshot());
        assert_eq!(
            *restored.inverted_lists.lock(),
            *index.inverted_lists.lock()
        );
        assert_eq!(encode(&*restored), bytes);
        for query in [[1.0, 0.0], [0.0, 1.0]] {
            assert_eq!(
                restored.search_knn(&query, 4).unwrap(),
                index.search_knn(&query, 4).unwrap()
            );
        }
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn malformed_training_metadata_and_source_indexes_never_escape_as_images() {
    let mut index = IVFIndex::with_params(2, 2, 1, 3);
    for id in 1..=3 {
        index.add(id, vec![id as f32, 1.0]).unwrap();
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let original = encode(&index);
    for (offset, value) in [(13, 0), (21, 0), (29, 0), (37, 255)] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        assert!(read_vector_image(&mut bytes.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    index.inverted_lists.lock()[0].push((100, 0));
    let mut output = Vec::new();
    assert!(index.write_image(&mut output, &control).is_err());
    assert!(output.is_empty());
}

#[test]
fn training_lock_wait_observes_cancellation_before_output() {
    let index = IVFIndex::new(2);
    let control = StorageReadControl::with_limit(1 << 20);
    let (entered, cancelled) = std::thread::scope(|scope| {
        let guard = index.nprobe.lock();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let index = &index;
        let control = &control;
        scope.spawn(move || {
            let mut output = Vec::new();
            let result = index.write_image(&mut output, control);
            send.send((result, output)).unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !index.vectors.is_locked() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let entered = index.vectors.is_locked();
        // The first publication lock proves the reader entered admission. Keep the
        // second lock unavailable through at least one cancellable wait interval.
        let waited = receive.recv_timeout(Duration::from_millis(30)).is_err();
        control.cancellation().cancel();
        let cancelled = receive.recv_timeout(Duration::from_secs(2));
        drop(guard);
        (entered && waited, cancelled)
    });
    assert!(entered);
    let (result, output) = cancelled.unwrap();
    assert!(matches!(
        result,
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    assert!(output.is_empty());
    assert_eq!(control.memory().used(), 0);
}
