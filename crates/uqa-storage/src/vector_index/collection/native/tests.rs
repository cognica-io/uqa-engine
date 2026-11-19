//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    memory_capture::{MemoryCaptureCell, MemoryCaptureMembership},
    read_control::StorageReadControl,
    vector_index::{MemoryVectorIndex, VectorIndexes},
};
use std::collections::BTreeMap;

fn source(
    count: usize,
    registry: &MemoryCaptureRegistry,
    memory: &MemoryBudget,
) -> MemoryCaptureCell<VectorIndexes> {
    let source = registry
        .cell(VectorIndexes::memory_with_budget(registry, memory))
        .unwrap();
    for field in 0..count {
        source
            .write()
            .live_mut()
            .unwrap()
            .insert(
                format!("vector_{field:04}"),
                Box::new(MemoryVectorIndex::new(2)),
            )
            .unwrap();
    }
    source
}

fn publish(
    source: &MemoryCaptureCell<VectorIndexes>,
    registry: &MemoryCaptureRegistry,
    publisher: &MemoryCaptureMembership,
    memory: &MemoryBudget,
) -> MemoryCaptureMember {
    let mut value = source.write();
    let binding = value.source_publication().unwrap();
    let selection = registry
        .sources(memory)
        .with_source(&source.key().unwrap())
        .unwrap()
        .with_sources(binding.sources())
        .unwrap();
    let member = publisher.publish(None, &selection).unwrap();
    binding.bind(member.clone());
    member
}

#[test]
fn native_vector_children_follow_membership_without_reviving_retired_owners() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(4 << 20);
    let publication = MemoryBudget::new(4 << 20);
    let publisher = MemoryCaptureMembership::new(&registry, &publication).unwrap();
    let source = source(1, &registry, &owner);
    let member = publish(&source, &registry, &publisher, &publication);
    let control = StorageReadControl::with_limit(4 << 20);
    let original = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    source
        .write()
        .live_mut()
        .unwrap()
        .insert("new".into(), Box::new(MemoryVectorIndex::new(3)))
        .unwrap();
    assert_eq!(publisher.snapshot().len(), 3);
    let changed = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    let captured = source.captured(&original).unwrap();
    assert_eq!(
        captured.fields().unwrap().collect::<Vec<_>>(),
        ["vector_0000"]
    );
    assert!(captured.image("new", &original).is_err());
    let later = source.captured(&changed).unwrap();
    assert_eq!(dimensions(&later.image("new", &changed).unwrap()), 3);
    source
        .write()
        .live_mut()
        .unwrap()
        .remove("vector_0000")
        .unwrap();
    assert_eq!(publisher.snapshot().len(), 2);
    assert!(publisher.remove(&member).unwrap());
    let excluded = StorageReadControl::with_limit(4096);
    let empty = registry
        .capture_selected(&publisher.snapshot(), &excluded)
        .unwrap();
    let admission = excluded.memory().used();
    source
        .write()
        .live_mut()
        .unwrap()
        .insert("detached".into(), Box::new(MemoryVectorIndex::new(4)))
        .unwrap();
    assert!(publisher.snapshot().is_empty());
    assert_eq!(excluded.memory().used(), admission);
    empty.check().unwrap();
    assert!(source.captured(&empty).is_err());
    let rebound = publish(&source, &registry, &publisher, &publication);
    assert_eq!(publisher.snapshot().len(), 3);
    assert!(!member
        .replace_source(None, Some(&source.key().unwrap()))
        .unwrap());
    let current = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    let current_value = source.captured(&current).unwrap();
    assert_eq!(
        current_value.fields().unwrap().collect::<Vec<_>>(),
        ["detached", "new"]
    );
    assert_eq!(
        dimensions(&current_value.image("detached", &current).unwrap()),
        4
    );
    assert_eq!(
        dimensions(&captured.image("vector_0000", &original).unwrap()),
        2
    );
    assert!(publisher.remove(&rebound).unwrap());
    drop((
        publisher,
        source,
        registry,
        original,
        changed,
        current,
        empty,
        captured,
        later,
        current_value,
    ));
    assert_eq!(owner.used(), 0);
    assert_eq!(publication.used(), 0);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(excluded.memory().used(), 0);
}

#[test]
fn native_vector_child_publication_failure_keeps_live_and_selected_membership_together() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(4 << 20);
    let publication = MemoryBudget::new(4 << 20);
    let publisher = MemoryCaptureMembership::new(&registry, &publication).unwrap();
    let source = source(4, &registry, &owner);
    let member = publish(&source, &registry, &publisher, &publication);
    let original = publisher.snapshot();
    let before = owner.used();
    let held = publication.reserve(publication.available()).unwrap();
    assert!(matches!(
        source
            .write()
            .live_mut()
            .unwrap()
            .insert("vector_0001".into(), Box::new(MemoryVectorIndex::new(3)),),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(matches!(
        source.write().live_mut().unwrap().remove("vector_0001"),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(source.read().len(), 4);
    assert_eq!(source.read()["vector_0001"].dimensions(), 2);
    assert_eq!(owner.used(), before);
    assert_eq!(publisher.snapshot().len(), 5);
    drop(held);
    let control = StorageReadControl::with_limit(4 << 20);
    let old = registry.capture_selected(&original, &control).unwrap();
    source
        .write()
        .live_mut()
        .unwrap()
        .remove("vector_0001")
        .unwrap();
    let captured = source.captured(&old).unwrap();
    assert_eq!(dimensions(&captured.image("vector_0001", &old).unwrap()), 2);
    assert_eq!(publisher.snapshot().len(), 4);
    assert!(publisher.remove(&member).unwrap());
    drop((publisher, source, registry, original, old, captured));
    assert_eq!(owner.used(), 0);
    assert_eq!(publication.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

fn dimensions(image: &VectorImageSnapshot) -> u32 {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut bytes = Vec::new();
    image.write_image(&mut bytes, &control).unwrap();
    crate::vector_index::image::read_vector_image(&mut bytes.as_slice(), &control)
        .unwrap()
        .dimensions()
}

#[test]
fn native_vector_membership_capture_has_constant_work_and_exact_admission() {
    let mut original = None;
    for count in [1, 64, 4096] {
        let registry = MemoryCaptureRegistry::new();
        let owner = MemoryBudget::new(64 << 20);
        let source = source(count, &registry, &owner);
        let selection = registry
            .sources(&owner)
            .with_source(&source.key().unwrap())
            .unwrap();
        let control = StorageReadControl::with_limit(64 << 20);
        let cut = registry.capture_selected(&selection, &control).unwrap();
        let mut captured = None;
        let measured = allocation_counter::measure(|| {
            captured = Some(source.captured(&cut).unwrap());
        });
        let allocations = (measured.count_total, measured.bytes_total);
        assert_eq!(*original.get_or_insert(allocations), allocations);
        assert!(measured.bytes_total < 4096);
        let captured = captured.unwrap();
        assert_eq!(captured.fields().unwrap().len(), count);
        let exact_bytes = control.memory().used();
        let exact = StorageReadControl::with_limit(exact_bytes);
        let exact_cut = registry.capture_selected(&selection, &exact).unwrap();
        let exact_value = source.captured(&exact_cut).unwrap();
        assert_eq!(exact.memory().used(), exact_bytes);
        let insufficient = StorageReadControl::with_limit(exact_bytes - 1);
        let insufficient_cut = registry
            .capture_selected(&selection, &insufficient)
            .unwrap();
        assert!(matches!(
            source.captured(&insufficient_cut),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(source.read().len(), count);
        drop((
            source,
            registry,
            selection,
            cut,
            exact_cut,
            insufficient_cut,
        ));
        assert!(owner.used() > 0);
        assert_eq!(captured.fields().unwrap().len(), count);
        assert_eq!(exact_value.fields().unwrap().len(), count);
        control.cancellation().cancel();
        assert!(matches!(
            captured.fields(),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(exact_value.fields().unwrap().len(), count);
        drop((captured, exact_value));
        assert_eq!(owner.used(), 0);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(exact.memory().used(), 0);
        assert_eq!(insufficient.memory().used(), 0);
    }
}

#[test]
fn native_vector_publication_failure_preserves_membership_and_child_incarnations() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(1 << 20);
    let source = source(4, &registry, &owner);
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = registry.capture(&control).unwrap();
    let original = source.captured(&cut).unwrap();
    let held = owner.reserve(owner.available()).unwrap();
    for field in ["vector_0001", "new"] {
        assert!(matches!(
            source
                .write()
                .live_mut()
                .unwrap()
                .insert(field.into(), Box::new(MemoryVectorIndex::new(3)),),
            Err(StorageBackendError::Memory(_))
        ));
    }
    assert!(matches!(
        source.write().live_mut().unwrap().remove("vector_0001"),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(source
        .write()
        .live_mut()
        .unwrap()
        .remove("absent")
        .unwrap()
        .is_none());
    let replacement: BTreeMap<FieldName, Box<dyn VectorIndex>> = BTreeMap::from([(
        "replacement".into(),
        Box::new(MemoryVectorIndex::new(5)) as Box<dyn VectorIndex>,
    )]);
    assert!(matches!(
        source.write().live_mut().unwrap().replace(replacement),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(source.read().len(), 4);
    assert_eq!(source.read()["vector_0001"].dimensions(), 2);
    assert_eq!(original.fields().unwrap().len(), 4);
    assert_eq!(dimensions(&original.image("vector_0001", &cut).unwrap()), 2);
    drop(held);
    source
        .write()
        .live_mut()
        .unwrap()
        .insert("vector_0001".into(), Box::new(MemoryVectorIndex::new(3)))
        .unwrap();
    source
        .write()
        .live_mut()
        .unwrap()
        .remove("vector_0002")
        .unwrap();
    assert_eq!(source.read().len(), 3);
    assert_eq!(source.read()["vector_0001"].dimensions(), 3);
    assert_eq!(dimensions(&original.image("vector_0001", &cut).unwrap()), 2);
    assert_eq!(dimensions(&original.image("vector_0002", &cut).unwrap()), 2);
    drop((source, registry, cut, original));
    assert_eq!(owner.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_vector_publication_changes_only_the_selected_field_path() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(64 << 20);
    let source = source(4096, &registry, &owner);
    let control = StorageReadControl::with_limit(64 << 20);
    let cut = registry.capture(&control).unwrap();
    let original = source.captured(&cut).unwrap();
    let measured = allocation_counter::measure(|| {
        source
            .write()
            .live_mut()
            .unwrap()
            .insert("vector_2048".into(), Box::new(MemoryVectorIndex::new(3)))
            .unwrap();
    });
    assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
    assert_eq!(original.fields().unwrap().len(), 4096);
    assert_eq!(dimensions(&original.image("vector_2048", &cut).unwrap()), 2);
    assert_eq!(source.read()["vector_2048"].dimensions(), 3);
    // The capture has already established its selected replacement obligation; release it before retiring the unrelated children.
    drop((original, cut, source, registry));
    assert_eq!(owner.used(), 0);
    assert_eq!(control.memory().used(), 0);
}
