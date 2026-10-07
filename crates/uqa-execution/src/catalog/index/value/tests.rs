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
    assert_eq!(
        ids(&index.scan(&Predicate::Equals(Value::Int(1))).unwrap()).len(),
        0
    );
    assert_eq!(ids(&index.scan(&Predicate::IsNull).unwrap()).len(), 0);
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

#[test]
fn selected_reads_observe_before_empty_or_cached_results_and_propagate_failure() {
    let index = ColumnValueIndex::build("v", [(1, Value::Int(10))].into_iter());
    for target in [10, 99] {
        let mut observed = false;
        let result = index
            .scan_observing(&Predicate::Equals(Value::Int(target)), || {
                observed = true;
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert!(observed);
        assert_eq!(result.len(), usize::from(target == 10));
    }
    let declined = index
        .scan_observing(&Predicate::NotEquals(Value::Int(10)), || {
            panic!("a declined predicate is not an observed index read")
        })
        .unwrap();
    assert!(declined.is_none());
    let error = index
        .scan_observing(&Predicate::Equals(Value::Int(10)), || {
            Err(uqa_sql::SQLError::Routine {
                sqlstate: "40001".into(),
                message: "observation failed".into(),
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("40001"));
    assert_eq!(
        ids(&index.scan(&Predicate::Equals(Value::Int(10))).unwrap()),
        vec![1]
    );
}

#[test]
fn legacy_vector_index_probes_preserve_operator_failures_without_rejecting_single_keys() {
    let invalid = Value::LegacyVector(
        uqa_core::LegacyVectorValue::try_from_array(
            uqa_core::LegacyVectorKind::Oid,
            uqa_core::ArrayValue::with_lower_bounds(vec![], vec![]).unwrap(),
        )
        .unwrap(),
    );
    let valid = uqa_sql::expr::cast_value(&Value::Str("2".into()), "oidvector").unwrap();
    let empty = ColumnValueIndex::build("v", std::iter::empty());
    assert!(empty
        .scan_observing(&Predicate::Equals(invalid.clone()), || Ok(()))
        .unwrap()
        .unwrap()
        .is_empty());
    for stored in [invalid.clone(), valid.clone()] {
        let index = ColumnValueIndex::build("v", [(1, stored)].into_iter());
        assert_eq!(
            index
                .scan_observing(&Predicate::Equals(invalid.clone()), || Ok(()))
                .unwrap_err()
                .sqlstate(),
            Some("42804")
        );
        assert_eq!(
            ids(&index
                .scan_observing(&Predicate::IsNotNull, || Ok(()))
                .unwrap()
                .unwrap()),
            vec![1]
        );
    }
    let index = ColumnValueIndex::build("v", [(1, invalid)].into_iter());
    assert_eq!(
        index
            .scan_observing(&Predicate::Equals(valid), || Ok(()))
            .unwrap_err()
            .sqlstate(),
        Some("42804")
    );
}

#[test]
fn a_carried_column_holds_stored_values_and_answers_no_predicate() {
    let values = vec![(1, Value::Int(10)), (2, Value::Null), (3, Value::Int(10))];
    let mut carried = ColumnValueIndex::build_carried(values.clone().into_iter());
    assert!(carried.is_carried());
    assert_eq!(carried.stored_value(1), Some(&Value::Int(10)));
    assert_eq!(carried.stored_value(2), Some(&Value::Null));
    assert!(carried.contains(3) && !carried.contains(4));
    for predicate in [
        Predicate::Equals(Value::Int(10)),
        Predicate::IsNull,
        Predicate::IsNotNull,
    ] {
        assert!(!carried.supports(&predicate));
        assert!(carried.scan(&predicate).is_none());
        assert!(carried.estimate_cardinality(&predicate).is_none());
        let mut observed = false;
        assert!(carried
            .scan_observing(&predicate, || {
                observed = true;
                Ok(())
            })
            .unwrap()
            .is_none());
        assert!(!observed, "a declined predicate registers no read");
    }
    carried.insert(4, &Value::Int(40));
    carried.insert(1, &Value::Int(11));
    carried.remove(3, &Value::Int(10));
    assert_eq!(carried.stored_value(1), Some(&Value::Int(11)));
    assert_eq!(carried.stored_value(3), None);
    assert_eq!(carried.stored_value(4), Some(&Value::Int(40)));
    carried.clear();
    assert!(!carried.contains(1));
}

#[test]
fn changing_the_use_of_a_column_keeps_its_stored_values() {
    let values = vec![
        (1, Value::Int(10)),
        (2, Value::Null),
        (3, Value::Int(10)),
        (4, Value::Int(5)),
    ];
    let key = ColumnValueIndex::build_carried(values.clone().into_iter()).with_use("qty", false);
    assert!(!key.is_carried());
    assert_eq!(
        ids(&key.scan(&Predicate::Equals(Value::Int(10))).unwrap()),
        vec![1, 3]
    );
    assert_eq!(ids(&key.scan(&Predicate::IsNull).unwrap()), vec![2]);
    assert_eq!(
        ids(&key.scan(&Predicate::LessThan(Value::Int(10))).unwrap()),
        vec![4]
    );
    let carried = key.with_use("qty", true);
    assert!(carried.is_carried());
    for (id, value) in &values {
        assert_eq!(carried.stored_value(*id), Some(value));
    }
    // A column already used as asked is returned as it is.
    assert!(carried.with_use("qty", true).is_carried());
}

#[test]
fn raw_field_eligibility_survives_index_mutation_and_carried_conversion() {
    let scalar_row = Value::Row(vec![Value::Int(1)].into());
    let record = Value::Record(vec![("x".into(), Value::Int(1))]);
    let enumeration = Value::Enum(uqa_core::EnumValue::new(
        10,
        uqa_core::EnumLabelKey::from_bytes(vec![128]).unwrap(),
    ));
    for special in [record, enumeration] {
        for wrap in [
            (|value| value) as fn(Value) -> Value,
            |value| Value::List(vec![value]),
            |value| Value::Row(vec![value].into()),
        ] {
            let special = wrap(special.clone());
            let values = [(1, scalar_row.clone()), (2, special.clone())];
            let mut index = ColumnValueIndex::build_carried(values.clone().into_iter());
            assert!(index.field_candidates(&scalar_row).is_none());
            index = index.with_use("a", false);
            assert!(index.field_candidates(&scalar_row).is_none());
            index.clear();
            index.insert(1, &scalar_row);
            assert_eq!(ids(&index.field_candidates(&scalar_row).unwrap()), [1]);
            assert!(index.field_candidates(&special).is_none());
            index.insert(2, &special);
            assert!(index.field_candidates(&scalar_row).is_none());
        }
    }
}
