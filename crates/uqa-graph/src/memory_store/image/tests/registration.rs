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
