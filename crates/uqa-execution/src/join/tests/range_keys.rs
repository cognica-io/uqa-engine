//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn range_field(text: &str, oid: u32, array: bool) -> Value {
    if array {
        Value::Array(
            uqa_core::ArrayValue::try_new(vec![Value::Str(text.into())])
                .unwrap()
                .with_element_type_oid(Some(oid)),
        )
    } else {
        Value::Row(
            uqa_core::RowValue::typed(
                vec![Value::Str(text.into())],
                vec![uqa_core::RecordFieldType {
                    collation_oid: None,
                    oid,
                    type_modifier: -1,
                }],
            )
            .unwrap(),
        )
    }
}

#[test]
fn range_join_preserves_direct_encoded_and_spilled_multiplicities() {
    for (oid, first, equal) in [
        (3906, "[1.0,3.00)", "[1.00,3.0)"),
        (4532, "{[1.0,2.00),[5,6)}", "{[1.00,2.0),[5.0,6.00)}"),
    ] {
        for shape in 0..3 {
            let ty = (shape == 0).then(|| {
                uqa_sql::catalog::type_metadata::builtin_scalar_type(oid)
                    .unwrap()
                    .clone()
            });
            let value = |text: &str| {
                if shape == 0 {
                    Value::Str(text.into())
                } else {
                    range_field(text, oid, shape == 2)
                }
            };
            for (left_count, right_count, budget, computed) in [
                (1, 2, 64 * 1024, false),
                (2, 1, 64 * 1024, false),
                (1, 2, 64 * 1024, true),
                (2, 1, 64 * 1024, true),
                (2, 2, 1, false),
                (2, 2, 1, true),
            ] {
                let left = TableScan::from_typed_rows(
                    vec!["l.k".into()],
                    vec![ty.clone()],
                    vec![row(&[("l.k", value(first))]); left_count],
                );
                let right = TableScan::from_typed_rows(
                    vec!["r.k".into()],
                    vec![ty.clone()],
                    vec![row(&[("r.k", value(equal))]); right_count],
                );
                let expression = |name: &str| {
                    let key = ScalarExpr::Column(name.into());
                    if computed {
                        ScalarExpr::Case {
                            base: None,
                            when: vec![(ScalarExpr::Literal(Value::Bool(true)), key)],
                            else_branch: None,
                        }
                    } else {
                        key
                    }
                };
                let mut join = HashJoin::new_with_work_mem(
                    Box::new(left),
                    Box::new(right),
                    JoinKind::Inner,
                    vec![expression("l.k")],
                    vec![expression("r.k")],
                    evaluator(),
                    row(&[("l.k", Value::Null)]),
                    row(&[("r.k", Value::Null)]),
                    budget,
                );
                join.open().unwrap();
                assert_eq!(join.builds_left_input(), left_count < right_count);
                if budget != 1 {
                    let state = join
                        .streaming_unique
                        .as_ref()
                        .expect("unique smaller input must select streaming join");
                    assert!(matches!(
                        (&state.hash_index, computed),
                        (super::super::UniqueHashIndex::Direct(_), false)
                            | (super::super::UniqueHashIndex::Encoded(_), true)
                    ));
                }
                let mut count = 0;
                while let Some(batch) = join.next().unwrap() {
                    count += batch.rows.len();
                }
                assert_eq!(count, left_count * right_count);
                assert_eq!(join.hash_index_has_spilled(), budget == 1);
                join.close().unwrap();
            }
        }
    }
}

#[test]
fn spilled_range_full_join_retains_unmatched_and_null_rows() {
    let values = |side, first, other| {
        [
            (1, range_field(first, 3906, false)),
            (2, range_field(first, 3906, false)),
            (3, range_field(other, 3906, false)),
            (4, Value::Null),
        ]
        .into_iter()
        .map(|(id, key)| {
            row(&[
                (side, Value::Int(id)),
                (if side == "l.id" { "l.k" } else { "r.k" }, key),
            ])
        })
        .collect()
    };
    let left = TableScan::from_rows(
        vec!["l.id".into(), "l.k".into()],
        values("l.id", "[1.0,3.00)", "[5,7)"),
    );
    let right = TableScan::from_rows(
        vec!["r.id".into(), "r.k".into()],
        values("r.id", "[1.00,3.0)", "[8,9)"),
    );
    let mut join = HashJoin::new_with_work_mem(
        Box::new(left),
        Box::new(right),
        JoinKind::Full,
        vec![ScalarExpr::Column("l.k".into())],
        vec![ScalarExpr::Column("r.k".into())],
        evaluator(),
        row(&[("l.id", Value::Null), ("l.k", Value::Null)]),
        row(&[("r.id", Value::Null), ("r.k", Value::Null)]),
        1,
    );
    let (_, rows) = run_to_rows(&mut join).unwrap();
    let mut pairs = rows
        .iter()
        .map(|row| (row["l.id"].clone(), row["r.id"].clone()))
        .collect::<Vec<_>>();
    pairs.sort();
    let mut expected = vec![
        (Value::Int(1), Value::Int(1)),
        (Value::Int(1), Value::Int(2)),
        (Value::Int(2), Value::Int(1)),
        (Value::Int(2), Value::Int(2)),
        (Value::Int(3), Value::Null),
        (Value::Int(4), Value::Null),
        (Value::Null, Value::Int(3)),
        (Value::Null, Value::Int(4)),
    ];
    expected.sort();
    assert_eq!(pairs, expected);
    assert!(join.hash_index_has_spilled());
    assert!(join.right_input_has_spilled());
    assert!(join.output_has_spilled());
}

#[test]
fn join_key_types_are_resolved_once_per_open_and_never_during_close() {
    struct CountTypes(AtomicUsize);
    impl crate::ExpressionEvaluator for CountTypes {
        fn evaluate(
            &self,
            expression: &ScalarExpr,
            row: &dyn uqa_sql::expr::RowLookup,
        ) -> ExecResult<Value> {
            TestEvaluator.evaluate(expression, row)
        }
        fn expression_type(
            &self,
            expression: &ScalarExpr,
            schema: &RowSchema,
        ) -> Result<Option<uqa_sql::ColumnType>, uqa_sql::SQLError> {
            if self.0.fetch_add(1, Ordering::Relaxed) >= 2 {
                return Err(uqa_sql::SQLError::Internal(
                    "join repeated its selected type lookup".into(),
                ));
            }
            crate::scalar_type(expression, schema, &[])
        }
    }
    let evaluator = Arc::new(CountTypes(AtomicUsize::new(0)));
    let ty = uqa_sql::ColumnType::from_sql_name("numrange").unwrap();
    let scan = |column: &str, text: &str| {
        Box::new(TableScan::from_typed_rows(
            vec![column.into()],
            vec![Some(ty.clone())],
            vec![row(&[(column, Value::Str(text.into()))])],
        ))
    };
    let mut join = HashJoin::new_with_work_mem(
        scan("l.k", "[1.0,3.00)"),
        scan("r.k", "[1.00,3.0)"),
        JoinKind::Inner,
        vec![ScalarExpr::Column("l.k".into())],
        vec![ScalarExpr::Column("r.k".into())],
        evaluator.clone(),
        row(&[("l.k", Value::Null)]),
        row(&[("r.k", Value::Null)]),
        65536,
    );
    assert_eq!(run_to_rows(&mut join).unwrap().1.len(), 1);
    assert_eq!(evaluator.0.load(Ordering::Relaxed), 2);
}
