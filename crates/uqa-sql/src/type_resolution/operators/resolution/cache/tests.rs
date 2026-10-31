//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn builtin_selection_cache_owns_no_request_allocation_on_misses_hits_or_eviction() {
    let cache = BuiltinSelections::<4>::default();
    let budget = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for name in ["+", "-", "*", "/", "%", "+"] {
        for _ in 0..2 {
            let mut selected = None;
            let allocations = allocation_counter::measure(|| {
                selected = Some(
                    super::super::select_with_cache(
                        name,
                        Some(&ColumnType::Integer),
                        Some(&ColumnType::Integer),
                        &control,
                        &cache,
                    )
                    .unwrap(),
                );
            });
            assert_eq!(allocations.bytes_total, 0);
            assert_eq!(budget.used(), 0);
            assert_eq!(
                *selected.unwrap(),
                [
                    ColumnType::Integer,
                    ColumnType::Integer,
                    ColumnType::Integer
                ]
            );
        }
    }
    let cache = cache.read().unwrap();
    assert_eq!(cache.len, 2);
    assert!(cache
        .get(&selection_key("+", Some(&ColumnType::Integer), Some(&ColumnType::Integer)).unwrap())
        .is_some());
    assert!(cache
        .get(&selection_key("-", Some(&ColumnType::Integer), Some(&ColumnType::Integer)).unwrap())
        .is_none());
}

#[test]
fn builtin_selection_cache_keys_keep_exact_catalog_identity_through_collisions() {
    let mut cache = SelectionTable::<3>::new();
    let mut first = None;
    let mut collision = None;
    for &(name, left, right, output, _, _) in SIGNATURES {
        let (Some(left), Some(right), Some(output)) = (
            scalar_catalog_type(left),
            scalar_catalog_type(right),
            scalar_catalog_type(output),
        ) else {
            continue;
        };
        let Some(key) = selection_key(name, Some(left), Some(right)) else {
            continue;
        };
        assert!(name.len() <= OPERATOR_NAME_BYTES);
        let entry = (key, [left, right, output]);
        if let Some((previous, _)) = first {
            if previous != key
                && SelectionTable::<3>::index(&previous) == SelectionTable::<3>::index(&key)
            {
                collision = Some(entry);
                break;
            }
        } else {
            first = Some(entry);
        }
    }
    let first = first.unwrap();
    let collision = collision.expect("catalog provides distinct keys in the same slot");
    let missing = selection_key("?", None, None).unwrap();
    cache.insert(first.0, first.1);
    cache.insert(collision.0, collision.1);
    assert_eq!(cache.get(&first.0), Some(first.1));
    assert_eq!(cache.get(&collision.0), Some(collision.1));
    assert_eq!(cache.get(&missing), None);
    cache.insert(first.0, first.1);
    assert_eq!(cache.len, 2);
    cache.insert(missing, [&ColumnType::Boolean; 3]);
    assert_eq!(cache.get(&missing), Some([&ColumnType::Boolean; 3]));
    let next = selection_key("?", Some(&ColumnType::Text), None).unwrap();
    cache.insert(next, [&ColumnType::Text; 3]);
    assert_eq!(cache.len, 1);
    assert_eq!(cache.get(&first.0), None);
    assert_eq!(cache.get(&collision.0), None);
    assert_eq!(cache.get(&next), Some([&ColumnType::Text; 3]));

    let cache = BuiltinSelections::<32>::default();
    let control = ProductionControl::uncontrolled();
    for &(name, left, right, _, _, _) in SIGNATURES {
        let (Some(left), Some(right)) = (scalar_catalog_type(left), scalar_catalog_type(right))
        else {
            continue;
        };
        let Some(key) = selection_key(name, Some(left), Some(right)) else {
            continue;
        };
        let fresh =
            super::super::select_binary_operator_types(name, Some(left), Some(right), &control)
                .unwrap();
        for _ in 0..2 {
            let selected =
                super::super::select_with_cache(name, Some(left), Some(right), &control, &cache)
                    .unwrap();
            assert_eq!(*selected, *fresh, "{name}: {left:?}, {right:?}");
        }
        assert_eq!(
            cache
                .read()
                .unwrap()
                .get(&key)
                .map(|types| types.map(Clone::clone)),
            Some((*fresh).clone())
        );
    }
}
