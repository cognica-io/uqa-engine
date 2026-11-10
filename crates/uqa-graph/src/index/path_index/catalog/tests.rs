//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{GraphStoreHandle, MemoryGraphStore};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError};

#[test]
fn decoded_path_catalog_retains_original_names_and_materializations_through_runtime_replacement() {
    let source = materialized("edge");
    let mut bytes = Vec::new();
    source
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    drop(source);
    let input = StorageReadControl::with_limit(65536);
    let runtime = MemoryBudget::new(65536);
    let name = ProductionControl::new(input.memory(), input.cancellation(), input.cancellation())
        .copy_text("g::path")
        .unwrap()
        .into_budgeted()
        .unwrap();
    let name_pointer = name.as_ptr();
    let catalog = PathIndexCatalog::new(input.memory())
        .read_image(name, &mut bytes.as_slice(), &input)
        .unwrap();
    assert_eq!(catalog.keys().next().unwrap().as_ptr(), name_pointer);
    let mut catalog = catalog.into_mutation_budget(&runtime);
    let retained = catalog.get("g::path").unwrap().clone();
    let control = StorageReadControl::with_limit(65536);
    let captured = catalog.capture(&control).unwrap();
    let image = captured.image("g::path").unwrap().unwrap();
    let full = input.memory().reserve(input.memory().available()).unwrap();
    input.cancellation().cancel();
    catalog.insert("g::path", materialized("changed")).unwrap();
    catalog.insert("g::new", materialized("new")).unwrap();
    drop((catalog, captured, full));
    assert_eq!(runtime.used(), 0);
    assert!(input.memory().used() > 0);
    assert!(retained.has_path(&["edge".into()]));
    assert!(!retained.has_path(&["changed".into()]));
    drop(retained);
    assert!(input.memory().used() > 0);
    assert_eq!(image.get().unwrap().paths[0].sequence, ["edge"]);
    drop(image);
    assert_eq!(input.memory().used(), 0);
    assert_eq!(control.memory().used(), 0);
}

fn materialized(label: &str) -> PathIndex {
    let mut graph = MemoryGraphStore::new();
    graph.create_graph("g").unwrap();
    PathIndex::build(&graph, "g", &[vec![label.into()]]).unwrap()
}

#[test]
fn native_path_catalog_capture_shares_constant_work_roots_and_original_materializations() {
    for count in [1, 64, 4096] {
        let index = materialized(&"label".repeat(1024));
        let original = Arc::downgrade(index.memory_paths().unwrap());
        let memory = MemoryBudget::new(16 << 20);
        let mut catalog = PathIndexCatalog::new(&memory);
        for number in 0..count {
            catalog
                .insert(&format!("g::{number:04}"), index.clone())
                .unwrap();
        }
        let control = StorageReadControl::with_limit(64 << 20);
        let mut captured = None;
        let measured = allocation_counter::measure(|| {
            captured = Some(catalog.capture(&control).unwrap());
        });
        assert_eq!(measured.count_total, 0);
        let captured = captured.unwrap();
        assert_eq!(captured.len(), count);
        assert_eq!(captured.names().count(), count);
        let admitted = control.memory().used();
        let exact = StorageReadControl::with_limit(admitted);
        let independent = catalog.capture(&exact).unwrap();
        let small = StorageReadControl::with_limit(admitted - 1);
        assert!(matches!(
            catalog.capture(&small),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
        let replacement = materialized("replacement");
        let changed = allocation_counter::measure(|| {
            catalog.insert("g::0000", replacement).unwrap();
        });
        assert!(changed.bytes_total < 16 * 1024, "{count}: {changed:?}");
        let child = captured.image("g::0000").unwrap().unwrap();
        assert!(std::ptr::eq(
            child.get().unwrap(),
            &raw const index.memory_paths().unwrap().image
        ));
        drop((catalog, index, captured, independent));
        assert_eq!(exact.memory().used(), 0);
        assert_eq!(memory.used(), 0);
        assert!(original.upgrade().is_some());
        assert_eq!(
            child.get().unwrap().paths[0].sequence,
            ["label".repeat(1024)]
        );
        assert!(control.memory().used() < admitted || count == 1);
        drop(child);
        assert!(original.upgrade().is_none());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_path_publication_and_removal_preserve_root_and_capability_on_admission_failure() {
    let memory = MemoryBudget::new(1 << 20);
    let mut catalog = PathIndexCatalog::new(&memory);
    for name in ["g::a", "g::b", "other::a"] {
        catalog.insert(name, PathIndex::default()).unwrap();
    }
    let original = catalog.clone();
    let footprint = catalog.indexes.retained_bytes().unwrap();
    let used = memory.used();
    let exhausted = memory.reserve(memory.available()).unwrap();
    let view = PathIndex {
        storage: PathStorage::ReadView {
            store: Arc::new(GraphStoreHandle::default()),
            graph: "missing".into(),
            sequences: vec![vec!["edge".into()]],
        },
    };
    assert!(matches!(
        catalog.insert("g::a", view.clone()),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(matches!(
        catalog.remove("g::a"),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(matches!(
        catalog.remove_prefix("g::"),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(catalog.indexes.shares_root(&original.indexes));
    assert_eq!(catalog.indexes.retained_bytes().unwrap(), footprint);
    assert_eq!(catalog.non_memory, 0);
    drop(exhausted);
    assert_eq!(memory.used(), used);
    catalog.insert("g::a", view).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    assert!(matches!(
        catalog.capture(&control),
        Err(StorageBackendError::Other(_))
    ));
    assert_eq!(control.memory().used(), 0);
    catalog.remove_prefix("g::").unwrap();
    assert_eq!(
        catalog.keys().map(String::as_str).collect::<Vec<_>>(),
        ["other::a"]
    );
    assert!(catalog.capture(&control).is_ok());
    assert_eq!(original.len(), 3);
    drop((catalog, original));
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_path_children_keep_original_admission_and_cancellation_after_parent_retirement() {
    let mut catalog = PathIndexCatalog::default();
    catalog
        .insert("g::saved", materialized("original"))
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let captured = catalog.capture(&control).unwrap();
    let before = control.memory().used();
    let exhausted = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            captured.image("g::saved"),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert_eq!(measured.count_total, 0);
    drop(exhausted);
    assert_eq!(control.memory().used(), before);
    let first = captured.image("g::saved").unwrap().unwrap();
    let after = control.memory().used();
    let mut second = None;
    let measured = allocation_counter::measure(|| {
        second = captured.image("g::saved").unwrap();
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(control.memory().used(), after);
    assert!(std::ptr::eq(
        first.get().unwrap(),
        second.as_ref().unwrap().get().unwrap()
    ));
    let independent_control = StorageReadControl::with_limit(1 << 20);
    let independent = catalog.capture(&independent_control).unwrap();
    let independent_child = independent.image("g::saved").unwrap().unwrap();
    drop((catalog, captured, independent));
    control.cancellation().cancel();
    assert!(matches!(
        first.get(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(
        independent_child.get().unwrap().paths[0].sequence,
        ["original"]
    );
    drop((first, second));
    assert_eq!(control.memory().used(), 0);
    assert!(independent_control.memory().used() > 0);
    drop(independent_child);
    assert_eq!(independent_control.memory().used(), 0);
}
