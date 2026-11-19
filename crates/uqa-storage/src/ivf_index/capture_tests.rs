//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{IVFIndex, IVFState};
use crate::{
    memory_capture::MemoryCaptureRegistry, read_control::StorageReadControl, StorageBackendError,
    VectorIndex,
};

fn index(registry: &MemoryCaptureRegistry) -> IVFIndex {
    let mut index = IVFIndex::with_params(2, 2, 1, 4);
    index.register_memory_capture(registry).unwrap();
    for document in 0..10 {
        index
            .add(document, vec![document as f32, 10.0 - document as f32])
            .unwrap();
    }
    index.train().unwrap();
    index
}

fn image(index: &IVFIndex) -> Vec<u8> {
    let mut bytes = Vec::new();
    index
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

fn captured_image(index: &crate::vector_index::image::VectorImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    index
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

#[test]
fn ivf_capture_preserves_probe_training_and_canonical_generations_after_source_loss() {
    let registry = MemoryCaptureRegistry::new();
    let mut source = index(&registry);
    let control = StorageReadControl::with_limit(1 << 20);
    let before = image(&source);
    let first = registry.capture(&control).unwrap();
    let registration = control.memory().used();
    assert!(registration < 4096);
    source.set_nprobe(2);
    let second_bytes = image(&source);
    let second = registry.capture(&control).unwrap();
    source
        .add_many(0, vec![vec![-0.0, 1.0], vec![1.0, -0.0]])
        .unwrap();
    source.delete(1).unwrap();
    source.train().unwrap();
    let first_image = source.captured(&first).unwrap();
    let again = source.captured(&first).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first_image, &again));
    drop(again);
    let second_image = source.captured(&second).unwrap();
    source.clear().unwrap();
    drop((source, first, second, registry));
    assert_eq!(captured_image(&first_image), before);
    assert_eq!(captured_image(&second_image), second_bytes);
    let escaped = (**first_image).clone();
    assert!(control.memory().used() > 0);
    drop((first_image, second_image));
    assert!(control.memory().used() > 0);
    assert_eq!(captured_image(&escaped), before);
    drop(escaped);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn ivf_capture_precedes_query_retraining_and_ignores_rejected_or_empty_queries() {
    let registry = MemoryCaptureRegistry::new();
    let mut source = index(&registry);
    for document in 0..3 {
        source.delete(document).unwrap();
    }
    assert_eq!(source.state(), IVFState::Stale);
    let before = image(&source);
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = registry.capture(&control).unwrap();
    let registration = control.memory().used();
    assert!(source.search_knn(&[f32::NAN, 0.0], 2).is_err());
    assert!(source.search_knn(&[1.0], 2).is_err());
    assert!(source.search_knn(&[1.0, 0.0], 0).unwrap().is_empty());
    source.search_threshold(&[1.0, 0.0], 0.5).unwrap();
    assert_eq!(control.memory().used(), registration);
    assert_eq!(source.state(), IVFState::Stale);
    let expected = source.detached_clone().search_knn(&[1.0, 0.0], 3).unwrap();
    let actual = std::thread::scope(|scope| {
        let one = scope.spawn(|| source.search_knn(&[1.0, 0.0], 3).unwrap());
        let two = scope.spawn(|| source.search_knn(&[1.0, 0.0], 3).unwrap());
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_eq!(actual.0, expected);
    assert_eq!(actual.1, expected);
    assert_eq!(source.state(), IVFState::Trained);
    let saved = source.captured(&cut).unwrap();
    assert_eq!(captured_image(&saved), before);
    assert_ne!(image(&source), before);
    drop((saved, cut, source));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn ivf_capture_failure_retires_only_the_archive_and_preserves_ordinary_progress() {
    let registry = MemoryCaptureRegistry::new();
    let mut source = index(&registry);
    let control = StorageReadControl::with_limit(1024);
    let cut = registry.capture(&control).unwrap();
    source.set_nprobe(7);
    assert_eq!(source.nprobe(), 7);
    assert!(matches!(cut.check(), Err(StorageBackendError::Memory(_))));
    assert!(source.captured(&cut).is_err());
    source.add(100, vec![1.0, 0.0]).unwrap();
    source.train().unwrap();
    assert!(source.contains_document(100).unwrap());
    drop(cut);
    assert_eq!(control.memory().used(), 0);

    let control = StorageReadControl::with_limit(1 << 20);
    let cut = registry.capture(&control).unwrap();
    let saved = source.captured(&cut).unwrap();
    control.cancellation().cancel();
    let mut bytes = Vec::new();
    assert!(matches!(
        saved.write_image(&mut bytes, &StorageReadControl::with_limit(0)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(bytes.is_empty());
    source.set_nprobe(3);
    assert_eq!(source.nprobe(), 3);
    assert!(matches!(
        cut.check(),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop((cut, saved));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn ivf_capture_registration_rejects_foreign_and_earlier_cuts_without_rebinding() {
    let registry = MemoryCaptureRegistry::new();
    let control = StorageReadControl::with_limit(1 << 20);
    let early = registry.capture(&control).unwrap();
    let mut source = index(&registry);
    assert!(source.captured(&early).is_err());
    let cut = registry.capture(&control).unwrap();
    source.register_memory_capture(&registry).unwrap();
    let foreign = MemoryCaptureRegistry::new();
    assert!(source.register_memory_capture(&foreign).is_err());
    assert!(source
        .captured(&foreign.capture(&control).unwrap())
        .is_err());
    let before = image(&source);
    source.clear().unwrap();
    assert_eq!(captured_image(&source.captured(&cut).unwrap()), before);
    drop((cut, early, source));
    assert_eq!(control.memory().used(), 0);
}
