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
        for array in [false, true] {
            for (left_count, right_count, budget, computed) in [
                (1, 2, 64 * 1024, false),
                (2, 1, 64 * 1024, false),
                (1, 2, 64 * 1024, true),
                (2, 1, 64 * 1024, true),
                (2, 2, 1, false),
                (2, 2, 1, true),
            ] {
                let left = TableScan::from_rows(
                    vec!["l.k".into()],
                    vec![row(&[("l.k", range_field(first, oid, array))]); left_count],
                );
                let right = TableScan::from_rows(
                    vec!["r.k".into()],
                    vec![row(&[("r.k", range_field(equal, oid, array))]); right_count],
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
