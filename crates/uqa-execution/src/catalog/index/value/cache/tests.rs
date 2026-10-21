//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::PostingList;

fn build(values: &[(DocId, Value)], carried: bool, allowance: usize) -> ValueIndexCache {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut builder = ValueIndexBuilder::new("key", carried, allowance).unwrap();
    for (id, value) in values.iter().rev() {
        builder.push(*id, value.clone(), &control).unwrap();
    }
    builder.finish(&control).unwrap()
}

fn ids(result: Option<PostingList>) -> Option<Vec<DocId>> {
    result.map(|posting| posting.entries().iter().map(|entry| entry.doc_id).collect())
}

#[test]
fn retained_index_cache_matches_resident_predicates_cardinality_and_full_identities() {
    let values = [
        (u64::MAX, Value::Int(1)),
        (0, Value::Float(1.0)),
        (9, Value::Null),
        (1, Value::Int(7)),
        (5, Value::Int(9)),
    ];
    let old = ColumnValueIndex::build("key", values.iter().cloned());
    let control = StorageReadControl::with_limit(64 * 1024);
    for allowance in [0, 64 * 1024] {
        let index = build(&values, false, allowance);
        assert_eq!(index.is_spilled(), allowance == 0);
        for predicate in [
            Predicate::Equals(Value::Int(1)),
            Predicate::Equals(Value::Null),
            Predicate::NotEquals(Value::Int(7)),
            Predicate::GreaterThan(Value::Int(1)),
            Predicate::GreaterThanOrEqual(Value::Int(7)),
            Predicate::LessThan(Value::Int(7)),
            Predicate::LessThanOrEqual(Value::Int(7)),
            Predicate::Between {
                low: Value::Int(1),
                high: Value::Int(7),
            },
            Predicate::InSet(
                [Value::Int(1), Value::Float(1.0), Value::Null]
                    .into_iter()
                    .collect(),
            ),
            Predicate::IsNull,
            Predicate::IsNotNull,
        ] {
            assert_eq!(
                ids(index
                    .scan_observing_with_catalog(&predicate, None, &control, || Ok(()))
                    .unwrap()),
                ids(old.scan(&predicate)),
                "{predicate:?}"
            );
            assert_eq!(
                index.estimate_cardinality(&predicate, &control).unwrap(),
                old.estimate_cardinality(&predicate),
                "{predicate:?}"
            );
        }
        let mut seen = Vec::new();
        assert!(index
            .visit_key_candidates_with_catalog(
                &Predicate::Equals(Value::Int(1)),
                None,
                &control,
                &mut |id| {
                    seen.push(id);
                    Ok(())
                },
            )
            .unwrap());
        assert_eq!(seen, [0, u64::MAX]);
        for (id, value) in &values {
            assert!(index
                .stored_value(*id, &control)
                .unwrap()
                .unwrap()
                .has_same_representation(value));
        }
    }
}

#[test]
fn retained_index_cache_preserves_carried_values_and_eligibility_changes() {
    let control = StorageReadControl::with_limit(64 * 1024);
    for value in [
        Value::Row(vec![Value::Null].into()),
        Value::Temporal(uqa_core::TemporalValue::parse_date("2020-01-01").unwrap()),
        Value::Float(f64::NAN),
    ] {
        let values = [(1, value)];
        let index = build(&values, true, 0);
        assert!(index.is_carried());
        assert!(!index.supports(&Predicate::IsNotNull));
        let index = index.with_use("key", false);
        let old = ColumnValueIndex::build("key", values.into_iter());
        for predicate in [
            Predicate::IsNull,
            Predicate::IsNotNull,
            Predicate::Equals(Value::Int(1)),
        ] {
            assert_eq!(index.supports(&predicate), old.supports(&predicate));
            assert_eq!(
                ids(index
                    .scan_observing_with_catalog(&predicate, None, &control, || Ok(()))
                    .unwrap()),
                ids(old.scan(&predicate))
            );
        }
    }
}

#[test]
fn retained_index_cache_spills_growth_and_keeps_prior_snapshots_and_stored_keys() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut index = build(
        &[(1, Value::Str("old evaluated expression".into()))],
        false,
        4096,
    );
    assert!(!index.is_spilled());
    let before = index.clone();
    let wide = Value::Str("x".repeat(16 * 1024));
    index.replace(2, Some(&wide), &control).unwrap();
    assert!(index.is_spilled());
    assert!(!before.contains(2, &control).unwrap());
    let after = index.clone();
    index.replace(1, Some(&Value::Int(4)), &control).unwrap();
    assert_eq!(
        after.stored_value(1, &control).unwrap().as_deref(),
        Some(&Value::Str("old evaluated expression".into()))
    );
    index.replace(2, None, &control).unwrap();
    assert_eq!(
        after.stored_value(2, &control).unwrap().as_deref(),
        Some(&wide)
    );
    index.clear();
    assert!(!index.contains(1, &control).unwrap());
    assert!(before.contains(1, &control).unwrap());
    assert!(after.contains(2, &control).unwrap());
}

#[test]
fn retained_index_cache_observes_before_reads_and_retains_consumer_and_cancellation_errors() {
    let mut index = build(
        &[(3, Value::Int(7)), (1, Value::Int(7)), (2, Value::Int(7))],
        false,
        0,
    );
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut seen = Vec::new();
    let error = index
        .visit_key_candidates_with_catalog(
            &Predicate::Equals(Value::Int(7)),
            None,
            &control,
            &mut |id| {
                seen.push(id);
                Err(SQLError::Routine {
                    sqlstate: "22012".into(),
                    message: "consumer".into(),
                })
            },
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(seen, [1]);
    control.cancellation().cancel();
    let error = index
        .scan_observing_with_catalog(&Predicate::IsNotNull, None, &control, || {
            Err(SQLError::Routine {
                sqlstate: "40001".into(),
                message: "observation".into(),
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("40001"));
    assert_eq!(
        index.replace(1, None, &control).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert!(index
        .contains(1, &StorageReadControl::with_limit(64 * 1024))
        .unwrap());
}

#[test]
fn retained_index_cache_wide_population_keeps_bounded_build_and_lookup_workspace() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut index = None;
    let measured = allocation_counter::measure(|| {
        let mut builder = ValueIndexBuilder::new("key", false, 64 * 1024).unwrap();
        for id in (0..256).rev() {
            builder
                .push(
                    id,
                    Value::Str(format!("{id:04}{}", "x".repeat(8192))),
                    &control,
                )
                .unwrap();
        }
        let built = builder.finish(&control).unwrap();
        assert!(built.is_spilled());
        for id in [0, 127, 255] {
            let key = Value::Str(format!("{id:04}{}", "x".repeat(8192)));
            assert_eq!(
                ids(built.field_candidates(&key, &control).unwrap()),
                Some(vec![id])
            );
        }
        index = Some(built);
    });
    // Sixteen merge heads may each retain source and key payloads; one row is indivisible. This stays below the 2MiB input population.
    assert!(measured.bytes_max <= 1536 * 1024, "{measured:?}");
    assert!(index.unwrap().is_spilled());
}

#[test]
fn retained_index_cache_keeps_identity_order_for_fallible_typed_comparison() {
    let typed = |ty: uqa_sql::ColumnType, value| {
        Value::Row(
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
        )
    };
    let first = typed(uqa_sql::ColumnType::Xid, Value::Int(1));
    let second = typed(uqa_sql::ColumnType::Json, Value::Null);
    let values = [(9, second), (1, first.clone())];
    let control = StorageReadControl::with_limit(64 * 1024);
    for allowance in [0, 64 * 1024] {
        let index = build(&values, false, allowance);
        assert!(index.field_candidates(&first, &control).unwrap().is_none());
        let error = index
            .visit_key_candidates_with_catalog(
                &Predicate::Equals(first.clone()),
                None,
                &control,
                &mut |_| panic!("invalid operator cannot produce a candidate"),
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"));
        assert_eq!(
            error.to_string(),
            "could not identify a comparison function for type xid"
        );
    }
}

#[test]
fn retained_index_cache_rejects_duplicate_identities_without_publishing_an_owner() {
    let control = StorageReadControl::with_limit(64 * 1024);
    for allowance in [0, 64 * 1024] {
        let mut builder = ValueIndexBuilder::new("key", false, allowance).unwrap();
        builder.push(1, Value::Int(2), &control).unwrap();
        builder.push(1, Value::Int(3), &control).unwrap();
        assert!(builder.finish(&control).is_err());
    }
}
