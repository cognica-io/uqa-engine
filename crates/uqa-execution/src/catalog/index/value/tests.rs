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
fn equality_bucket_visits_keep_constant_workspace_and_typed_consumer_errors() {
    let count = 16_384;
    let index = ColumnValueIndex::build("key", (0..count).map(|id| (id, Value::Int(7))));
    let mut next = 0;
    let allocated = allocation_counter::measure(|| {
        assert!(index
            .visit_equal_with_catalog(&Value::Int(7), None, &mut |id| {
                assert_eq!(id, next);
                next += 1;
                Ok(())
            })
            .unwrap());
    });
    assert_eq!(next, count);
    assert!(allocated.bytes_max < 4096, "{allocated:?}");
    next = 0;
    let error = index
        .visit_equal_with_catalog(&Value::Int(7), None, &mut |id| {
            next += 1;
            if id == 2 {
                Err(uqa_sql::SQLError::Routine {
                    sqlstate: "53200".into(),
                    message: "candidate consumer exhausted".into(),
                })
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(next, 3);
    assert!(index
        .visit_equal_with_catalog(&Value::Int(8), None, &mut |_| {
            panic!("missing bucket visited a row")
        })
        .unwrap());
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
                .visit_equal_with_catalog(&invalid, None, &mut |_| {
                    panic!("failed SQL comparison delivered a candidate")
                })
                .unwrap_err()
                .sqlstate(),
            Some("42804")
        );
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
    let record = Value::Record(vec![("x".into(), Value::Int(1))].into());
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

#[test]
fn typed_field_probes_preserve_btree_operator_errors() {
    for (ty, value, name) in [
        (uqa_sql::ColumnType::Xid, Value::Int(1), "xid"),
        (uqa_sql::ColumnType::Json, Value::Null, "json"),
    ] {
        let row = Value::Row(
            uqa_core::RowValue::typed(
                vec![value],
                vec![uqa_core::RecordFieldType {
                    collation_oid: None,
                    oid: uqa_sql::catalog::type_metadata::pg_type_oid(&ty)
                        .try_into()
                        .unwrap(),
                    type_modifier: -1,
                }],
            )
            .unwrap(),
        );
        let index = ColumnValueIndex::build("v", [(1, row.clone())].into_iter());
        assert!(index.field_candidates(&row).is_none());
        let error = index
            .visit_equal_with_catalog(&row, None, &mut |_| {
                panic!("missing comparison operator delivered a candidate")
            })
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"));
        assert_eq!(
            error.to_string(),
            format!("could not identify a comparison function for type {name}")
        );
        for predicate in [
            Predicate::Equals(row.clone()),
            Predicate::NotEquals(row.clone()),
            Predicate::InSet([row.clone()].into_iter().collect()),
            Predicate::LessThan(row),
        ] {
            let mut observed = false;
            let error = index
                .scan_observing(&predicate, || {
                    observed = true;
                    Ok(())
                })
                .unwrap_err();
            assert!(observed);
            assert_eq!(error.sqlstate(), Some("42883"));
            assert_eq!(
                error.to_string(),
                format!("could not identify a comparison function for type {name}")
            );
        }
    }
}

#[test]
fn scalar_null_candidates_visit_without_posting_materialization_or_equality_conflation() {
    let count = 16_384;
    let index = ColumnValueIndex::build(
        "key",
        (0..count).map(|id| {
            (
                id,
                if id % 2 == 0 {
                    Value::Null
                } else {
                    Value::Int(7)
                },
            )
        }),
    );
    let mut expected = 0;
    let allocated = allocation_counter::measure(|| {
        assert!(index
            .visit_key_candidates_with_catalog(&Predicate::IsNull, None, &mut |id| {
                assert_eq!(id, expected);
                expected += 2;
                Ok(())
            })
            .unwrap());
    });
    assert_eq!(expected, count);
    assert!(allocated.bytes_max < 4096, "{allocated:?}");
    assert!(index
        .visit_key_candidates_with_catalog(&Predicate::Equals(Value::Null), None, &mut |_| panic!(
            "NULL equality is not IS NULL"
        ))
        .unwrap());
    let mut visits = 0;
    let error = index
        .visit_key_candidates_with_catalog(&Predicate::IsNull, None, &mut |_| {
            visits += 1;
            Err(uqa_sql::SQLError::Routine {
                sqlstate: "22012".into(),
                message: "consumer failure".into(),
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(visits, 1);
}

#[test]
fn null_candidate_eligibility_preserves_row_and_carried_index_fallbacks() {
    for index in [
        ColumnValueIndex::build(
            "key",
            [(1, Value::Row(vec![Value::Null].into()))].into_iter(),
        ),
        ColumnValueIndex::build_carried([(1, Value::Null)].into_iter()),
    ] {
        assert!(!index
            .visit_key_candidates_with_catalog(&Predicate::IsNull, None, &mut |_| panic!(
                "declined null probe emitted candidates"
            ))
            .unwrap());
    }
    let index = ColumnValueIndex::build("key", [(1, Value::Int(7))].into_iter());
    assert!(!index
        .visit_key_candidates_with_catalog(
            &Predicate::GreaterThan(Value::Int(0)),
            None,
            &mut |_| panic!("unsupported candidate predicate")
        )
        .unwrap());
}
