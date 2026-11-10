//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{GraphCatalog, GraphStoreError, GraphStoreHandle, LabelKind};
use std::sync::Arc;
use uqa_core::memory::{MemoryBudget, ProductionControl};

fn image(source: &crate::GraphImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

fn name(control: &StorageReadControl) -> uqa_core::memory::Budgeted<String> {
    ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text("g")
    .unwrap()
    .into_budgeted()
    .unwrap()
}

fn encoded() -> Vec<u8> {
    let mut source = MemoryGraphStore::new();
    source.create_graph("g").unwrap();
    source.add_vertex(Vertex::new(1, "p"), "g").unwrap();
    source.add_vertex(Vertex::new(2, "p"), "g").unwrap();
    source
        .add_edge(uqa_core::Edge::new(3, 1, 2, "r"), "g")
        .unwrap();
    source.create_label("g", "old", LabelKind::Vertex).unwrap();
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

#[test]
fn graph_input_keeps_native_headers_and_all_selected_mutations_after_original_allowance_exhaustion()
{
    let input = StorageReadControl::with_limit(1 << 20);
    let runtime = MemoryBudget::new(1 << 20);
    let catalog = GraphCatalog::new(input.memory())
        .read_image(name(&input), &mut encoded().as_slice(), &runtime, &input)
        .unwrap();
    let mut handle = catalog.get("g").unwrap().clone();
    drop(catalog);
    let full = input.memory().reserve(input.memory().available()).unwrap();
    input.cancellation().cancel();
    let GraphStoreHandle::Memory(store) = Arc::get_mut(&mut handle).unwrap() else {
        unreachable!()
    };
    let before = store
        .image_memory
        .as_ref()
        .map(uqa_core::memory::MemoryReservation::bytes);
    assert_eq!(before, Some(size_of::<GraphStoreHandle>()));
    store.add_vertex(Vertex::new(4, "p"), "g").unwrap();
    store
        .add_edge(uqa_core::Edge::new(5, 2, 4, "r"), "g")
        .unwrap();
    store.allocate_vertex_id("old", "g").unwrap();
    store.create_label("g", "new", LabelKind::Vertex).unwrap();
    store.copy_graph("g", "copy").unwrap();
    assert!(store
        .transaction(|graph| {
            graph.add_vertex(Vertex::new(6, "p"), "g")?;
            Err::<(), _>(GraphStoreError::InvalidQuery("rollback".into()))
        })
        .is_err());
    assert!(!store.vertex_ids_in_graph("g").unwrap().contains(&6));
    assert_eq!(
        store
            .image_memory
            .as_ref()
            .map(uqa_core::memory::MemoryReservation::bytes),
        before
    );
    store.drop_graph("copy").unwrap();
    assert!(runtime.used() > 0);
    let source = handle
        .capture_image_with_control(&StorageReadControl::with_limit(1 << 20))
        .unwrap();
    drop((handle, full));
    assert!(input.memory().used() > 0);
    let bytes = image(&source);
    let check = StorageReadControl::with_limit(1 << 20);
    let restored = MemoryGraphStore::read_image(&mut bytes.as_slice(), &check).unwrap();
    assert_eq!(
        restored.vertex_ids_in_graph("g").unwrap(),
        [1, 2, 4].into_iter().collect()
    );
    drop((restored, source));
    assert_eq!(input.memory().used(), 0);
    assert_eq!(runtime.used(), 0);
    assert_eq!(check.memory().used(), 0);
}

#[test]
fn graph_input_retires_failed_native_payloads_and_preceding_membership() {
    for failure in 0..3 {
        let input = StorageReadControl::with_limit(1 << 20);
        let runtime = MemoryBudget::new(1 << 20);
        let catalog = GraphCatalog::new(input.memory())
            .read_image(name(&input), &mut encoded().as_slice(), &runtime, &input)
            .unwrap();
        let mut bytes = encoded();
        let key =
            ProductionControl::new(input.memory(), input.cancellation(), input.cancellation())
                .copy_text("next")
                .unwrap()
                .into_budgeted()
                .unwrap();
        let full = match failure {
            0 => {
                input.cancellation().cancel();
                None
            }
            1 => {
                bytes.push(0);
                None
            }
            _ => Some(input.memory().reserve(input.memory().available()).unwrap()),
        };
        assert!(catalog
            .read_image(key, &mut bytes.as_slice(), &runtime, &input)
            .is_err());
        drop(full);
        assert_eq!(input.memory().used(), 0);
        assert_eq!(runtime.used(), 0);
    }
}

#[derive(Clone, Copy)]
enum EntryPoint {
    Mapped,
    Handle,
    Native,
}

fn failed_operation<G: GraphStore + Default>(
    graph: &mut G,
    input: &StorageReadControl,
    panics: bool,
    replace: bool,
) -> Result<(), GraphStoreError> {
    graph.remove_vertex(1, "g")?;
    if replace {
        *graph = G::default();
    }
    assert_eq!(input.memory().available(), 0);
    assert!(!panics, "mapped rollback");
    Err(GraphStoreError::InvalidQuery("mapped rollback".into()))
}

fn rollback(entry: EntryPoint, panics: bool, replace: bool) {
    let input = StorageReadControl::with_limit(1 << 20);
    let runtime = MemoryBudget::new(1 << 20);
    let catalog = GraphCatalog::new(input.memory())
        .read_image(name(&input), &mut encoded().as_slice(), &runtime, &input)
        .unwrap();
    let mut handle = catalog.get("g").unwrap().clone();
    drop(catalog);
    let full = input.memory().reserve(input.memory().available()).unwrap();
    input.cancellation().cancel();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let handle = Arc::get_mut(&mut handle).unwrap();
        match entry {
            EntryPoint::Mapped => handle.transaction_mapped(
                |graph| failed_operation(graph, &input, panics, replace),
                std::convert::identity,
            ),
            EntryPoint::Handle => {
                handle.transaction(|graph| failed_operation(graph, &input, panics, replace))
            }
            EntryPoint::Native => {
                let GraphStoreHandle::Memory(store) = handle else {
                    unreachable!()
                };
                store.transaction(|graph| failed_operation(graph, &input, panics, replace))
            }
        }
    }));
    if panics {
        assert_eq!(
            result.unwrap_err().downcast_ref::<&str>(),
            Some(&"mapped rollback")
        );
    } else {
        assert!(
            matches!(result.unwrap(), Err(GraphStoreError::InvalidQuery(message)) if message == "mapped rollback")
        );
    }
    assert_eq!(
        handle.vertex_ids_in_graph("g").unwrap(),
        [1, 2].into_iter().collect()
    );
    let GraphStoreHandle::Memory(store) = handle.as_ref() else {
        unreachable!()
    };
    assert_eq!(
        store
            .image_memory
            .as_ref()
            .map(uqa_core::memory::MemoryReservation::bytes),
        Some(size_of::<GraphStoreHandle>())
    );
    assert_eq!(input.memory().available(), 0);
    assert_eq!(runtime.used(), 0);
    drop((handle, full));
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn mapped_graph_error_preserves_image_header_and_original_generation() {
    rollback(EntryPoint::Mapped, false, false);
}

#[test]
fn mapped_graph_panic_preserves_image_header_and_original_generation() {
    rollback(EntryPoint::Mapped, true, false);
}

#[test]
fn mapped_graph_error_restores_replaced_handle_and_original_header() {
    rollback(EntryPoint::Mapped, false, true);
}

#[test]
fn mapped_graph_panic_restores_replaced_handle_and_original_header() {
    rollback(EntryPoint::Mapped, true, true);
}

#[test]
fn graph_trait_transactions_preserve_image_headers_through_error_and_unwind() {
    for panics in [false, true] {
        for replace in [false, true] {
            rollback(EntryPoint::Handle, panics, replace);
        }
    }
}

#[test]
fn native_graph_transactions_preserve_image_headers_through_error_and_unwind() {
    for panics in [false, true] {
        for replace in [false, true] {
            rollback(EntryPoint::Native, panics, replace);
        }
    }
}

#[test]
fn graph_transaction_success_keeps_original_header_across_resource_replacement() {
    for entry in [EntryPoint::Mapped, EntryPoint::Handle, EntryPoint::Native] {
        for persistent in [false, true] {
            if persistent && matches!(entry, EntryPoint::Native) {
                continue;
            }
            let input = StorageReadControl::with_limit(1 << 20);
            let runtime = MemoryBudget::new(1 << 20);
            let catalog = GraphCatalog::new(input.memory())
                .read_image(name(&input), &mut encoded().as_slice(), &runtime, &input)
                .unwrap();
            let mut handle = catalog.get("g").unwrap().clone();
            drop(catalog);
            let full = input.memory().reserve(input.memory().available()).unwrap();
            input.cancellation().cancel();
            let replacement = if persistent {
                let records = Arc::new(uqa_storage::MemoryKeyValueStore::new());
                GraphStoreHandle::from_catalog(
                    Arc::new(uqa_storage::KeyValueCatalog::new(records.clone())),
                    Arc::new(uqa_storage::KeyValueStorageBackend::new(records)),
                )
            } else {
                GraphStoreHandle::default()
            };
            let operation = |graph: &mut GraphStoreHandle| {
                *graph = replacement;
                assert_eq!(input.memory().available(), 0);
                Ok::<_, GraphStoreError>(7)
            };
            let graph = Arc::get_mut(&mut handle).unwrap();
            let result = match entry {
                EntryPoint::Mapped => graph.transaction_mapped(operation, std::convert::identity),
                EntryPoint::Handle => graph.transaction(operation),
                EntryPoint::Native => {
                    let GraphStoreHandle::Memory(store) = graph else {
                        unreachable!()
                    };
                    store.transaction(|store| {
                        *store = MemoryGraphStore::default();
                        assert_eq!(input.memory().available(), 0);
                        Ok(7)
                    })
                }
            };
            assert_eq!(result.unwrap(), 7);
            let reservation = match handle.as_ref() {
                GraphStoreHandle::Memory(store) => &store.image_memory,
                GraphStoreHandle::Persistent(store) => &store.image_memory,
            };
            assert_eq!(
                reservation.as_ref().unwrap().bytes(),
                size_of::<GraphStoreHandle>()
            );
            drop(full);
            assert_eq!(input.memory().used(), size_of::<GraphStoreHandle>());
            assert_eq!(runtime.used(), 0);
            drop(handle);
            assert_eq!(input.memory().used(), 0);
        }
    }
}

#[test]
fn nested_mapped_graph_transactions_restore_the_enclosing_image_generation() {
    let input = StorageReadControl::with_limit(1 << 20);
    let runtime = MemoryBudget::new(1 << 20);
    let catalog = GraphCatalog::new(input.memory())
        .read_image(name(&input), &mut encoded().as_slice(), &runtime, &input)
        .unwrap();
    let mut handle = catalog.get("g").unwrap().clone();
    drop(catalog);
    let used = input.memory().used();
    let result = Arc::get_mut(&mut handle).unwrap().transaction_mapped(
        |graph| {
            graph.transaction(|inner| inner.add_vertex(Vertex::new(4, "p"), "g"))?;
            assert!(graph
                .transaction(|inner| {
                    inner.remove_vertex(2, "g")?;
                    Err::<(), _>(GraphStoreError::InvalidQuery("inner".into()))
                })
                .is_err());
            assert_eq!(
                graph.vertex_ids_in_graph("g")?,
                [1, 2, 4].into_iter().collect()
            );
            Err::<(), _>(GraphStoreError::InvalidQuery("outer".into()))
        },
        std::convert::identity,
    );
    assert!(matches!(result, Err(GraphStoreError::InvalidQuery(message)) if message == "outer"));
    assert_eq!(
        handle.vertex_ids_in_graph("g").unwrap(),
        [1, 2].into_iter().collect()
    );
    assert_eq!(input.memory().used(), used);
    assert_eq!(runtime.used(), 0);
    drop(handle);
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn replaced_persistent_graph_keeps_original_header_through_native_transactions() {
    fn persistent() -> crate::PersistentGraphStore {
        let records = Arc::new(uqa_storage::MemoryKeyValueStore::new());
        crate::PersistentGraphStore::from_catalog(
            Arc::new(uqa_storage::KeyValueCatalog::new(records.clone())),
            Arc::new(uqa_storage::KeyValueStorageBackend::new(records)),
        )
    }
    for panics in [false, true] {
        let input = StorageReadControl::with_limit(1 << 20);
        let mut handle = MemoryGraphStore::new().into_image_handle(&input).unwrap();
        Arc::get_mut(&mut handle)
            .unwrap()
            .transaction_mapped(
                |graph| {
                    *graph = GraphStoreHandle::Persistent(persistent());
                    Ok::<_, GraphStoreError>(())
                },
                std::convert::identity,
            )
            .unwrap();
        let full = input.memory().reserve(input.memory().available()).unwrap();
        let GraphStoreHandle::Persistent(store) = Arc::get_mut(&mut handle).unwrap() else {
            unreachable!()
        };
        store.create_graph("original").unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.transaction(|store| {
                store.create_graph("temporary")?;
                *store = persistent();
                assert_eq!(input.memory().available(), 0);
                assert!(!panics, "persistent rollback");
                Err::<(), _>(GraphStoreError::InvalidQuery("persistent rollback".into()))
            })
        }));
        if panics {
            assert_eq!(
                outcome.unwrap_err().downcast_ref::<&str>(),
                Some(&"persistent rollback")
            );
        } else {
            assert!(
                matches!(outcome.unwrap(), Err(GraphStoreError::InvalidQuery(message)) if message == "persistent rollback")
            );
        }
        assert!(store.has_graph("original").unwrap());
        assert!(!store.has_graph("temporary").unwrap());
        assert_eq!(
            store.image_memory.as_ref().unwrap().bytes(),
            size_of::<GraphStoreHandle>()
        );
        drop(full);
        assert_eq!(input.memory().used(), size_of::<GraphStoreHandle>());
        drop(handle);
        assert_eq!(input.memory().used(), 0);
    }
}
