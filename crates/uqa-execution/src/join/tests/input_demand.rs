//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Empty probes preserve `PostgreSQL`'s demand for independent function inputs.

use super::*;

#[test]
fn hash_join_resumes_duplicate_candidates_before_later_residual_errors() {
    for bytes in [1, 64 * 1024] {
        for kind in [
            JoinKind::Inner,
            JoinKind::Left,
            JoinKind::Right,
            JoinKind::Full,
        ] {
            for build_left in [false, true] {
                if build_left && kind != JoinKind::Inner {
                    continue;
                }
                let key = if build_left { "l.k" } else { "r.k" };
                let value = if build_left { "l.x" } else { "r.x" };
                let probe_key = if build_left { "r.k" } else { "l.k" };
                let build = TableScan::from_rows(
                    vec![key.into(), value.into()],
                    [1, 2]
                        .map(|n| row(&[(key, Value::Int(1)), (value, Value::Int(n))]))
                        .into(),
                );
                let calls = Arc::new(AtomicUsize::new(0));
                let observed = Arc::clone(&calls);
                let probe = crate::PhysicalRowIteratorScan::new(
                    RowSchema::new(vec![probe_key.into()]),
                    Box::new((0..2).map(move |_| {
                        observed.fetch_add(1, Ordering::Relaxed);
                        Ok(PhysicalRow::from_values(vec![Value::Int(1)]))
                    })),
                );
                let (left, right): (Box<dyn PhysicalOperator>, Box<dyn PhysicalOperator>) =
                    if build_left {
                        (Box::new(build), Box::new(probe))
                    } else {
                        (Box::new(probe), Box::new(build))
                    };
                let predicate = ScalarExpr::Binary {
                    op: uqa_sql::ast::BinaryOp::Greater,
                    lhs: Box::new(ScalarExpr::Binary {
                        op: uqa_sql::ast::BinaryOp::Divide,
                        lhs: Box::new(ScalarExpr::Literal(Value::Int(10))),
                        rhs: Box::new(ScalarExpr::Binary {
                            op: uqa_sql::ast::BinaryOp::Subtract,
                            lhs: Box::new(ScalarExpr::Literal(Value::Int(2))),
                            rhs: Box::new(ScalarExpr::Column(value.into())),
                        }),
                    }),
                    rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
                };
                let mut join = HashJoin::new_with_work_mem_and_predicate(
                    left,
                    right,
                    kind,
                    vec![ScalarExpr::Column("l.k".into())],
                    vec![ScalarExpr::Column("r.k".into())],
                    Some(predicate),
                    evaluator(),
                    ResultRow::new(),
                    ResultRow::new(),
                    bytes,
                );
                join.build_left = build_left;
                join.open().unwrap();
                assert_eq!(calls.load(Ordering::Relaxed), 0);
                assert_eq!(
                    join.next_with_demand(crate::physical::demand::ONE)
                        .unwrap()
                        .unwrap()
                        .len(),
                    1
                );
                assert_eq!(calls.load(Ordering::Relaxed), 1);
                assert!(join
                    .next_with_demand(crate::physical::demand::ONE)
                    .unwrap_err()
                    .to_string()
                    .contains("division by zero"));
                assert_eq!(calls.load(Ordering::Relaxed), 1);
                join.close().unwrap();
            }
        }
    }
}

#[test]
fn nested_loop_consumes_only_the_requested_prefix_even_when_it_spills() {
    for kind in [
        JoinKind::Cross,
        JoinKind::Inner,
        JoinKind::Left,
        JoinKind::Right,
        JoinKind::Full,
    ] {
        for bytes in [1, 64 * 1024] {
            for fail_right in [false, true] {
                let left_calls = Arc::new(AtomicUsize::new(0));
                let right_calls = Arc::new(AtomicUsize::new(0));
                let scan = |name: &str, calls: Arc<AtomicUsize>, fails: bool| {
                    crate::PhysicalRowIteratorScan::new(
                        RowSchema::new(vec![name.into()]),
                        Box::new((0..2).filter_map(move |index| {
                            if index == 1 && !fails {
                                return None;
                            }
                            calls.fetch_add(1, Ordering::Relaxed);
                            Some(if index == 0 {
                                Ok(PhysicalRow::from_values(vec![Value::Int(1)]))
                            } else {
                                Err(ExecError::Other("unrequested input row".into()))
                            })
                        })),
                    )
                };
                let mut join = NestedLoopJoin::new_with_work_mem(
                    Box::new(scan("l.k", Arc::clone(&left_calls), !fail_right)),
                    Box::new(scan("r.k", Arc::clone(&right_calls), fail_right)),
                    kind,
                    None,
                    evaluator(),
                    ResultRow::new(),
                    ResultRow::new(),
                    bytes,
                );
                join.open().unwrap();
                assert_eq!(left_calls.load(Ordering::Relaxed), 0);
                assert_eq!(right_calls.load(Ordering::Relaxed), 0);
                let batch = join
                    .next_with_demand(crate::physical::demand::ONE)
                    .unwrap()
                    .unwrap();
                assert_eq!(batch.len(), 1, "{kind:?}, {bytes}, {fail_right}");
                assert_eq!(left_calls.load(Ordering::Relaxed), 1);
                assert_eq!(right_calls.load(Ordering::Relaxed), 1);
                assert_eq!(join.right_input_has_spilled(), bytes == 1);
                assert!(join
                    .next_with_demand(crate::physical::demand::ONE)
                    .unwrap_err()
                    .to_string()
                    .contains("unrequested input row"));
                join.close().unwrap();
                assert_eq!(
                    left_calls.load(Ordering::Relaxed),
                    if fail_right { 1 } else { 2 }
                );
                assert_eq!(
                    right_calls.load(Ordering::Relaxed),
                    if fail_right { 2 } else { 1 }
                );
            }
        }
    }
}

#[test]
fn nested_loop_replays_each_consumed_right_prefix_without_repeating_its_effects() {
    for bytes in [1, 64 * 1024] {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let right = crate::PhysicalRowIteratorScan::new(
            RowSchema::new(vec!["r.k".into()]),
            Box::new((1..=3).map(move |value| {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(PhysicalRow::from_values(vec![Value::Int(value)]))
            })),
        );
        let left = TableScan::from_rows(
            vec!["l.k".into()],
            vec![
                row(&[("l.k", Value::Int(1))]),
                row(&[("l.k", Value::Int(2))]),
            ],
        );
        let mut join = NestedLoopJoin::new_with_work_mem(
            Box::new(left),
            Box::new(right),
            JoinKind::Cross,
            None,
            evaluator(),
            ResultRow::new(),
            ResultRow::new(),
            bytes,
        );
        join.open().unwrap();
        let mut actual = Vec::new();
        while let Some(batch) = join.next_with_demand(crate::physical::demand::ONE).unwrap() {
            assert_eq!(batch.len(), 1);
            actual.push(
                batch
                    .schema
                    .view(&batch.rows[0])
                    .get("r.k")
                    .cloned()
                    .unwrap(),
            );
        }
        join.close().unwrap();
        assert_eq!(actual, [1, 2, 3, 1, 2, 3].map(Value::Int));
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }
}

#[test]
fn empty_left_inputs_request_right_rows_only_when_the_join_preserves_them() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/lateral_functions_oracle.expected.json"
    ))
    .unwrap();
    for (kind, name) in [
        (JoinKind::Cross, "cross"),
        (JoinKind::Inner, "inner"),
        (JoinKind::Left, "left"),
        (JoinKind::Right, "right"),
        (JoinKind::Full, "full"),
    ] {
        let id = format!("empty_volatile_{name}");
        let reference = oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == id)
            .unwrap();
        let expected_rows = reference["results"][0]["rows"].as_array().unwrap().len();
        for hash in [false, true] {
            if hash && kind == JoinKind::Cross {
                continue;
            }
            for bytes in [1, 64 * 1024] {
                let requested = Arc::new(AtomicUsize::new(0));
                let calls = Arc::clone(&requested);
                let right = crate::PhysicalRowIteratorScan::new(
                    RowSchema::with_types(
                        vec!["r.k".into()],
                        vec![Some(uqa_sql::ColumnType::Integer)],
                    ),
                    Box::new((1..=2).map(move |value| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        Ok(PhysicalRow::from_values(vec![Value::Int(value)]))
                    })),
                );
                let left = TableScan::from_rows(vec!["l.k".into()], vec![]);
                let mut join: Box<dyn PhysicalOperator> = if hash {
                    let mut join = HashJoin::new_with_work_mem(
                        Box::new(left),
                        Box::new(right),
                        kind,
                        vec![ScalarExpr::Column("l.k".into())],
                        vec![ScalarExpr::Column("r.k".into())],
                        evaluator(),
                        ResultRow::new(),
                        ResultRow::new(),
                        bytes,
                    );
                    // Exercise an empty probe even when a different cardinality estimate could choose it as the build input.
                    join.build_left = false;
                    Box::new(join)
                } else {
                    Box::new(NestedLoopJoin::new_with_work_mem(
                        Box::new(left),
                        Box::new(right),
                        kind,
                        None,
                        evaluator(),
                        ResultRow::new(),
                        ResultRow::new(),
                        bytes,
                    ))
                };
                let (_, rows) = run_to_rows(join.as_mut()).unwrap();
                assert_eq!(
                    rows.len(),
                    expected_rows,
                    "{id}, hash={hash}, bytes={bytes}"
                );
                assert_eq!(
                    requested.load(Ordering::Relaxed),
                    expected_rows,
                    "{id}, hash={hash}, bytes={bytes}"
                );
            }
        }
    }
}
