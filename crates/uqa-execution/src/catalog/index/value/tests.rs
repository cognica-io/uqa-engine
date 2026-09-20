//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Value-index lookup and mutation semantics follow the owning execution implementation.

use super::*;

fn ids(list: &PostingList) -> Vec<DocId> {
    list.entries().iter().map(|e| e.doc_id).collect()
}

#[test]
fn build_scan_equals_and_ranges() {
    let index = ColumnValueIndex::build(
        "qty",
        vec![
            (1, Value::Int(10)),
            (2, Value::Int(20)),
            (3, Value::Int(20)),
            (4, Value::Null),
            (5, Value::Int(30)),
        ]
        .into_iter(),
    );
    assert_eq!(
        ids(&index.scan(&Predicate::Equals(Value::Int(20))).unwrap()),
        vec![2, 3]
    );
    assert_eq!(
        ids(&index.scan(&Predicate::GreaterThan(Value::Int(10))).unwrap()),
        vec![2, 3, 5]
    );
    assert_eq!(
        ids(&index
            .scan(&Predicate::Between {
                low: Value::Int(10),
                high: Value::Int(20),
            })
            .unwrap()),
        vec![1, 2, 3]
    );
    assert_eq!(ids(&index.scan(&Predicate::IsNull).unwrap()), vec![4]);
    assert_eq!(
        ids(&index.scan(&Predicate::IsNotNull).unwrap()),
        vec![1, 2, 3, 5]
    );
    assert!(index.scan(&Predicate::NotEquals(Value::Int(10))).is_none());
}

#[test]
fn incremental_insert_remove_tracks_nulls() {
    let mut index = ColumnValueIndex::build("qty", std::iter::empty());
    index.insert(7, &Value::Int(1));
    index.insert(8, &Value::Null);
    assert_eq!(
        ids(&index.scan(&Predicate::Equals(Value::Int(1))).unwrap()),
        vec![7]
    );
    assert_eq!(ids(&index.scan(&Predicate::IsNull).unwrap()), vec![8]);
    index.remove(7, &Value::Int(1));
    index.remove(8, &Value::Null);
    assert!(ids(&index.scan(&Predicate::Equals(Value::Int(1))).unwrap()).is_empty());
    assert!(ids(&index.scan(&Predicate::IsNull).unwrap()).is_empty());
}

#[test]
fn temporal_and_nan_guards_refuse_acceleration() {
    let temporal = uqa_core::TemporalValue::parse_date("2024-01-01").unwrap();
    let index = ColumnValueIndex::build(
        "ts",
        vec![(1, Value::Temporal(temporal.clone()))].into_iter(),
    );
    assert!(index
        .scan(&Predicate::Equals(Value::Str("2024-01-01".into())))
        .is_none());

    let numeric = ColumnValueIndex::build("f", vec![(1, Value::Float(1.0))].into_iter());
    assert!(numeric
        .scan(&Predicate::Equals(Value::Float(f64::NAN)))
        .is_none());
    assert!(numeric
        .scan(&Predicate::Equals(Value::Temporal(temporal)))
        .is_none());
}
