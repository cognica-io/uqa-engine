//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::RelationStats;
use uqa_core::Value;

#[test]
fn empty_statistics_and_handle_clones_do_not_allocate() {
    let allocation = allocation_counter::measure(|| {
        let empty = ColumnStatistics::default();
        let owned = ColumnStatistics::from(BTreeMap::new());
        let collected = std::iter::empty().collect::<ColumnStatistics>();
        for columns in [empty.clone(), owned.clone(), collected.clone()] {
            assert!(columns.is_empty());
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn shared_statistics_keep_payloads_until_the_last_reader_releases_them() {
    let catalog = Arc::new(BTreeMap::from([(
        "payload".to_owned(),
        ColumnStats {
            row_count: 2,
            distinct_count: 2,
            mcv_values: vec![Value::Str("x".repeat(4 * 1024 * 1024))],
            mcv_frequencies: vec![0.5],
            ..Default::default()
        },
    )]));
    let weak = Arc::downgrade(&catalog);
    let column_pointer = &raw const catalog["payload"];
    let payload_pointer = catalog["payload"].mcv_values.as_ptr();
    let mut readers: [Option<ColumnStatistics>; 64] = std::array::from_fn(|_| None);
    let allocation = allocation_counter::measure(|| {
        let columns = ColumnStatistics::from(catalog);
        for reader in &mut readers {
            *reader = Some(columns.clone());
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    for reader in readers.iter().flatten() {
        assert_eq!(&raw const reader["payload"], column_pointer);
        assert_eq!(reader["payload"].mcv_values.as_ptr(), payload_pointer);
        assert_eq!(reader["payload"].equality_selectivity(), 0.5);
    }
    let last_reader = readers[0].take().unwrap();
    drop(readers);
    assert!(weak.upgrade().is_some());
    drop(last_reader);
    assert!(weak.upgrade().is_none());
}

#[test]
fn relation_builder_detaches_before_changing_shared_statistics() {
    let original = RelationStats::new(4).with_column(
        "id",
        ColumnStats {
            row_count: 4,
            distinct_count: 2,
            mcv_values: vec![Value::Int(1)],
            mcv_frequencies: vec![0.75],
            ..Default::default()
        },
    );
    let reader = original.clone();
    let changed = original.with_column(
        "id",
        ColumnStats {
            row_count: 4,
            distinct_count: 4,
            mcv_values: vec![Value::Int(2)],
            mcv_frequencies: vec![0.25],
            ..Default::default()
        },
    );
    assert!(!std::ptr::eq(
        &raw const reader.columns["id"],
        &raw const changed.columns["id"]
    ));
    assert_eq!(reader.column("id").unwrap().mcv_values, [Value::Int(1)]);
    assert_eq!(changed.column("id").unwrap().mcv_values, [Value::Int(2)]);
    assert_eq!(
        reader
            .column("id")
            .unwrap()
            .equality_selectivity_for(&Value::Int(1)),
        0.75
    );
    assert_eq!(
        changed
            .column("id")
            .unwrap()
            .equality_selectivity_for(&Value::Int(1)),
        0.25
    );
}
