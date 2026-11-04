//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{DeepLayerSpec, GatingSpec};
use uqa_storage::StorageBackendError;

fn model(capacity: usize) -> Arc<DeepModel> {
    let mut weights = Vec::with_capacity(capacity);
    weights.push(-0.0);
    Arc::new(DeepModel {
        layers: vec![
            DeepLayerSpec::Input { dimensions: 1 },
            DeepLayerSpec::Dense {
                weights,
                bias: vec![f64::from_bits(1)],
                output_channels: 1,
                input_channels: 1,
            },
        ],
        alpha: 0.5,
        gating: GatingSpec::None,
    })
}

fn image(source: &ModelImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

#[test]
fn model_catalog_capture_and_publication_do_not_traverse_unrelated_layers() {
    for entries in [1, 64, 4096] {
        let mut source = ModelCatalog::default();
        for key in 0..entries {
            source.insert(format!("model-{key:05}"), model(1)).unwrap();
        }
        source.insert("large".into(), model(1 << 19)).unwrap();
        let first = StorageReadControl::with_limit(16 << 20);
        let second = StorageReadControl::with_limit(16 << 20);
        let mut captured = None;
        let usage =
            allocation_counter::measure(|| captured = Some(source.capture(&first).unwrap()));
        assert_eq!(usage.count_total, 0, "{entries}: {usage:?}");
        let captured = captured.unwrap();
        let independent = source.capture(&second).unwrap();
        assert_eq!(first.memory().used(), second.memory().used());
        assert!(first.memory().used() > 4 << 20);
        let original = source.clone();
        let name = "model-00000".to_owned();
        let replacement = model(1);
        let usage = allocation_counter::measure(|| source.insert(name, replacement).unwrap());
        assert!(usage.bytes_total < 32768, "{entries}: {usage:?}");
        assert!(Arc::ptr_eq(
            source.get("large").unwrap(),
            original.get("large").unwrap()
        ));
        let usage = allocation_counter::measure(|| assert!(source.remove("large").unwrap()));
        assert!(usage.bytes_total < 32768, "{usage:?}");
        source.clear();
        drop((source, original));
        assert_eq!(captured.len(), entries + 1);
        assert!(captured.names().eq(independent.names()));
        assert_eq!(
            image(&captured.image("large").unwrap().unwrap()),
            image(&independent.image("large").unwrap().unwrap())
        );
        drop(captured);
        assert_eq!(first.memory().used(), 0);
        drop(independent);
        assert_eq!(second.memory().used(), 0);
    }
}

#[test]
fn detached_model_images_transfer_payload_leases_and_release_unselected_models() {
    let mut source = ModelCatalog::default();
    let selected = model(1 << 18);
    let unread = model(1 << 18);
    let selected_weak = Arc::downgrade(&selected);
    let unread_weak = Arc::downgrade(&unread);
    source.insert("selected".into(), selected).unwrap();
    source.insert("unread".into(), unread).unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let captured = source.capture(&control).unwrap();
    let retained = control.memory().used();
    assert!(retained > 4 << 20);
    // Failure at handle, key or node admission cannot consume the root's payload lease.
    for spare in [0, 64] {
        let occupied = control
            .memory()
            .reserve(control.memory().available() - spare)
            .unwrap();
        let usage = allocation_counter::measure(|| assert!(captured.image("selected").is_err()));
        if spare == 0 {
            assert_eq!(usage.count_total, 0, "denied first read: {usage:?}");
        }
        drop(occupied);
        assert_eq!(control.memory().used(), retained);
    }
    assert!(captured.image("absent").unwrap().is_none());
    let selected = captured.image("selected").unwrap().unwrap();
    let after = control.memory().used();
    assert!(after > retained && after - retained < 4096);
    let same = captured.image("selected").unwrap().unwrap();
    assert_eq!(control.memory().used(), after);
    drop((source, captured));
    assert!(unread_weak.upgrade().is_none());
    assert!(selected_weak.upgrade().is_some());
    assert!(control.memory().used() > 2 << 20);
    assert!(control.memory().used() < (2 << 20) + 4096);
    assert_eq!(image(&selected), image(&same));
    control.cancellation().cancel();
    let mut bytes = Vec::new();
    assert!(matches!(
        same.write_image(&mut bytes, &StorageReadControl::with_limit(0)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(bytes.is_empty());
    drop((selected, same));
    assert!(selected_weak.upgrade().is_none());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn model_catalog_admission_is_independent_and_preserves_failed_native_updates() {
    let construction = MemoryBudget::new(8192);
    let mut source = ModelCatalog::new(&construction);
    source.insert("a".into(), model(1)).unwrap();
    source.insert("saved".into(), model(1 << 18)).unwrap();
    source.insert("z".into(), model(1)).unwrap();
    let first = StorageReadControl::with_limit(4 << 20);
    let captured = source.capture(&first).unwrap();
    let retained = first.memory().used();
    let denied = StorageReadControl::with_limit(retained - 1);
    assert!(source.capture(&denied).is_err());
    assert_eq!(denied.memory().used(), 0);
    let occupied = construction.reserve(construction.available()).unwrap();
    assert!(source.insert("saved".into(), model(1)).is_err());
    assert!(source.insert("new".into(), model(1)).is_err());
    assert!(source.remove("saved").is_err());
    assert!(source.contains_key("saved"));
    assert!(!source.contains_key("new"));
    drop(occupied);
    let exact = StorageReadControl::with_limit(retained);
    let independent = source.capture(&exact).unwrap();
    assert_eq!(exact.memory().used(), retained);
    assert!(independent.image("saved").is_err());
    assert_eq!(exact.memory().used(), retained);
    first.cancellation().cancel();
    assert!(source.capture(&first).is_err());
    assert!(matches!(
        captured.image("saved"),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(source.remove("saved").unwrap());
    drop((source, captured, independent));
    assert_eq!(construction.used(), 0);
    assert_eq!(first.memory().used(), 0);
    assert_eq!(exact.memory().used(), 0);
}

#[test]
fn concurrent_model_image_selection_keeps_one_payload_lease_until_the_last_child() {
    let mut source = ModelCatalog::default();
    source.insert("saved".into(), model(1 << 18)).unwrap();
    let sequential = StorageReadControl::with_limit(4 << 20);
    let solo = source.capture(&sequential).unwrap();
    let retained = sequential.memory().used();
    let child = solo.image("saved").unwrap().unwrap();
    let metadata = sequential.memory().used() - retained;
    drop((child, solo));
    assert_eq!(sequential.memory().used(), 0);
    let control = StorageReadControl::with_limit(4 << 20);
    let captured = Arc::new(source.capture(&control).unwrap());
    let used = control.memory().used();
    let ready = Arc::new(std::sync::Barrier::new(8));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let captured = captured.clone();
        let ready = ready.clone();
        workers.push(std::thread::spawn(move || {
            ready.wait();
            captured.image("saved").unwrap().unwrap()
        }));
    }
    let mut children: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(control.memory().used(), used + metadata);
    let first = children.pop().unwrap();
    drop((source, captured));
    for child in &children {
        assert_eq!(image(child), image(&first));
    }
    drop(first);
    assert!(control.memory().used() > 2 << 20);
    drop(children);
    assert_eq!(control.memory().used(), 0);
}
