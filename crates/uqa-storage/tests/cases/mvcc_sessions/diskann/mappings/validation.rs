//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn diskann_mapping_cleanup_rejects_corrupt_guards_and_keeps_its_original_key() {
    for bytes in [vec![], vec![0; 8], vec![1; 9]] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let (_repository, _scope, generation) = retired(&store, &control);
        let guard = store.scan_prefix(b"\0uqa-diskann-v1\0\x07").unwrap()[0]
            .0
            .clone();
        store.put(&guard, &bytes).unwrap();
        let before = store.scan_prefix(b"\0uqa-diskann-v1\0").unwrap();
        let mut pass = KeyValueDiskANNMappingMaintenance::start(&store, &control).unwrap();
        assert!(pass.step().is_err());
        assert_eq!(store.scan_prefix(b"\0uqa-diskann-v1\0").unwrap(), before);
        store
            .put(&guard, &generation.generation().to_be_bytes())
            .unwrap();
        assert_eq!(pass.step().unwrap(), Some(true));
        assert_eq!(pass.step().unwrap(), Some(true));
        assert_eq!(pass.step().unwrap(), None);
    }
}

#[test]
fn diskann_mapping_cleanup_rejects_incomplete_catalog_identity_without_deleting_records() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(1 << 20);
    let (_repository, _scope, _generation) = retired(&store, &control);
    let mut key = store.scan_prefix(b"\0uqa-diskann-v1\0\x03").unwrap()[0]
        .0
        .clone();
    key.pop();
    store.put(&key, b"invalid").unwrap();
    let before = store.scan_prefix(b"\0uqa-diskann-v1\0").unwrap();
    assert!(KeyValueDiskANNMappingMaintenance::run(&store, &control).is_err());
    assert_eq!(store.scan_prefix(b"\0uqa-diskann-v1\0").unwrap(), before);
}

#[test]
fn diskann_mapping_cleanup_preserves_original_controls_and_accepts_guardless_predecessors() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(1 << 20);
    let (_repository, _scope, _generation) = retired(&store, &control);
    let before = store.scan_prefix(b"\0uqa-diskann-v1\0").unwrap();
    assert!(
        KeyValueDiskANNMappingMaintenance::start(&store, &StorageReadControl::with_limit(0))
            .is_err()
    );
    let cancelled = StorageReadControl::with_limit(8192);
    let mut pass = KeyValueDiskANNMappingMaintenance::start(&store, &cancelled).unwrap();
    cancelled.cancellation().cancel();
    assert!(matches!(
        pass.step(),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(pass);
    assert_eq!(cancelled.memory().used(), 0);
    assert_eq!(store.scan_prefix(b"\0uqa-diskann-v1\0").unwrap(), before);
    for prefix in [b"\0uqa-diskann-v1\0\x06", b"\0uqa-diskann-v1\0\x07"] {
        for (key, _) in store.scan_prefix(prefix).unwrap() {
            store.delete(&key).unwrap();
        }
    }
    KeyValueDiskANNMappingMaintenance::run(&store, &control).unwrap();
    for prefix in [b"\0uqa-diskann-v1\0\x02", b"\0uqa-diskann-v1\0\x03"] {
        assert!(store.scan_prefix(prefix).unwrap().is_empty());
    }
}

#[test]
fn diskann_bound_stage_outlives_private_catalog_undo_but_cannot_restart_after_discard() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(1 << 20);
    store.begin_transaction().unwrap();
    let scope = super::super::identity::scope(&store, &control);
    let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
    repository.initialize(&control).unwrap();
    let mut stage = repository.allocate_bound_stage(&scope, &control).unwrap();
    store.rollback_transaction().unwrap();
    assert!(store.get(b"catalog-fixture").unwrap().is_none());
    KeyValueDiskANNMaintenance::run(&store, &control).unwrap();
    assert_eq!(
        stage.status(&control).unwrap(),
        Some(DiskANNStageStatus::Writing)
    );
    assert!(stage.discard_step(64, &control).unwrap());
    KeyValueDiskANNMaintenance::run(&store, &control).unwrap();
    assert!(stage.start(&control).is_err());
    for prefix in [b"\0uqa-diskann-v1\0\x02", b"\0uqa-diskann-v1\0\x03"] {
        assert!(store.scan_prefix(prefix).unwrap().is_empty());
    }
}
