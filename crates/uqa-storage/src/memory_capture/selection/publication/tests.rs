//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    memory_capture::{MemoryCaptureCell, MemoryCaptureSource},
    read_control::StorageReadControl,
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Value {
    value: u64,
    freezes: Arc<AtomicUsize>,
}

impl MemoryCaptureSource for Value {
    type Frozen = u64;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<u64> {
        control.check()?;
        self.freezes.fetch_add(1, Ordering::Relaxed);
        Ok(self.value)
    }
}

fn cell(registry: &MemoryCaptureRegistry, value: u64) -> MemoryCaptureCell<Value> {
    registry
        .cell(Value {
            value,
            freezes: Arc::default(),
        })
        .unwrap()
}

#[test]
fn publication_mutation_budget_keeps_existing_addresses_and_original_node_owners() {
    let registry = MemoryCaptureRegistry::new();
    let original = MemoryBudget::new(1 << 20);
    let runtime = MemoryBudget::new(1 << 20);
    let first = cell(&registry, 1);
    let second = cell(&registry, 2);
    let one = registry
        .sources(&original)
        .with_source(&first.key().unwrap())
        .unwrap();
    let two = registry
        .sources(&runtime)
        .with_source(&second.key().unwrap())
        .unwrap();
    let publisher = MemoryCaptureMembership::new(&registry, &original).unwrap();
    let address = publisher.publish(None, &one).unwrap();
    let retained = publisher.snapshot();
    let publisher = publisher.into_mutation_budget(&runtime);
    let full = original.reserve(original.available()).unwrap();
    assert!(address.replace_sources(&one, &two).unwrap());
    let control = StorageReadControl::with_limit(1 << 20);
    let current = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    assert!(first.captured(&current).is_err());
    assert_eq!(**second.captured(&current).unwrap(), 2);
    assert!(publisher.remove(&address).unwrap());
    let old = registry.capture_selected(&retained, &control).unwrap();
    assert_eq!(**first.captured(&old).unwrap(), 1);
    drop((publisher, retained, old, current, one, two, full));
    assert_eq!(original.used(), 0);
    assert_eq!(runtime.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn published_membership_preserves_selected_incarnations_after_retirement_and_replacement() {
    let registry = MemoryCaptureRegistry::new();
    let memory = MemoryBudget::new(1 << 20);
    let publisher = MemoryCaptureMembership::new(&registry, &memory).unwrap();
    let first = cell(&registry, 1);
    let second = cell(&registry, 2);
    let third = cell(&registry, 3);
    let set = |value: &MemoryCaptureCell<Value>| {
        registry
            .sources(&memory)
            .with_source(&value.key().unwrap())
            .unwrap()
    };
    let one = set(&first);
    let two = set(&second);
    let three = set(&third);
    let first_member = publisher.publish(None, &one).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let old = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    let replacement = publisher.publish(Some(&first_member), &two).unwrap();
    assert!(!first_member.replace_sources(&one, &three).unwrap());
    let current = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    first.write().value = 11;
    second.write().value = 22;
    third.write().value = 33;
    assert_eq!(**first.captured(&old).unwrap(), 1);
    assert_eq!(**second.captured(&current).unwrap(), 2);
    assert!(first.captured(&current).is_err());
    assert!(third.captured(&old).is_err());
    assert!(third.captured(&current).is_err());
    assert_eq!(third.read().freezes.load(Ordering::Relaxed), 0);
    assert!(publisher.remove(&replacement).unwrap());
    assert!(!publisher.remove(&replacement).unwrap());
    assert!(!replacement.replace_sources(&two, &three).unwrap());
    assert!(publisher.snapshot().is_empty());
    drop((publisher, first, second, third, one, two, three));
    assert!(!first_member
        .replace_sources(&registry.sources(&memory), &registry.sources(&memory))
        .unwrap());
    drop((old, current));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn published_child_updates_are_atomic_under_admission_domain_and_overlap_failures() {
    let registry = MemoryCaptureRegistry::new();
    let memory = MemoryBudget::new(1 << 20);
    let construction = MemoryBudget::new(1 << 20);
    let publisher = MemoryCaptureMembership::new(&registry, &memory).unwrap();
    let values: Vec<_> = (0..4).map(|n| cell(&registry, n)).collect();
    let sources: Vec<_> = values
        .iter()
        .map(|value| {
            registry
                .sources(&construction)
                .with_source(&value.key().unwrap())
                .unwrap()
        })
        .collect();
    let first = publisher.publish(None, &sources[0]).unwrap();
    let second = publisher.publish(None, &sources[1]).unwrap();
    let held = memory.reserve(memory.available()).unwrap();
    assert!(matches!(
        first.replace_sources(&sources[0], &sources[2]),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(matches!(
        publisher.publish(Some(&first), &sources[2]),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(matches!(
        publisher.remove(&second),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(publisher.snapshot().len(), 2);
    assert!(first.replace_sources(&sources[0], &sources[0]).unwrap());
    drop(held);
    assert!(publisher.publish(None, &sources[0]).is_err());
    assert!(first.replace_sources(&sources[0], &sources[1]).is_err());
    assert!(first.replace_sources(&sources[3], &sources[2]).is_err());
    let foreign = MemoryCaptureRegistry::new();
    let foreign_source = cell(&foreign, 9);
    let foreign_sources = foreign
        .sources(&construction)
        .with_source(&foreign_source.key().unwrap())
        .unwrap();
    assert!(publisher.publish(None, &foreign_sources).is_err());
    assert!(first
        .replace_sources(&sources[0], &foreign_sources)
        .is_err());
    let other = MemoryCaptureMembership::new(&registry, &memory).unwrap();
    assert!(other.remove(&first).is_err());
    assert!(other.publish(Some(&first), &sources[0]).is_err());
    assert!(first.replace_sources(&sources[0], &sources[2]).unwrap());
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = registry
        .capture_selected(&publisher.snapshot(), &control)
        .unwrap();
    assert!(values[0].captured(&cut).is_err());
    assert_eq!(**values[1].captured(&cut).unwrap(), 1);
    assert_eq!(**values[2].captured(&cut).unwrap(), 2);
    assert!(values[3].captured(&cut).is_err());
    assert!(publisher.remove(&first).unwrap());
    assert!(publisher.remove(&second).unwrap());
    drop((
        cut,
        publisher,
        other,
        sources,
        foreign_sources,
        foreign_source,
        values,
    ));
    assert_eq!(memory.used(), 0);
    assert_eq!(construction.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn published_selection_snapshot_does_not_visit_members_or_sources() {
    let registry = MemoryCaptureRegistry::new();
    let memory = MemoryBudget::new(32 << 20);
    let publisher = MemoryCaptureMembership::new(&registry, &memory).unwrap();
    let values: Vec<_> = (0..4096).map(|n| cell(&registry, n)).collect();
    for value in &values {
        let sources = registry
            .sources(&memory)
            .with_source(&value.key().unwrap())
            .unwrap();
        publisher.publish(None, &sources).unwrap();
    }
    let mut selected = None;
    let measured = allocation_counter::measure(|| selected = Some(publisher.snapshot()));
    assert_eq!(measured.bytes_total, 0);
    let selected = selected.unwrap();
    assert_eq!(selected.len(), 4096);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut cut = None;
    let measured = allocation_counter::measure(|| {
        cut = Some(registry.capture_selected(&selected, &control).unwrap());
    });
    assert!(measured.bytes_total < 4096);
    assert!(values
        .iter()
        .all(|value| value.read().freezes.load(Ordering::Relaxed) == 0));
    drop((cut, selected, publisher, values));
    assert_eq!(memory.used(), 0);
    assert_eq!(control.memory().used(), 0);
}
