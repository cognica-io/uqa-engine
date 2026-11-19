//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{GraphStore, MemoryGraphStore};
use uqa_core::Vertex;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

mod input;

fn graph(value: u64) -> Arc<GraphStoreHandle> {
    let mut source = MemoryGraphStore::new();
    source.create_graph("g").unwrap();
    source.add_vertex(Vertex::new(value, "node"), "g").unwrap();
    Arc::new(GraphStoreHandle::Memory(source))
}

fn image(source: &crate::GraphImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(1))
        .unwrap();
    bytes
}

#[test]
fn graph_catalog_capture_and_selected_publication_retain_native_roots_without_enumeration() {
    for count in [1, 64, 4096] {
        let memory = MemoryBudget::new(128 << 20);
        let mut catalog = GraphCatalog::new(&memory);
        let original = graph(1);
        let expected = image(
            &original
                .capture_image_with_control(&StorageReadControl::with_limit(1 << 20))
                .unwrap(),
        );
        for id in 0..count {
            catalog
                .insert(&format!("g-{id:04}"), Arc::clone(&original))
                .unwrap();
        }
        let old = catalog.clone();
        let control = StorageReadControl::with_limit(128 << 20);
        let independent = StorageReadControl::with_limit(128 << 20);
        let mut captured = None;
        let usage = allocation_counter::measure(|| {
            captured = Some(catalog.capture(&control).unwrap());
        });
        assert_eq!(usage.count_total, 0, "{count}: {usage:?}");
        let captured = captured.unwrap();
        let exact = StorageReadControl::with_limit(control.memory().used());
        drop(catalog.capture(&exact).unwrap());
        let small = StorageReadControl::with_limit(control.memory().used() - 1);
        assert!(matches!(
            catalog.capture(&small),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
        let other = catalog.capture(&independent).unwrap();
        let replacement = graph(2);
        let usage = allocation_counter::measure(|| {
            catalog.insert("g-0000", replacement).unwrap();
        });
        assert!(usage.bytes_total < 16 << 10, "{count}: {usage:?}");
        assert!(Arc::ptr_eq(&old["g-0000"], &original));
        let child = captured.image("g-0000").unwrap().unwrap();
        let retained = control.memory().used();
        drop(captured.image("g-0000").unwrap().unwrap());
        assert_eq!(control.memory().used(), retained);
        assert_eq!(image(&child), expected);
        assert_eq!(other.names().count(), count);
        assert!(captured.image("missing").unwrap().is_none());
        drop((original, old, catalog, captured, other));
        assert_eq!(independent.memory().used(), 0);
        assert_eq!(memory.used(), 0);
        assert_eq!(image(&child), expected);
        assert!(control.memory().used() > 0);
        drop(child);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn graph_catalog_denial_preserves_membership_capability_and_original_child_control() {
    for operation in 0..3 {
        let memory = MemoryBudget::new(1 << 20);
        let mut catalog = GraphCatalog::new(&memory);
        let original = graph(1);
        for name in ["g", "other", "third"] {
            catalog.insert(name, Arc::clone(&original)).unwrap();
        }
        let previous = catalog.clone();
        let control = StorageReadControl::with_limit(1 << 20);
        let captured = catalog.capture(&control).unwrap();
        let held = memory.reserve(memory.available()).unwrap();
        let result = match operation {
            0 => catalog.insert("g", graph(2)),
            1 => catalog.remove("g").map(|_| ()),
            _ => catalog.rename("g", "renamed", graph(2)),
        };
        assert!(matches!(result, Err(StorageBackendError::Memory(_))));
        assert_eq!(catalog.len(), 3);
        assert_eq!(
            catalog.graphs.retained_bytes().unwrap(),
            previous.graphs.retained_bytes().unwrap()
        );
        for (name, source) in previous.iter() {
            assert!(std::ptr::eq(catalog.get(name).unwrap(), source));
        }
        assert!(Arc::ptr_eq(&catalog["g"], &original));
        drop(held);
        catalog.insert("g", graph(3)).unwrap();
        let child = captured.image("g").unwrap().unwrap();
        drop((catalog, captured, original, previous));
        control.cancellation().cancel();
        let mut bytes = Vec::new();
        assert!(matches!(
            child.write_image(&mut bytes, &StorageReadControl::with_limit(1 << 20)),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert!(bytes.is_empty());
        drop(child);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn graph_catalog_provider_capability_and_child_admission_follow_their_owning_generation() {
    let memory = MemoryBudget::new(1 << 20);
    let mut catalog = GraphCatalog::new(&memory);
    catalog.insert("g", graph(1)).unwrap();
    let records = Arc::new(uqa_storage::MemoryKeyValueStore::new());
    let persistent = Arc::new(GraphStoreHandle::from_catalog(
        Arc::new(uqa_storage::KeyValueCatalog::new(records.clone())),
        Arc::new(uqa_storage::KeyValueStorageBackend::new(records)),
    ));
    let held = memory.reserve(memory.available()).unwrap();
    assert!(matches!(
        catalog.insert("g", persistent.clone()),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(catalog.non_memory, 0);
    drop(held);
    let control = StorageReadControl::with_limit(1 << 20);
    let captured = catalog.capture(&control).unwrap();
    let independent_control = StorageReadControl::with_limit(1 << 20);
    let independent = catalog.capture(&independent_control).unwrap();
    catalog.insert("g", persistent).unwrap();
    assert_eq!(catalog.non_memory, 1);
    let rejected = StorageReadControl::with_limit(1 << 20);
    let usage = allocation_counter::measure(|| {
        assert!(matches!(
            catalog.capture(&rejected),
            Err(StorageBackendError::Other(_))
        ));
    });
    // Only the diagnostic string is allocated; no graph names or physical data are visited.
    assert_eq!(usage.count_total, 1);
    assert_eq!(rejected.memory().used(), 0);
    catalog.remove("g").unwrap();
    assert_eq!(catalog.non_memory, 0);
    assert!(catalog.capture(&rejected).unwrap().is_empty());
    let original = control.memory().used();
    let held = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    let usage = allocation_counter::measure(|| {
        assert!(matches!(
            captured.image("g"),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert_eq!(usage.count_total, 0);
    drop(held);
    assert_eq!(control.memory().used(), original);
    let child = captured.image("g").unwrap().unwrap();
    let other = independent.image("g").unwrap().unwrap();
    let expected = image(&child);
    let used = control.memory().used();
    let usage = allocation_counter::measure(|| {
        drop(captured.image("g").unwrap().unwrap());
    });
    assert_eq!(usage.count_total, 0);
    assert_eq!(control.memory().used(), used);
    drop((catalog, captured, independent));
    control.cancellation().cancel();
    assert_eq!(image(&other), expected);
    let mut bytes = Vec::new();
    assert!(matches!(
        child.write_image(&mut bytes, &StorageReadControl::with_limit(1 << 20)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(bytes.is_empty());
    drop((child, other));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(independent_control.memory().used(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn graph_catalog_restoration_reuses_unchanged_membership_and_discards_failed_successors() {
    let records = Arc::new(uqa_storage::MemoryKeyValueStore::new());
    let catalog: Arc<dyn CatalogFacade> =
        Arc::new(uqa_storage::KeyValueCatalog::new(records.clone()));
    let backend: Arc<dyn PersistentStorageBackend> =
        Arc::new(uqa_storage::KeyValueStorageBackend::new(records));
    let memory = MemoryBudget::new(1 << 20);
    let mut graphs = GraphCatalog::new(&memory);
    for name in ["g", "h", "z"] {
        catalog.save_named_graph(name).unwrap();
        graphs
            .insert(
                name,
                Arc::new(GraphStoreHandle::from_catalog(
                    catalog.clone(),
                    backend.clone(),
                )),
            )
            .unwrap();
    }
    let original = memory.used();
    let held = memory.reserve(memory.available()).unwrap();
    assert!(graphs
        .restored(catalog.as_ref(), || panic!(
            "unchanged membership needs no new handle"
        ))
        .unwrap()
        .is_none());
    catalog.drop_named_graph("g").unwrap();
    catalog.save_named_graph("later").unwrap();
    assert!(matches!(
        graphs.restored(catalog.as_ref(), || Ok(GraphStoreHandle::from_catalog(
            catalog.clone(),
            backend.clone()
        ))),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(
        graphs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["g", "h", "z"]
    );
    drop(held);
    assert_eq!(memory.used(), original);
    assert!(matches!(
        graphs.restored(catalog.as_ref(), || Err(StorageBackendError::Other(
            "unavailable physical session".into()
        ))),
        Err(StorageBackendError::Other(_))
    ));
    assert_eq!(memory.used(), original);
    let restored = graphs
        .restored(catalog.as_ref(), || {
            Ok(GraphStoreHandle::from_catalog(
                catalog.clone(),
                backend.clone(),
            ))
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        restored.keys().map(String::as_str).collect::<Vec<_>>(),
        ["h", "later", "z"]
    );
    assert!(Arc::ptr_eq(&graphs["h"], &restored["h"]));
    assert_eq!(graphs.len(), 3);
    drop((graphs, restored));
    assert_eq!(memory.used(), 0);
}
