//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::index::value::ValueIndexBuilder;
use uqa_core::{memory::ProductionControl, Value};

fn name(value: &str, input: &StorageReadControl) -> Budgeted<String> {
    ProductionControl::new(input.memory(), input.cancellation(), input.cancellation())
        .copy_text(value)
        .unwrap()
        .into_budgeted()
        .unwrap()
}

fn index(value: i64) -> ValueIndexCache {
    ValueIndexBuilder::from_values(
        "stored",
        false,
        1024,
        [(1, Value::Int(value))],
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap()
}

#[test]
fn admitted_value_registrations_preserve_original_names_and_exact_captured_children() {
    let input = StorageReadControl::with_limit(1 << 20);
    let output = StorageReadControl::with_limit(1 << 20);
    let registry = MemoryCaptureRegistry::new();
    let original = name("original", &input);
    let pointer = original.as_ptr();
    let indexes = MemoryValueIndexesBuilder::new(&registry, &input)
        .add_index(original, index(10))
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(indexes.keys().next().unwrap().name().as_ptr(), pointer);
    let source = registry.cell(indexes).unwrap();
    let cut = registry.capture(&output).unwrap();
    let names = source.captured(&cut).unwrap();
    let occupied = input.memory().reserve(input.memory().available()).unwrap();
    input.cancellation().cancel();
    let key = ValueIndexKey::Index("original".into());
    source.write().insert(key.clone(), index(20)).unwrap();
    source
        .write()
        .insert(ValueIndexKey::Index("new".into()), index(30))
        .unwrap();
    let child = names.get(&key, &cut).unwrap().unwrap();
    drop((source, names, cut, registry));
    assert_eq!(child.value(1).unwrap().as_deref(), Some(&Value::Int(10)));
    drop(child);
    assert_eq!(input.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn admitted_value_registration_rejection_retires_complete_partial_ownership() {
    for failure in 0..5 {
        let input = StorageReadControl::with_limit(1 << 20);
        let other = StorageReadControl::with_limit(1 << 20);
        let registry = MemoryCaptureRegistry::new();
        let builder = MemoryValueIndexesBuilder::new(&registry, &input)
            .add_index(name("first", &input), index(1))
            .unwrap();
        let name = match failure {
            0 => name("first", &input),
            1 => name("second", &other),
            2 => Budgeted::new("second".into(), input.memory().empty_reservation()),
            _ => name("second", &input),
        };
        let occupied =
            (failure == 3).then(|| input.memory().reserve(input.memory().available()).unwrap());
        if failure == 4 {
            input.cancellation().cancel();
        }
        assert!(builder.add_index(name, index(2)).is_err());
        assert_eq!(
            input.memory().used(),
            occupied
                .as_ref()
                .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        assert_eq!(other.memory().used(), 0);
        drop(occupied);
        assert_eq!(input.memory().used(), 0);
    }
}
