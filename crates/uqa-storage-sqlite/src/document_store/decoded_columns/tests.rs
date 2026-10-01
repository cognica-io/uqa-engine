//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;
use uqa_core::Value;

use super::{DecodedColumns, NativeRecordOwner};

fn owner(identity: u8) -> NativeRecordOwner {
    NativeRecordOwner::Object {
        identity: [identity; 16],
        generation: [1; 16],
    }
}

fn insert(cache: &DecodedColumns, table: &str, generation: i64, field: &str, values: &[i64]) {
    let mut ids = BudgetedVec::new(cache.budget());
    let mut column = BudgetedVec::new(cache.budget());
    for (id, value) in values.iter().enumerate() {
        ids.push(i64::try_from(id).unwrap()).unwrap();
        column.push(Value::Int(*value)).unwrap();
    }
    cache.insert(
        table,
        owner(1),
        generation,
        ids,
        vec![(field.to_owned(), column)],
        cache.budget().empty_reservation(),
    );
}

#[test]
fn entries_serve_complete_projections_of_their_own_generation() {
    let cache = DecodedColumns::new(1 << 20);
    insert(&cache, "items", 4, "value", &[10, 20]);
    assert!(cache
        .get("items", owner(1), 4, &["value", "label"])
        .is_none());
    assert!(cache.get("items", owner(1), 5, &["value"]).is_none());
    assert!(cache.get("items", owner(2), 4, &["value"]).is_none());
    insert(&cache, "items", 4, "label", &[1, 2]);
    let (ids, columns) = cache
        .get("items", owner(1), 4, &["label", "value", "label"])
        .unwrap();
    assert_eq!(*ids, [0, 1]);
    assert_eq!(*columns[0], [Value::Int(1), Value::Int(2)]);
    assert_eq!(*columns[1], [Value::Int(10), Value::Int(20)]);
    assert!(std::sync::Arc::ptr_eq(&columns[0], &columns[2]));
    // A newer generation replaces the table's entry.
    insert(&cache, "items", 5, "value", &[11, 21]);
    assert!(cache.get("items", owner(1), 4, &["value"]).is_none());
    assert!(cache.get("items", owner(1), 5, &["value"]).is_some());
}

#[test]
fn eviction_releases_the_least_recently_used_tables_allowance() {
    let cache = DecodedColumns::new(1 << 20);
    insert(&cache, "first", 1, "value", &[1, 2, 3]);
    insert(&cache, "second", 1, "value", &[4, 5, 6]);
    assert!(cache.get("first", owner(1), 1, &["value"]).is_some());
    let used = cache.budget().used();
    assert!(cache.evict_other("third"));
    assert!(cache.get("second", owner(1), 1, &["value"]).is_none());
    assert!(cache.get("first", owner(1), 1, &["value"]).is_some());
    assert!(cache.budget().used() < used);
    assert!(!cache.evict_other("first"));
    cache.retire_stale("first", owner(1), 2);
    assert_eq!(cache.budget().used(), 0);
}
