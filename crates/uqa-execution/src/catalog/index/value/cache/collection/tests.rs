//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::index::value::ValueIndexBuilder;
use uqa_core::Value;
use uqa_storage::{
    memory_capture::MemoryCaptureMembership, read_control::StorageReadControl, StorageBackendError,
};

fn index(value: i64) -> ValueIndexCache {
    ValueIndexBuilder::from_values(
        "stored",
        false,
        1 << 20,
        [(1, Value::Int(value))],
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap()
}

fn name(value: &str) -> ValueIndexKey {
    ValueIndexKey::Index(value.into())
}

#[test]
fn native_value_membership_capture_retains_names_without_visiting_child_values() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(4 << 20);
    let source = registry
        .cell(ValueIndexes::memory_with_budget(&registry, &owner))
        .unwrap();
    for n in 0..128 {
        source
            .write()
            .insert(name(&format!("index-{n:03}")), index(n))
            .unwrap();
    }
    source
        .write()
        .insert(ValueIndexKey::Column("derived".into()), index(99))
        .unwrap();
    let control = StorageReadControl::with_limit(4 << 20);
    let selected = source
        .read()
        .capture_sources(
            &registry
                .sources(&owner)
                .with_source(&source.key().unwrap())
                .unwrap(),
        )
        .unwrap();
    let cut = registry.capture_selected(&selected, &control).unwrap();
    let mut captured = None;
    let allocated = allocation_counter::measure(|| captured = Some(source.captured(&cut).unwrap()));
    assert!(allocated.bytes_total < 2048, "{allocated:?}");
    let captured = captured.unwrap();
    assert_eq!(captured.keys().unwrap().len(), 128);
    assert_eq!(captured.next_key(None).unwrap(), Some(&name("index-000")));
    assert_eq!(
        captured.next_key(Some(&name("index-126"))).unwrap(),
        Some(&name("index-127"))
    );
    assert!(captured
        .next_key(Some(&name("index-127")))
        .unwrap()
        .is_none());
    for (_, key) in source.read().native.as_ref().unwrap().names.iter() {
        assert!(key.retained(&cut).unwrap().is_none());
    }
    assert!(captured
        .get(&ValueIndexKey::Column("derived".into()), &cut)
        .unwrap()
        .is_none());
    let value = captured.get(&name("index-127"), &cut).unwrap().unwrap();
    assert_eq!(value.value(1).unwrap().as_deref(), Some(&Value::Int(127)));
    drop((value, captured, cut, source, selected, registry));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(owner.used(), 0);
}

#[test]
fn native_value_membership_publication_keeps_exact_children_after_replacement_and_retirement() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(4 << 20);
    let publisher = MemoryCaptureMembership::new(&registry, &owner).unwrap();
    let source = registry
        .cell(ValueIndexes::memory_with_budget(&registry, &owner))
        .unwrap();
    source.write().insert(name("a"), index(10)).unwrap();
    let parent = registry
        .sources(&owner)
        .with_source(&source.key().unwrap())
        .unwrap();
    {
        let mut values = source.write();
        let publication = values.source_publication().unwrap();
        let member = publisher
            .publish(None, &parent.with_sources(publication.sources()).unwrap())
            .unwrap();
        publication.bind(member);
    }
    let control = StorageReadControl::with_limit(4 << 20);
    let old = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    source.write().insert(name("a"), index(20)).unwrap();
    source.write().insert(name("b"), index(30)).unwrap();
    assert_eq!(publisher.snapshot().len(), 3);
    let current = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    source.write().retain(|key| key == &name("b")).unwrap();
    assert_eq!(publisher.snapshot().len(), 2);
    let final_cut = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    let parent_key = source.key().unwrap();
    drop(source);
    let old_names = parent_key.captured(&old).unwrap();
    let current_names = parent_key.captured(&current).unwrap();
    let final_names = parent_key.captured(&final_cut).unwrap();
    assert!(old_names.keys().unwrap().eq([&name("a")]));
    assert!(final_names.keys().unwrap().eq([&name("b")]));
    assert!(old_names.get(&name("a"), &current).is_err());
    let first = old_names.get(&name("a"), &old).unwrap().unwrap();
    let second = current_names.get(&name("a"), &current).unwrap().unwrap();
    let third = final_names.get(&name("b"), &final_cut).unwrap().unwrap();
    drop((
        old,
        current,
        final_cut,
        old_names,
        current_names,
        final_names,
        parent_key,
        parent,
        publisher,
        registry,
    ));
    assert_eq!(first.value(1).unwrap().as_deref(), Some(&Value::Int(10)));
    assert_eq!(second.value(1).unwrap().as_deref(), Some(&Value::Int(20)));
    assert_eq!(third.value(1).unwrap().as_deref(), Some(&Value::Int(30)));
    drop((first, second, third));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(owner.used(), 0);
}

#[test]
fn native_value_membership_denied_successors_preserve_live_and_selected_collections() {
    let registry = MemoryCaptureRegistry::new();
    let owner = MemoryBudget::new(1 << 20);
    let publication_memory = MemoryBudget::new(1 << 20);
    let publisher = MemoryCaptureMembership::new(&registry, &publication_memory).unwrap();
    let source = registry
        .cell(ValueIndexes::memory_with_budget(&registry, &owner))
        .unwrap();
    source.write().insert(name("a"), index(7)).unwrap();
    let parent = registry
        .sources(&owner)
        .with_source(&source.key().unwrap())
        .unwrap();
    {
        let mut values = source.write();
        let publication = values.source_publication().unwrap();
        let member = publisher
            .publish(None, &parent.with_sources(publication.sources()).unwrap())
            .unwrap();
        publication.bind(member);
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    let held = owner.reserve(owner.available()).unwrap();
    assert!(matches!(
        source.write().insert(name("b"), index(8)),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(source.read().len(), 1);
    assert_eq!(publisher.snapshot().len(), 2);
    drop(held);
    let held = publication_memory
        .reserve(publication_memory.available())
        .unwrap();
    assert!(matches!(
        source.write().clear(),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(source.read().len(), 1);
    assert_eq!(publisher.snapshot().len(), 2);
    drop(held);
    source
        .write()
        .get_mut(&name("a"))
        .unwrap()
        .replace(
            1,
            Some(&Value::Int(9)),
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
    let saved = source
        .captured(&cut)
        .unwrap()
        .get(&name("a"), &cut)
        .unwrap()
        .unwrap();
    assert_eq!(saved.value(1).unwrap().as_deref(), Some(&Value::Int(7)));
    assert_eq!(
        source
            .read()
            .get(&name("a"))
            .unwrap()
            .stored_value(1, &control)
            .unwrap()
            .as_deref(),
        Some(&Value::Int(9))
    );
    drop((source, saved, cut, parent, publisher, registry));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(owner.used(), 0);
    assert_eq!(publication_memory.used(), 0);
}
