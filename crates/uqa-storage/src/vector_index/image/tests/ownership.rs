//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decoded native owners carry admission through live mutation and retained readers.

use super::{encode, read_vector_image, sources};
use crate::{
    diskann_index::{build::DiskANNTemporaryBudget, DiskANNIndexOptions, DiskANNMemoryIndex},
    memory_capture::MemoryCaptureRegistry,
    read_control::StorageReadControl,
    vector_index::{DiskANNIndexParams, MemoryVectorIndex, VectorIndexes},
    VectorIndex,
};

fn owners() -> Vec<Box<dyn VectorIndex>> {
    let mut owners = sources();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut options =
        DiskANNIndexOptions::for_parameters(DiskANNIndexParams::for_dimensions(2).unwrap());
    options.merge.sort_buffer_records = 8;
    options.generation.code_batch_nodes = 4;
    options.generation.side_batch_entries = 4;
    let temporary = DiskANNTemporaryBudget::new(8 << 20);
    let mut diskann = DiskANNMemoryIndex::new(2, options, &temporary, &control).unwrap();
    diskann.add(1, vec![1.0, 0.0]).unwrap();
    diskann.add(9, vec![0.0, 1.0]).unwrap();
    diskann.initialize().unwrap();
    owners.push(Box::new(diskann));
    owners
}

#[test]
fn imported_vector_owners_enter_live_capture_and_preserve_unread_retirement() {
    for source in owners() {
        let expected = encode(&*source);
        drop(source);
        let memory = StorageReadControl::with_limit(32 << 20);
        let restored = read_vector_image(&mut expected.as_slice(), &memory).unwrap();
        assert!(memory.memory().used() > 0);
        let registry = MemoryCaptureRegistry::new();
        let collection = registry.cell(VectorIndexes::memory(&registry)).unwrap();
        collection
            .write()
            .live_mut()
            .unwrap()
            .insert("embedding".into(), restored)
            .unwrap();
        let capture_control = StorageReadControl::with_limit(32 << 20);
        let capture = registry.capture(&capture_control).unwrap();
        let selected = collection.captured(&capture).unwrap();
        drop(collection);
        let image = selected.image("embedding", &capture).unwrap();
        drop((selected, capture, registry));
        let working = StorageReadControl::with_limit(1 << 20);
        let mut actual = Vec::new();
        image.write_image(&mut actual, &working).unwrap();
        assert_eq!(actual, expected);
        drop(image);
        assert_eq!(memory.memory().used(), 0);
        assert_eq!(capture_control.memory().used(), 0);
        assert_eq!(working.memory().used(), 0);
    }
}

#[test]
fn imported_exact_tensor_reservations_follow_shared_roots_after_owner_replacement() {
    let mut source = MemoryVectorIndex::new(1024);
    source.add(7, vec![1.0; 1024]).unwrap();
    let expected = encode(&source);
    drop(source);
    let memory = StorageReadControl::with_limit(1 << 20);
    let mut restored = read_vector_image(&mut expected.as_slice(), &memory).unwrap();
    let original = restored.snapshot().unwrap();
    let mut fork = restored.writable_snapshot().unwrap();
    restored.clear().unwrap();
    drop(restored);
    assert!(memory.memory().used() >= 1024 * size_of::<f32>());
    assert_eq!(encode(&*original), expected);
    fork.add(7, vec![-1.0; 1024]).unwrap();
    assert_eq!(encode(&*original), expected);
    drop(original);
    assert_eq!(memory.memory().used(), 0);
    assert!(fork.contains_document(7).unwrap());
    drop(fork);
}

#[test]
fn imported_vector_mutations_leave_independent_snapshots_and_images_unchanged() {
    for mut source in owners() {
        let expected = encode(&*source);
        let memory = StorageReadControl::with_limit(32 << 20);
        let mut restored = read_vector_image(&mut expected.as_slice(), &memory).unwrap();
        let snapshots = StorageReadControl::with_limit(32 << 20);
        let retained = restored.snapshot_with_control(&snapshots).unwrap();
        let image = restored.capture_image_with_control(&snapshots).unwrap();
        let observed = retained.search_knn(&[1.0, 0.0], 8).unwrap();
        for index in [&mut *source, &mut *restored] {
            index.add(41, vec![0.25, 0.75]).unwrap();
            index.delete(9).unwrap();
        }
        assert_eq!(
            restored.search_knn(&[1.0, 0.0], 8).unwrap(),
            source.search_knn(&[1.0, 0.0], 8).unwrap()
        );
        drop((source, restored));
        assert_eq!(retained.search_knn(&[1.0, 0.0], 8).unwrap(), observed);
        let working = StorageReadControl::with_limit(1 << 20);
        let mut actual = Vec::new();
        image.write_image(&mut actual, &working).unwrap();
        assert_eq!(actual, expected);
        drop((retained, image, observed));
        assert_eq!(memory.memory().used(), 0);
        assert_eq!(snapshots.memory().used(), 0);
    }
}

#[test]
fn rejected_imported_vector_registration_retires_its_original_allowance() {
    for source in owners() {
        let bytes = encode(&*source);
        let registry = MemoryCaptureRegistry::new();
        let admitted = uqa_core::memory::MemoryBudget::new(0);
        let mut indexes = VectorIndexes::memory_with_budget(&registry, &admitted);
        let memory = StorageReadControl::with_limit(32 << 20);
        let restored = read_vector_image(&mut bytes.as_slice(), &memory).unwrap();
        assert!(indexes
            .live_mut()
            .unwrap()
            .insert("embedding".into(), restored)
            .is_err());
        assert!(indexes.is_empty());
        assert_eq!(memory.memory().used(), 0);
        assert_eq!(admitted.used(), 0);
    }
}

#[test]
fn imported_vector_rejection_and_unwinding_release_every_native_owner() {
    for source in owners() {
        let bytes = encode(&*source);
        let memory = StorageReadControl::with_limit(32 << 20);
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(read_vector_image(&mut trailing.as_slice(), &memory).is_err());
        assert_eq!(memory.memory().used(), 0);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _restored = read_vector_image(&mut bytes.as_slice(), &memory).unwrap();
            assert!(memory.memory().used() > 0);
            panic!("discard the private destination");
        }));
        assert!(unwind.is_err());
        assert_eq!(memory.memory().used(), 0);
    }
}
